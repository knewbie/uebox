use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};
use uebox_chat_history::{
    ActorLabelReceipt, Chat, ChatHistory, ChatSummary, Engine, HistoryError, Turn, TurnState,
};

use crate::profile_storage::{
    acquire_lock, checked_file_exists, private_directory, ProfileError, ProfileLock,
};

const MAX_PAGE: u32 = 50;
const HISTORY_UNAVAILABLE: &str = "chat_history_unavailable";

#[derive(Clone, Default)]
pub(crate) struct ChatHistoryHost {
    storage: Arc<Mutex<Option<OpenHistory>>>,
    #[cfg(test)]
    test_data_dir: Arc<std::sync::OnceLock<PathBuf>>,
}

struct OpenHistory {
    database: ChatHistory,
    _lock: ProfileLock,
}

fn lock_chat_history(
    root: &Path,
    create: bool,
) -> Result<Option<(PathBuf, ProfileLock)>, &'static str> {
    let map = |error| match error {
        ProfileError::Invalid => "chat_history_invalid",
        ProfileError::Locked => "chat_history_locked",
        ProfileError::Unavailable => HISTORY_UNAVAILABLE,
    };
    let Some(directory) = private_directory(root, "chat-history", create).map_err(map)? else {
        return Ok(None);
    };
    let lock = acquire_lock(&directory.join("history.lock")).map_err(map)?;
    let database = directory.join("history.sqlite");
    checked_file_exists(&database).map_err(map)?;
    Ok(Some((database, lock)))
}

impl ChatHistoryHost {
    fn with_database<T>(
        &self,
        root: PathBuf,
        create: bool,
        read_or_write: impl FnOnce(Option<&mut ChatHistory>) -> Result<T, HistoryError>,
    ) -> Result<T, &'static str> {
        let mut storage = self.storage.lock().map_err(|_| HISTORY_UNAVAILABLE)?;
        if storage.is_none() {
            let Some((path, lock)) = lock_chat_history(&root, create)? else {
                return read_or_write(None).map_err(history_error);
            };
            let database = match ChatHistory::open_existing_recovering(&path) {
                Ok(mut database) => {
                    database.recover_interrupted().map_err(history_error)?;
                    database
                }
                Err(HistoryError::Missing) if create => {
                    ChatHistory::create_new(&path).map_err(history_error)?
                }
                Err(HistoryError::Missing) => return read_or_write(None).map_err(history_error),
                Err(error) => return Err(history_error(error)),
            };
            *storage = Some(OpenHistory {
                database,
                _lock: lock,
            });
        }
        read_or_write(storage.as_mut().map(|storage| &mut storage.database)).map_err(history_error)
    }

    fn data_dir<R: Runtime>(&self, app: &AppHandle<R>) -> Result<PathBuf, &'static str> {
        #[cfg(test)]
        if let Some(root) = self.test_data_dir.get() {
            return Ok(root.clone());
        }
        app.path()
            .app_local_data_dir()
            .map_err(|_| HISTORY_UNAVAILABLE)
    }

    async fn read<R, T>(
        app: AppHandle<R>,
        operation: impl FnOnce(Option<&mut ChatHistory>) -> Result<T, HistoryError> + Send + 'static,
    ) -> Result<T, &'static str>
    where
        R: Runtime,
        T: Send + 'static,
    {
        let history = app.state::<Self>().inner().clone();
        let root = history.data_dir(&app)?;
        tauri::async_runtime::spawn_blocking(move || history.with_database(root, false, operation))
            .await
            .map_err(|_| HISTORY_UNAVAILABLE)?
    }
}

fn history_error(error: HistoryError) -> &'static str {
    match error {
        HistoryError::InvalidInput => "chat_history_invalid_input",
        HistoryError::ChatNotFound => "chat_history_not_found",
        HistoryError::ActiveTurn => "chat_history_active_turn",
        HistoryError::RecoveryRequired => "chat_history_recovery_required",
        HistoryError::Busy => "chat_history_locked",
        HistoryError::Unsupported | HistoryError::Corrupt => "chat_history_invalid",
        _ => HISTORY_UNAVAILABLE,
    }
}

#[cfg(any(feature = "unreal", feature = "unity", feature = "godot"))]
mod agent_runs {
    use super::*;
    use std::borrow::Cow;
    use uebox_agent_core::{AgentSession, AssistantMessage, Error, Run};
    use uebox_chat_history::{FailureCode, TerminalTurn, TurnId};

    #[derive(Clone)]
    pub(crate) struct RecordedTurn {
        host: ChatHistoryHost,
        root: PathBuf,
        id: TurnId,
    }

    pub(crate) fn start_recorded_run<R: Runtime>(
        app: &AppHandle<R>,
        session: &AgentSession,
        agent_id: &str,
        engine: Engine,
        text: String,
        save_history: bool,
        map_error: impl FnOnce(Error) -> &'static str,
    ) -> Result<(Run, Option<RecordedTurn>), &'static str> {
        if session.has_active_run() {
            return Err("agent_busy");
        }
        let recording = if save_history {
            let host = app.state::<ChatHistoryHost>().inner().clone();
            let root = host.data_dir(app)?;
            let id = host.with_database(root.clone(), true, |database| {
                database
                    .ok_or(HistoryError::Missing)?
                    .begin_chat_turn(agent_id, engine, &text)
            })?;
            Some(RecordedTurn { host, root, id })
        } else {
            None
        };
        let run = match session.start(text) {
            Ok(run) => run,
            Err(error) => {
                if let Some(recording) = recording {
                    recording.finish(Err(error.clone()))?;
                }
                return Err(map_error(error));
            }
        };
        Ok((run, recording))
    }

    impl RecordedTurn {
        #[cfg(feature = "unreal")]
        pub(crate) fn record_actor_label(
            &self,
            approval_id: &str,
            status: &str,
            actual_label: Option<&str>,
        ) -> Result<(), &'static str> {
            use uebox_chat_history::ActorLabelStatus;
            let status = match status {
                "applied" => ActorLabelStatus::Applied,
                "mismatch" => ActorLabelStatus::Mismatch,
                "not_executed" => ActorLabelStatus::NotExecuted,
                "unknown" => ActorLabelStatus::Unknown,
                _ => return Err("chat_history_invalid_input"),
            };
            self.host
                .with_database(self.root.clone(), false, |database| {
                    database
                        .ok_or(HistoryError::Missing)?
                        .record_actor_label_receipt(self.id, approval_id, status, actual_label)
                })
        }

        fn finish(self, outcome: Result<AssistantMessage, Error>) -> Result<(), &'static str> {
            let answer = outcome.as_ref().ok().map(|answer| {
                if answer.text.contains('\0') {
                    Cow::Owned(answer.text.replace('\0', ""))
                } else {
                    Cow::Borrowed(answer.text.as_str())
                }
            });
            let terminal = match &outcome {
                Ok(_) => TerminalTurn::Completed {
                    assistant_text: answer.as_deref().unwrap_or_default(),
                },
                Err(Error::Cancelled) => TerminalTurn::Cancelled,
                Err(error) => TerminalTurn::Failed {
                    code: match error {
                        Error::Network | Error::HttpStatus | Error::CredentialUnavailable => {
                            FailureCode::ModelUnavailable
                        }
                        Error::Provider | Error::Protocol => FailureCode::ModelResponse,
                        Error::OutputLimit | Error::StepLimit | Error::ContextLimit => {
                            FailureCode::LimitReached
                        }
                        Error::TargetChanged => FailureCode::TargetUnavailable,
                        _ => FailureCode::Internal,
                    },
                },
            };
            self.host.with_database(self.root, false, |database| {
                database
                    .ok_or(HistoryError::Missing)?
                    .finish_turn(self.id, terminal)
            })
        }
    }

    pub(crate) async fn finish_recorded_run(
        recording: Option<RecordedTurn>,
        outcome: Result<AssistantMessage, Error>,
        map_error: impl FnOnce(Error) -> &'static str,
    ) -> Result<(), &'static str> {
        let result = outcome.as_ref().map(|_| ()).map_err(Clone::clone);
        if let Some(recording) = recording {
            tauri::async_runtime::spawn_blocking(move || recording.finish(outcome))
                .await
                .map_err(|_| HISTORY_UNAVAILABLE)??;
        }
        result.map_err(map_error)
    }
}

#[cfg(feature = "unreal")]
pub(crate) use agent_runs::RecordedTurn;
#[cfg(any(feature = "unreal", feature = "unity", feature = "godot"))]
pub(crate) use agent_runs::{finish_recorded_run, start_recorded_run};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Summary {
    id: String,
    engine: &'static str,
    title: String,
    created_at: i64,
    updated_at: i64,
}

impl From<ChatSummary> for Summary {
    fn from(summary: ChatSummary) -> Self {
        Self {
            id: summary.id,
            engine: match summary.engine {
                Engine::Unreal => "unreal",
                Engine::Unity => "unity",
                Engine::Godot => "godot",
            },
            title: summary.title,
            created_at: summary.created_at_ms / 1000,
            updated_at: summary.updated_at_ms / 1000,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistoryTurn {
    id: String,
    user_text: String,
    assistant_text: Option<String>,
    status: &'static str,
    failure_code: Option<&'static str>,
    created_at: i64,
    finished_at: Option<i64>,
    actor_label_receipts: Vec<HistoryReceipt>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryReceipt {
    approval_id: String,
    status: &'static str,
    actual_label: Option<String>,
}

impl From<ActorLabelReceipt> for HistoryReceipt {
    fn from(receipt: ActorLabelReceipt) -> Self {
        Self {
            approval_id: receipt.approval_id,
            status: receipt.status.as_str(),
            actual_label: receipt.actual_label,
        }
    }
}

impl From<Turn> for HistoryTurn {
    fn from(turn: Turn) -> Self {
        let (status, assistant_text, failure_code) = match turn.state {
            TurnState::Active => ("pending", None, None),
            TurnState::Completed { assistant_text } => ("completed", Some(assistant_text), None),
            TurnState::Failed { code } => ("failed", None, Some(code.as_str())),
            TurnState::Cancelled => ("cancelled", None, None),
            TurnState::Interrupted => ("interrupted", None, None),
        };
        Self {
            id: turn.id.to_string(),
            user_text: turn.user_text,
            assistant_text,
            status,
            failure_code,
            created_at: turn.started_at_ms / 1000,
            finished_at: turn.finished_at_ms.map(|value| value / 1000),
            actor_label_receipts: turn
                .actor_label_receipts
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Conversation {
    #[serde(flatten)]
    summary: Summary,
    turns: Vec<HistoryTurn>,
}

impl From<Chat> for Conversation {
    fn from(chat: Chat) -> Self {
        Self {
            summary: chat.summary.into(),
            turns: chat.turns.into_iter().map(HistoryTurn::from).collect(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page {
    chats: Vec<Summary>,
    next_offset: Option<u64>,
}

#[tauri::command]
pub(crate) async fn list_agent_chats<R: Runtime>(
    app: AppHandle<R>,
    limit: u32,
    offset: u64,
) -> Result<Page, &'static str> {
    if !(1..=MAX_PAGE).contains(&limit) || offset > u64::from(u32::MAX) {
        return Err("chat_history_invalid_input");
    }
    ChatHistoryHost::read(app, move |database| {
        let mut rows = match database {
            Some(database) => database.list_chats(limit + 1, offset)?,
            None => Vec::new(),
        };
        let next_offset = (rows.len() > limit as usize).then(|| offset + u64::from(limit));
        rows.truncate(limit as usize);
        Ok(Page {
            chats: rows.into_iter().map(Summary::from).collect(),
            next_offset,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn get_agent_chat<R: Runtime>(
    app: AppHandle<R>,
    chat_id: String,
) -> Result<Conversation, &'static str> {
    ChatHistoryHost::read(app, move |database| {
        let database = database.ok_or(HistoryError::ChatNotFound)?;
        database
            .get_chat(&chat_id)?
            .map(Conversation::from)
            .ok_or(HistoryError::ChatNotFound)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
    use uebox_chat_history::TerminalTurn;

    #[test]
    fn history_keeps_the_profile_locked_and_recovers_pending_turns_after_reopen() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile");
        let host = ChatHistoryHost::default();
        host.with_database(path.clone(), true, |db| {
            db.unwrap()
                .begin_chat_turn("previous", Engine::Godot, "Interrupted question")
        })
        .unwrap();
        let second = ChatHistoryHost::default();
        assert!(matches!(
            second.with_database(path.clone(), false, |_| Ok(())),
            Err("chat_history_locked")
        ));
        host.with_database(path.clone(), false, |db| {
            let chat = db.unwrap().get_chat("previous")?.unwrap();
            assert_eq!(chat.turns[0].state, TurnState::Active);
            Ok(())
        })
        .unwrap();
        drop(host);
        second
            .with_database(path, false, |db| {
                let chat = db.unwrap().get_chat("previous")?.unwrap();
                assert_eq!(chat.turns[0].state, TurnState::Interrupted);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn ipc_list_is_lazy_and_detail_contains_only_visible_history() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile");
        let app = crate::configure_commands(mock_builder())
            .build(mock_context(noop_assets()))
            .unwrap();
        let host = app.state::<ChatHistoryHost>();
        host.test_data_dir.set(path.clone()).unwrap();
        let window = tauri::WebviewWindowBuilder::new(&app, "history-test", Default::default())
            .build()
            .unwrap();
        let invoke = |command: &str, args: serde_json::Value| {
            get_ipc_response(
                &window,
                tauri::webview::InvokeRequest {
                    cmd: command.into(),
                    callback: CallbackFn(0),
                    error: CallbackFn(1),
                    url: if cfg!(windows) {
                        "http://tauri.localhost"
                    } else {
                        "tauri://localhost"
                    }
                    .parse()
                    .unwrap(),
                    body: InvokeBody::Json(args),
                    headers: Default::default(),
                    invoke_key: INVOKE_KEY.into(),
                },
            )
            .map(|body| body.deserialize::<serde_json::Value>().unwrap())
        };
        let page_args = serde_json::json!({"limit": 1, "offset": 0});
        assert_eq!(
            invoke("list_agent_chats", page_args.clone()).unwrap()["chats"],
            serde_json::json!([])
        );
        assert!(!path.exists());
        host.with_database(path, true, |db| {
            let db = db.unwrap();
            let turn = db.begin_chat_turn("unity-view", Engine::Unity, "Inspect the scene")?;
            db.finish_turn(
                turn,
                TerminalTurn::Completed {
                    assistant_text: "The scene is ready.",
                },
            )?;
            db.begin_chat_turn("unreal-view", Engine::Unreal, "Show project details")?;
            Ok(())
        })
        .unwrap();
        let page = invoke("list_agent_chats", page_args).unwrap();
        assert_eq!(page["chats"].as_array().unwrap().len(), 1);
        assert_eq!(page["nextOffset"], 1);
        let chat = invoke(
            "get_agent_chat",
            serde_json::json!({"chatId": "unity-view"}),
        )
        .unwrap();
        assert_eq!(chat["title"], "Inspect the scene");
        assert_eq!(chat["engine"], "unity");
        assert_eq!(chat["turns"][0]["assistantText"], "The scene is ready.");
        assert_eq!(chat["turns"][0]["status"], "completed");
        assert!(chat["turns"][0]["id"].is_string());
        assert_eq!(
            invoke("get_agent_chat", serde_json::json!({"chatId": "absent"})).unwrap_err(),
            serde_json::json!("chat_history_not_found")
        );
    }

    #[cfg(feature = "unreal")]
    #[tokio::test]
    async fn agent_records_a_cancelled_turn_and_its_late_receipt_independently() {
        use uebox_agent_core::{
            AgentSession, Engine as AgentEngine, Error, OpenAiProvider, ProviderConfig, RunTarget,
            ToolRegistry,
        };

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile");
        let app = crate::configure_commands(mock_builder())
            .build(mock_context(noop_assets()))
            .unwrap();
        let host = app.state::<ChatHistoryHost>();
        host.test_data_dir.set(path.clone()).unwrap();
        let target = RunTarget::new(AgentEngine::Unreal, "fixture", "isolated-editor", 1).unwrap();
        let provider = OpenAiProvider::new(ProviderConfig::new(
            "http://127.0.0.1:9/v1/chat/completions",
            "fixture-model",
        ))
        .unwrap();
        let session =
            AgentSession::new(provider, ToolRegistry::new(target), Arc::new(|_| true), 3).unwrap();
        let (run, recording) = start_recorded_run(
            app.handle(),
            &session,
            "test-agent",
            Engine::Unreal,
            "Rename the selected actor".into(),
            true,
            |_| "agent_error",
        )
        .unwrap();
        assert!(matches!(
            start_recorded_run(
                app.handle(),
                &session,
                "test-agent",
                Engine::Unreal,
                "Duplicate".into(),
                true,
                |_| "agent_error",
            ),
            Err("agent_busy")
        ));
        let launched = recording.unwrap();
        let book = Arc::new(crate::unreal_approval::ApprovalBook::default());
        let _registration = book.record_run("test-agent".into(), run.id(), launched.clone());
        let run_id = run.id();
        run.cancellation().cancel();
        let outcome = run
            .execute(|_| panic!("cancelled before model network call"))
            .await;
        assert_eq!(outcome.unwrap_err(), Error::Cancelled);
        assert_eq!(
            finish_recorded_run(Some(launched), Err(Error::Cancelled), |_| "cancelled").await,
            Err("cancelled")
        );
        book.record(
            "main".into(),
            crate::unreal_approval::Receipt {
                approval_id: "4".into(),
                agent_id: "test-agent".into(),
                run_id,
                status: "applied",
                actual_label: Some("New label".into()),
                history_error: None,
            },
        );
        assert!(book.receipts("main")[0].history_error.is_none());
        host.with_database(path, false, |db| {
            let chat = db.unwrap().get_chat("test-agent")?.unwrap();
            assert_eq!(chat.turns.len(), 1);
            assert_eq!(chat.turns[0].state, TurnState::Cancelled);
            assert_eq!(chat.turns[0].user_text, "Rename the selected actor");
            assert_eq!(chat.turns[0].actor_label_receipts.len(), 1);
            Ok(())
        })
        .unwrap();
    }
}
