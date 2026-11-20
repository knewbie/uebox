#include "UAL_ActorCommands.h"

#include "UAL_CommandUtils.h"
#include "UAL_NetworkManager.h"
#include "Serialization/JsonSerializer.h"

#if WITH_EDITOR
#include "Editor.h"
#include "Engine/Level.h"
#include "Engine/Selection.h"
#include "Engine/World.h"
#include "GameFramework/Actor.h"
#include "HAL/PlatformTime.h"
#include "Misc/App.h"
#include "Misc/Paths.h"
#include "UAL_CommandScope.h"
#include "UAL_ScopedTransaction.h"
#endif

namespace
{
	void SendActorLabelReply(uint64 SocketEpoch, const FString& RequestId, int32 Code,
		const TCHAR* Status, const FGuid* Ticket = nullptr, const TCHAR* Reason = nullptr,
		const FString* ActorPath = nullptr, const TCHAR* LabelField = nullptr, const FString* Label = nullptr)
	{
		if (RequestId.IsEmpty())
		{
			return;
		}
		const TSharedRef<FJsonObject> Result = MakeShared<FJsonObject>();
		Result->SetStringField(TEXT("protocol"), TEXT("actor_label_v1"));
		Result->SetStringField(TEXT("status"), Status);
		if (Ticket)
		{
			Result->SetStringField(TEXT("ticket"), Ticket->ToString(EGuidFormats::Digits).ToLower());
		}
		if (Reason)
		{
			Result->SetStringField(TEXT("reason"), Reason);
		}
		if (ActorPath && LabelField && Label)
		{
			Result->SetStringField(TEXT("actor_path"), *ActorPath);
			Result->SetStringField(LabelField, *Label);
		}

		const TSharedRef<FJsonObject> Frame = MakeShared<FJsonObject>();
		Frame->SetStringField(TEXT("ver"), TEXT("1.0"));
		Frame->SetStringField(TEXT("type"), TEXT("res"));
		Frame->SetStringField(TEXT("id"), RequestId);
		Frame->SetNumberField(TEXT("code"), Code);
		Frame->SetObjectField(TEXT("result"), Result);
		FString Output;
		FJsonSerializer::Serialize(Frame, TJsonWriterFactory<>::Create(&Output));
		FUAL_NetworkManager::Get().SendMessageForSocketEpoch(SocketEpoch, Output);
	}

#if WITH_EDITOR
	struct FActorLabelPreview
	{
		uint64 SocketEpoch = 0;
		double ExpiresAt = 0;
		FString ProjectName;
		FString ProjectDirectory;
		TWeakObjectPtr<UWorld> World;
		TWeakObjectPtr<ULevel> Level;
		TWeakObjectPtr<AActor> Actor;
		FString ActorPath;
		FString OldLabel;
		FString NewLabel;
	};

	// Accessed only by the game-thread dispatcher. Entries never keep UE objects alive.
	TMap<FGuid, FActorLabelPreview> ActorLabelPreviews;
	constexpr int32 MaxActorLabelPreviews = 64;
	constexpr double ActorLabelPreviewLifetime = 120.0;

	UWorld* GetLabelEditorWorld()
	{
		UWorld* World = GEditor ? GEditor->GetEditorWorldContext().World() : nullptr;
		return World && World->WorldType == EWorldType::Editor ? World : nullptr;
	}

	AActor* GetSingleLabelActor(UWorld* World)
	{
		if (!World || !GEditor || !GEditor->GetSelectedActors() || GEditor->GetSelectedActors()->Num() != 1)
		{
			return nullptr;
		}
		FSelectionIterator Selection = GEditor->GetSelectedActorIterator();
		AActor* Actor = Selection ? Cast<AActor>(*Selection) : nullptr;
		return IsValid(Actor) && Actor->GetWorld() == World && Actor->GetLevel()
			&& Actor->GetLevel() == World->GetCurrentLevel()
			&& Actor->HasAnyFlags(RF_Transactional) && !Actor->HasAnyFlags(RF_Transient | RF_ClassDefaultObject)
			? Actor : nullptr;
	}

	bool IsValidRequestedLabel(const FString& Label)
	{
		if (Label.Len() > 128 || Label.TrimStartAndEnd().IsEmpty())
		{
			return false;
		}
		for (TCHAR Character : Label)
		{
			if (Character < 0x20 || (Character >= 0x7f && Character <= 0x9f))
			{
				return false;
			}
		}
		return true;
	}
#endif
}

void FUAL_ActorCommands::Handle_PrepareActorLabel(const TSharedPtr<FJsonObject>& Payload,
	const FString& RequestId, uint64 SocketEpoch)
{
	check(IsInGameThread());
	if (RequestId.IsEmpty() || !FUAL_NetworkManager::Get().IsSocketEpochCurrent(SocketEpoch))
	{
		return;
	}
#if WITH_EDITOR
	FString NewLabel;
	if (!Payload.IsValid() || Payload->Values.Num() != 1
		|| !Payload->TryGetStringField(TEXT("new_label"), NewLabel) || !IsValidRequestedLabel(NewLabel))
	{
		SendActorLabelReply(SocketEpoch, RequestId, 400, TEXT("not_executed"), nullptr, TEXT("invalid_request"));
		return;
	}
	if (UAL_CommandUtils::IsPlayInProgress())
	{
		SendActorLabelReply(SocketEpoch, RequestId, 409, TEXT("not_executed"), nullptr, TEXT("play_in_progress"));
		return;
	}
	UWorld* World = GetLabelEditorWorld();
	AActor* Actor = GetSingleLabelActor(World);
	if (!Actor)
	{
		SendActorLabelReply(SocketEpoch, RequestId, 409, TEXT("not_executed"), nullptr, TEXT("actor_changed"));
		return;
	}
	const double Now = FPlatformTime::Seconds();
	for (auto It = ActorLabelPreviews.CreateIterator(); It; ++It)
	{
		if (It.Value().SocketEpoch != SocketEpoch || It.Value().ExpiresAt <= Now || !It.Value().Actor.IsValid())
		{
			It.RemoveCurrent();
		}
	}
	if (ActorLabelPreviews.Num() >= MaxActorLabelPreviews)
	{
		SendActorLabelReply(SocketEpoch, RequestId, 409, TEXT("not_executed"), nullptr, TEXT("preview_limit"));
		return;
	}

	FGuid Ticket;
	do
	{
		Ticket = FGuid::NewGuid();
	} while (!Ticket.IsValid() || ActorLabelPreviews.Contains(Ticket));

	FActorLabelPreview& Preview = ActorLabelPreviews.Add(Ticket);
	Preview.SocketEpoch = SocketEpoch;
	Preview.ExpiresAt = Now + ActorLabelPreviewLifetime;
	Preview.ProjectName = FApp::GetProjectName();
	Preview.ProjectDirectory = FPaths::ConvertRelativePathToFull(FPaths::ProjectDir());
	Preview.World = World;
	Preview.Level = Actor->GetLevel();
	Preview.Actor = Actor;
	Preview.ActorPath = Actor->GetPathName();
	Preview.OldLabel = Actor->GetActorLabel(/*bCreateIfNone=*/false);
	Preview.NewLabel = MoveTemp(NewLabel);
	SendActorLabelReply(SocketEpoch, RequestId, 200, TEXT("prepared"), &Ticket, nullptr,
		&Preview.ActorPath, TEXT("old_label"), &Preview.OldLabel);
#else
	SendActorLabelReply(SocketEpoch, RequestId, 409, TEXT("not_executed"), nullptr, TEXT("transaction_unavailable"));
#endif
}

void FUAL_ActorCommands::Handle_CommitActorLabel(const TSharedPtr<FJsonObject>& Payload,
	const FString& RequestId, uint64 SocketEpoch)
{
	check(IsInGameThread());
	FUAL_NetworkManager& Network = FUAL_NetworkManager::Get();
	if (RequestId.IsEmpty() || !Network.IsSocketEpochCurrent(SocketEpoch))
	{
		return;
	}

	const TSharedPtr<FJsonObject>* Guard = nullptr;
	FString TicketText;
	FGuid Ticket;
	if (!Payload.IsValid() || !Payload->TryGetObjectField(TEXT("guarded_actor_label"), Guard)
		|| !Guard || !Guard->IsValid() || !(*Guard)->TryGetStringField(TEXT("ticket"), TicketText)
		|| !FGuid::ParseExact(TicketText, EGuidFormats::Digits, Ticket) || !Ticket.IsValid())
	{
		SendActorLabelReply(SocketEpoch, RequestId, 400, TEXT("not_executed"), nullptr, TEXT("invalid_request"));
		return;
	}
	const auto Reject = [&](const TCHAR* Reason)
	{
		SendActorLabelReply(SocketEpoch, RequestId, 200, TEXT("not_executed"), &Ticket, Reason);
	};

#if WITH_EDITOR
	// Remove before validation or mutation. A duplicate frame cannot enter the setter again.
	FActorLabelPreview Preview;
	if (!ActorLabelPreviews.RemoveAndCopyValue(Ticket, Preview) || Preview.ExpiresAt <= FPlatformTime::Seconds())
	{
		Reject(TEXT("expired_or_consumed"));
		return;
	}
	if (Payload->Values.Num() != 1 || (*Guard)->Values.Num() != 1)
	{
		Reject(TEXT("invalid_request"));
		return;
	}
	if (Preview.SocketEpoch != SocketEpoch
		|| !Preview.ProjectName.Equals(FApp::GetProjectName(), ESearchCase::CaseSensitive)
		|| !Preview.ProjectDirectory.Equals(FPaths::ConvertRelativePathToFull(FPaths::ProjectDir()), ESearchCase::CaseSensitive))
	{
		Reject(TEXT("context_changed"));
		return;
	}
	if (UAL_CommandUtils::IsPlayInProgress())
	{
		Reject(TEXT("play_in_progress"));
		return;
	}
	UWorld* World = GetLabelEditorWorld();
	if (!World || Preview.World.Get() != World || Preview.Level.Get() != World->GetCurrentLevel())
	{
		Reject(TEXT("context_changed"));
		return;
	}
	AActor* Actor = Preview.Actor.Get();
	if (!Actor || GetSingleLabelActor(World) != Actor
		|| !Actor->GetPathName().Equals(Preview.ActorPath, ESearchCase::CaseSensitive))
	{
		Reject(TEXT("actor_changed"));
		return;
	}
	if (!Actor->GetActorLabel(/*bCreateIfNone=*/false).Equals(Preview.OldLabel, ESearchCase::CaseSensitive))
	{
		Reject(TEXT("label_changed"));
		return;
	}
	if (!Preview.NewLabel.Equals(Preview.OldLabel, ESearchCase::CaseSensitive))
	{
		FUAL_CommandScope TouchScope;
		FUAL_ScopedTransaction Transaction(NSLOCTEXT("UALActor", "SetApprovedActorLabel", "Set Actor Label"));
		if (!Transaction.IsOutstanding() || !Actor->Modify(false) || !Network.IsSocketEpochCurrent(SocketEpoch))
		{
			Transaction.Cancel();
			Reject(TEXT("transaction_unavailable"));
			return;
		}
		Actor->SetActorLabel(Preview.NewLabel);
	}

	// The transaction and command scope have ended. Never serialize the request
	// label as the actual result. A callback may have invalidated this object.
	Actor = Preview.Actor.Get();
	if (!Actor || Actor->GetWorld() != World || Actor->GetLevel() != Preview.Level.Get()
		|| !Actor->GetPathName().Equals(Preview.ActorPath, ESearchCase::CaseSensitive))
	{
		SendActorLabelReply(SocketEpoch, RequestId, 200, TEXT("outcome_unknown"), &Ticket);
		return;
	}
	const FString ActualLabel = Actor->GetActorLabel(/*bCreateIfNone=*/false);
	const FString ActorPath = Actor->GetPathName();
	SendActorLabelReply(SocketEpoch, RequestId, 200,
		ActualLabel.Equals(Preview.NewLabel, ESearchCase::CaseSensitive) ? TEXT("applied") : TEXT("readback_mismatch"),
		&Ticket, nullptr, &ActorPath, TEXT("actual_label"), &ActualLabel);
#else
	Reject(TEXT("transaction_unavailable"));
#endif
}
