#include "UAL_NetworkManager.h"

#include "UAL_ProxyDiagnostics.h"
#include "Async/Async.h"
#include "WebSocketsModule.h"
#include "HAL/PlatformProcess.h"
#include "Serialization/JsonWriter.h"
#include "Serialization/JsonSerializer.h"

DEFINE_LOG_CATEGORY_STATIC(LogUALNetwork, Log, All);

FUAL_NetworkManager& FUAL_NetworkManager::Get()
{
	static FUAL_NetworkManager Instance;
	return Instance;
}

void FUAL_NetworkManager::Init(const FString& ServerUrl)
{
	TargetUrl = ServerUrl;
	bWantsReconnect = true;

	// 代理只查一次：这些设置在编辑器活着的时候不会变，而连接失败每 5 秒来一次。
	// 查到了也先不喊 —— 万一这台机器的代理确实放过了回环，喊了就是误报。
	// 真喊是在第一次连接失败的时候（HandleOnConnectionError），那时它才是答案。
	ProxyRemedy = UAL_ProxyDiagnostics::DescribeRemedy(
		UAL_ProxyDiagnostics::DiagnoseCurrentEnvironment(TargetUrl), TargetUrl);

	Connect();
	StartReconnectTimer();
	StartHeartbeatTimer();
}

void FUAL_NetworkManager::Shutdown()
{
	bWantsReconnect = false;
	StopReconnectTimer();
	StopHeartbeatTimer();
	CleanupSocket();
}

bool FUAL_NetworkManager::IsConnected() const
{
	return Socket.IsValid() && Socket->IsConnected();
}

void FUAL_NetworkManager::SendMessage(const FString& JsonData)
{
	FScopeLock Lock(&SendMutex);
	if (IsConnected())
	{
		UE_LOG(LogUALNetwork, Verbose, TEXT("SendMessage: %s"), *JsonData);
		Socket->Send(JsonData);
	}
	else
	{
		UE_LOG(LogUALNetwork, Warning, TEXT("SendMessage skipped: socket not connected"));
	}
}

bool FUAL_NetworkManager::IsSocketEpochCurrent(uint64 Epoch) const
{
	return Epoch != 0 && ActiveSocketEpoch.load() == Epoch;
}

void FUAL_NetworkManager::SendMessageForSocketEpoch(uint64 Epoch, const FString& JsonData)
{
	FScopeLock Lock(&SendMutex);
	if (IsSocketEpochCurrent(Epoch) && IsConnected())
	{
		Socket->Send(JsonData);
	}
}

void FUAL_NetworkManager::Connect()
{
	if (bIsConnecting || TargetUrl.IsEmpty())
	{
		return;
	}

	if (!FModuleManager::Get().IsModuleLoaded("WebSockets"))
	{
		FModuleManager::LoadModuleChecked<FWebSocketsModule>("WebSockets");
	}

	bIsConnecting = true;
	ConnectStartedAt = FPlatformTime::Seconds();
	UE_LOG(LogUALNetwork, Log, TEXT("Connecting to %s"), *TargetUrl);

	const TSharedPtr<IWebSocket> NewSocket = FWebSocketsModule::Get().CreateWebSocket(TargetUrl);
	uint64 Epoch;
	{
		FScopeLock Lock(&SendMutex);
		Epoch = ++NextSocketEpoch;
		Socket = NewSocket;
		ActiveSocketEpoch.store(Epoch);
		BindSocketEvents(Epoch);
	}
	NewSocket->Connect();
}

void FUAL_NetworkManager::BindSocketEvents(uint64 Epoch)
{
	if (!Socket.IsValid())
	{
		return;
	}

	ConnectedHandle = Socket->OnConnected().AddLambda([this, Epoch]()
	{
		if (IsSocketEpochCurrent(Epoch))
		{
			HandleOnConnected();
		}
	});
	ErrorHandle = Socket->OnConnectionError().AddLambda([this, Epoch](const FString& Error)
	{
		uint64 Expected = Epoch;
		if (ActiveSocketEpoch.compare_exchange_strong(Expected, 0))
		{
			HandleOnConnectionError(Epoch, Error);
		}
	});
	ClosedHandle = Socket->OnClosed().AddLambda([this, Epoch](int32 Code, const FString& Reason, bool bWasClean)
	{
		uint64 Expected = Epoch;
		if (ActiveSocketEpoch.compare_exchange_strong(Expected, 0))
		{
			HandleOnClosed(Epoch, Code, Reason, bWasClean);
		}
	});
	MessageHandle = Socket->OnMessage().AddLambda([this, Epoch](const FString& Data)
	{
		if (IsSocketEpochCurrent(Epoch))
		{
			MessageReceivedDelegate.Broadcast(Data, Epoch);
		}
	});
}

void FUAL_NetworkManager::CleanupSocket(uint64 ExpectedEpoch)
{
	TSharedPtr<IWebSocket> OldSocket;
	FDelegateHandle OldConnected, OldError, OldClosed, OldMessage;
	{
		FScopeLock Lock(&SendMutex);
		if (ExpectedEpoch != 0 && ExpectedEpoch != NextSocketEpoch)
		{
			return;
		}
		ActiveSocketEpoch.store(0);
		OldSocket = MoveTemp(Socket);
		OldConnected = ConnectedHandle;
		OldError = ErrorHandle;
		OldClosed = ClosedHandle;
		OldMessage = MessageHandle;
	}
	if (OldSocket.IsValid())
	{
		OldSocket->OnConnected().Remove(OldConnected);
		OldSocket->OnConnectionError().Remove(OldError);
		OldSocket->OnClosed().Remove(OldClosed);
		OldSocket->OnMessage().Remove(OldMessage);
		OldSocket->Close();
	}
	bIsConnecting = false;
}

void FUAL_NetworkManager::StartReconnectTimer()
{
	if (ReconnectTickerHandle.IsValid())
	{
		return;
	}

	ReconnectTickerHandle = UAL_CORE_TICKER.AddTicker(FTickerDelegateType::CreateRaw(this, &FUAL_NetworkManager::TickReconnect), 5.0f);
}

void FUAL_NetworkManager::StopReconnectTimer()
{
	if (ReconnectTickerHandle.IsValid())
	{
		UAL_CORE_TICKER.RemoveTicker(ReconnectTickerHandle);
		ReconnectTickerHandle.Reset();
	}
}

bool FUAL_NetworkManager::TickReconnect(float DeltaTime)
{
	if (!bWantsReconnect)
	{
		return false; // 停止 ticker
	}

	// `bIsConnecting` 卡住的话重连会**永久停摆**。
	//
	// 真机上遇到过：盒子重启后 UE 再也连不回来，日志里只有一行
	// "Connecting to ws://127.0.0.1:17860"，之后既没有 Connected、
	// 也没有 Connection error、也没有 Socket closed —— 那次连接尝试
	// 没有任何回调落地，于是 bIsConnecting 一直是 true，下面的判断
	// 永远进不去，用户只能重启编辑器。
	//
	// 不去追究 WebSockets 层为什么不回调（那依赖平台实现），直接给
	// 「连接中」加一个上限：超时就当它失败，清掉重来。
	if (bIsConnecting)
	{
		const double Elapsed = FPlatformTime::Seconds() - ConnectStartedAt;
		if (Elapsed > ConnectTimeoutSeconds)
		{
			UE_LOG(LogUALNetwork, Warning,
				TEXT("Connect attempt stuck for %.1fs with no callback; forcing retry"), Elapsed);
			LogProxyRemedyOnce(); // 没有任何回调也可能是代理把连接吞了
			CleanupSocket(); // 内部会把 bIsConnecting 置回 false
		}
	}

	if (!IsConnected() && !bIsConnecting)
	{
		UE_LOG(LogUALNetwork, Verbose, TEXT("Reconnect ticker triggering connect"));
		Connect();
	}
	return true; // 继续运行
}

void FUAL_NetworkManager::StartHeartbeatTimer()
{
	if (HeartbeatTickerHandle.IsValid())
	{
		return;
	}

	// 每10秒发送一次心跳（前端配置是15秒检查间隔，45秒超时，所以10秒发送一次足够）
	HeartbeatTickerHandle = UAL_CORE_TICKER.AddTicker(FTickerDelegateType::CreateRaw(this, &FUAL_NetworkManager::TickHeartbeat), 10.0f);
}

void FUAL_NetworkManager::StopHeartbeatTimer()
{
	if (HeartbeatTickerHandle.IsValid())
	{
		UAL_CORE_TICKER.RemoveTicker(HeartbeatTickerHandle);
		HeartbeatTickerHandle.Reset();
	}
}

bool FUAL_NetworkManager::TickHeartbeat(float DeltaTime)
{
	if (!bWantsReconnect || !IsConnected())
	{
		return false; // 停止 ticker
	}

	// 发送心跳消息
	TSharedPtr<FJsonObject> Root = MakeShared<FJsonObject>();
	Root->SetStringField(TEXT("ver"), TEXT("1.0"));
	Root->SetStringField(TEXT("type"), TEXT("evt"));
	Root->SetStringField(TEXT("method"), TEXT("system.heartbeat"));
	
	// payload可以为空，或者包含一些基本信息
	TSharedPtr<FJsonObject> Payload = MakeShared<FJsonObject>();
	Root->SetObjectField(TEXT("payload"), Payload);

	FString OutJson;
	const TSharedRef<TJsonWriter<>> Writer = TJsonWriterFactory<>::Create(&OutJson);
	FJsonSerializer::Serialize(Root.ToSharedRef(), Writer);

	SendMessage(OutJson);

	return true; // 继续运行
}

void FUAL_NetworkManager::HandleOnConnected()
{
	UE_LOG(LogUALNetwork, Display, TEXT("Connected to %s"), *TargetUrl);
	bIsConnecting = false;
	bBroadcastedConnected = true;
	ConnectedDelegate.Broadcast();
}

void FUAL_NetworkManager::HandleOnClosed(uint64 Epoch, int32 StatusCode, const FString& Reason, bool bWasClean)
{
	UE_LOG(LogUALNetwork, Warning, TEXT("Socket closed (%d): %s Clean=%d"), StatusCode, *Reason, bWasClean);
	bIsConnecting = false;
	BroadcastDisconnectedOnce();
	if (bWantsReconnect)
	{
		CleanupSocket(Epoch);
	}
}

void FUAL_NetworkManager::HandleOnConnectionError(uint64 Epoch, const FString& Error)
{
	UE_LOG(LogUALNetwork, Error, TEXT("Connection error: %s"), *Error);
	LogProxyRemedyOnce();
	bIsConnecting = false;
	BroadcastDisconnectedOnce();
	if (bWantsReconnect)
	{
		CleanupSocket(Epoch);
	}
}

void FUAL_NetworkManager::LogProxyRemedyOnce()
{
	// 只播一次。连接失败每 5 秒来一次，这段话不短，跟着刷会把日志冲烂 ——
	// 而它每次的内容都一模一样，刷一百遍也不多一个字的信息。
	if (ProxyRemedy.IsEmpty() || bLoggedProxyRemedy)
	{
		return;
	}
	bLoggedProxyRemedy = true;
	UE_LOG(LogUALNetwork, Error, TEXT("%s"), *ProxyRemedy);
}

void FUAL_NetworkManager::BroadcastDisconnectedOnce()
{
	// 盒子不在时重连 ticker 每隔几秒就会失败一次。不去重的话「断开」会被播成
	// 一条稳定的心跳，订阅方（比如清资产锁角标）跟着被反复叫醒做无用功
	if (!bBroadcastedConnected)
	{
		return;
	}
	bBroadcastedConnected = false;
	DisconnectedDelegate.Broadcast();
}

