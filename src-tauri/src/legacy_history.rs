//! Explicit import of the Electron application's portable visible-history archive.
use std::{path::PathBuf, sync::Mutex};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};
use uebox_legacy_history::{
    AttachmentMetadata, ChatDetail, ChatMessage, ChatSummary, EmbeddedAttachment, HistoryError,
    ImportSummary, LegacyHistory, ManifestSource,
};

use crate::profile_storage::{
    acquire_lock, checked_file_exists, private_directory, ProfileError, ProfileLock,
};

const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_PAGE_SIZE: u32 = 50;
const UNAVAILABLE: &str = "legacy_history_unavailable";
const INVALID: &str = "legacy_history_invalid";
const NOT_FOUND: &str = "legacy_history_not_found";

#[derive(Default)]
pub(crate) struct LegacyHistoryHost {
    history: Mutex<Option<OpenArchive>>,
    #[cfg(test)]
    test_root: std::sync::OnceLock<PathBuf>,
}

struct OpenArchive {
    archive: LegacyHistory,
    _lock: ProfileLock,
}

impl LegacyHistoryHost {
    fn root<R: Runtime>(&self, app: &AppHandle<R>) -> Result<PathBuf, &'static str> {
        #[cfg(test)]
        if let Some(root) = self.test_root.get() {
            return Ok(root.clone());
        }
        app.path().app_local_data_dir().map_err(|_| UNAVAILABLE)
    }

    async fn run<R, T>(
        app: AppHandle<R>,
        create: bool,
        operation: impl FnOnce(Option<&mut LegacyHistory>) -> Result<T, HistoryError> + Send + 'static,
    ) -> Result<T, &'static str>
    where
        R: Runtime,
        T: Send + 'static,
    {
        tauri::async_runtime::spawn_blocking(move || {
            let host = app.state::<Self>();
            let root = host.root(&app)?;
            let mut history = host.history.lock().map_err(|_| UNAVAILABLE)?;
            if history.is_none() {
                let Some(directory) = private_directory(&root, "legacy-chat-history", create)
                    .map_err(storage_error)?
                else {
                    return operation(None).map_err(history_error);
                };
                let lock = acquire_lock(&directory.join("archive.lock")).map_err(storage_error)?;
                let file = directory.join("archive.sqlite");
                if !checked_file_exists(&file).map_err(storage_error)? && !create {
                    return operation(None).map_err(history_error);
                }
                let archive = LegacyHistory::new(file).map_err(history_error)?;
                *history = Some(OpenArchive {
                    archive,
                    _lock: lock,
                });
            }
            operation(history.as_mut().map(|open| &mut open.archive)).map_err(history_error)
        })
        .await
        .map_err(|_| UNAVAILABLE)?
    }
}

fn storage_error(error: ProfileError) -> &'static str {
    match error {
        ProfileError::Invalid => INVALID,
        ProfileError::Locked => "legacy_history_locked",
        ProfileError::Unavailable => UNAVAILABLE,
    }
}

fn history_error(error: HistoryError) -> &'static str {
    match error {
        HistoryError::InvalidInput
        | HistoryError::InvalidManifest
        | HistoryError::Corrupt
        | HistoryError::Unsupported
        | HistoryError::Conflict => INVALID,
        HistoryError::LimitExceeded => "legacy_history_limit",
        HistoryError::Missing => NOT_FOUND,
        HistoryError::RecoveryRequired => "legacy_history_recovery_required",
        HistoryError::Busy => "legacy_history_locked",
        _ => UNAVAILABLE,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportReport {
    inserted: u32,
    already_imported: u32,
    embedded_attachments: u32,
    unavailable_attachments: u32,
}

impl From<ImportSummary> for ImportReport {
    fn from(imported: ImportSummary) -> Self {
        Self {
            inserted: imported.inserted,
            already_imported: imported.already_present,
            embedded_attachments: imported.embedded_attachments,
            unavailable_attachments: imported.unavailable_attachments,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Summary {
    id: String,
    title: String,
    created_at: i64,
    updated_at: i64,
}

impl From<ChatSummary> for Summary {
    fn from(summary: ChatSummary) -> Self {
        Self {
            id: summary.id,
            title: summary.title,
            created_at: summary.created_at / 1000,
            updated_at: summary.updated_at / 1000,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Attachment {
    kind: &'static str,
    availability: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_type: Option<&'static str>,
}

impl From<AttachmentMetadata> for Attachment {
    fn from(attachment: AttachmentMetadata) -> Self {
        Self {
            kind: attachment.kind.as_str(),
            availability: attachment.availability.as_str(),
            id: attachment.id,
            media_type: attachment.media_type.map(|media_type| media_type.as_str()),
        }
    }
}

#[derive(Serialize)]
struct Message {
    role: &'static str,
    text: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<Attachment>,
}

impl From<ChatMessage> for Message {
    fn from(message: ChatMessage) -> Self {
        Self {
            role: message.role.as_str(),
            text: message.text,
            status: message.status.as_str(),
            attachments: message.attachments.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct Chat {
    #[serde(flatten)]
    summary: Summary,
    messages: Vec<Message>,
}

impl From<ChatDetail> for Chat {
    fn from(detail: ChatDetail) -> Self {
        Self {
            summary: detail.summary.into(),
            messages: detail.messages.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Image {
    media_type: &'static str,
    base64: String,
}

impl From<EmbeddedAttachment> for Image {
    fn from(attachment: EmbeddedAttachment) -> Self {
        Self {
            media_type: attachment.media_type.as_str(),
            base64: STANDARD.encode(attachment.bytes),
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
pub(crate) async fn import_legacy_chat_history<R: Runtime>(
    app: AppHandle<R>,
    manifest: String,
) -> Result<ImportReport, &'static str> {
    if manifest.len() > MAX_MANIFEST_BYTES {
        return Err("legacy_history_limit");
    }
    LegacyHistoryHost::run(app, true, move |archive| {
        archive
            .ok_or(HistoryError::Missing)?
            .import_manifest(ManifestSource::Bytes(manifest.as_bytes()))
            .map(Into::into)
    })
    .await
}

#[tauri::command]
pub(crate) async fn list_legacy_chats<R: Runtime>(
    app: AppHandle<R>,
    limit: u32,
    offset: u64,
) -> Result<Page, &'static str> {
    if !(1..=MAX_PAGE_SIZE).contains(&limit) || offset > u64::from(u32::MAX) {
        return Err(INVALID);
    }
    LegacyHistoryHost::run(app, false, move |archive| {
        let mut chats = match archive {
            Some(archive) => archive.list_chats(limit + 1, offset)?,
            None => Vec::new(),
        };
        let next_offset = (chats.len() > limit as usize).then(|| offset + u64::from(limit));
        chats.truncate(limit as usize);
        Ok(Page {
            chats: chats.into_iter().map(Into::into).collect(),
            next_offset,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn get_legacy_chat<R: Runtime>(
    app: AppHandle<R>,
    chat_id: String,
) -> Result<Chat, &'static str> {
    LegacyHistoryHost::run(app, false, move |archive| {
        archive
            .ok_or(HistoryError::Missing)?
            .get_chat(&chat_id)?
            .map(Into::into)
            .ok_or(HistoryError::Missing)
    })
    .await
}

#[tauri::command]
pub(crate) async fn read_legacy_chat_attachment<R: Runtime>(
    app: AppHandle<R>,
    attachment_id: String,
) -> Result<Image, &'static str> {
    LegacyHistoryHost::run(app, false, move |archive| {
        archive
            .ok_or(HistoryError::Missing)?
            .get_attachment(&attachment_id)?
            .map(Into::into)
            .ok_or(HistoryError::Missing)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};

    #[test]
    fn portable_ipc_import_is_atomic_and_image_reads_are_explicit() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("profile");
        let app = crate::configure_commands(mock_builder())
            .build(mock_context(noop_assets()))
            .unwrap();
        app.state::<LegacyHistoryHost>()
            .test_root
            .set(root.clone())
            .unwrap();
        let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let invoke = |command: &str, body: Value| {
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
                    body: InvokeBody::Json(body),
                    headers: Default::default(),
                    invoke_key: INVOKE_KEY.into(),
                },
            )
            .map(|body| body.deserialize::<Value>().unwrap())
        };
        let pagination = json!({"limit": 10, "offset": 0});
        assert_eq!(
            invoke("list_legacy_chats", pagination.clone()).unwrap()["chats"],
            json!([])
        );
        assert!(!root.exists());
        let image = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+X2ioAAAAASUVORK5CYII=";
        let manifest = json!({
            "format": "uebox-visible-chat-history",
            "version": 1,
            "chats": [{
                "id": "legacy-test",
                "title": "Visible chat",
                "createdAt": 123000,
                "updatedAt": 124000,
                "messages": [{
                    "role": "user",
                    "text": "Look at this",
                    "status": "completed",
                    "attachments": [
                        {"kind": "image", "availability": "embedded", "mediaType": "image/png",
                         "sha256": "d2a766f2cbe30658248edcb1b2d5265f9cf6f793c9f40fdec80ea383236eac6e", "base64": image},
                        {"kind": "audio", "availability": "unavailable"}
                    ]
                }]
            }]
        });
        let import = |manifest: &Value| {
            invoke(
                "import_legacy_chat_history",
                json!({"manifest": manifest.to_string()}),
            )
        };
        let mut tampered = manifest.clone();
        tampered["chats"][0]["messages"][0]["attachments"][0]["sha256"] = json!("0".repeat(64));
        assert!(import(&tampered).is_err());
        assert_eq!(
            invoke("list_legacy_chats", pagination.clone()).unwrap()["chats"],
            json!([])
        );
        assert_eq!(
            import(&manifest).unwrap(),
            json!({
                "inserted": 1, "alreadyImported": 0, "embeddedAttachments": 1, "unavailableAttachments": 1
            })
        );
        assert_eq!(import(&manifest).unwrap()["alreadyImported"], 1);
        let entry = &invoke("list_legacy_chats", pagination).unwrap()["chats"][0];
        assert_eq!(entry["createdAt"], 123);
        let chat = invoke("get_legacy_chat", json!({"chatId": entry["id"]})).unwrap();
        let photo = &chat["messages"][0]["attachments"][0];
        assert_eq!(photo["mediaType"], "image/png");
        assert!(photo.get("base64").is_none());
        let payload = invoke(
            "read_legacy_chat_attachment",
            json!({"attachmentId": photo["id"]}),
        )
        .unwrap();
        assert_eq!(payload, json!({"mediaType": "image/png", "base64": image}));
        assert_eq!(
            chat["messages"][0]["attachments"][1],
            json!({"kind": "audio", "availability": "unavailable"})
        );
    }
}
