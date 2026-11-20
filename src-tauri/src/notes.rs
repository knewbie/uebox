//! Offline notebooks and notes in the preview profile.
pub(crate) mod attachments;
pub(crate) mod transfer;

use std::{path::PathBuf, sync::Mutex};

use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime, WebviewWindow};
use uebox_notes::{
    AttachmentSummary, Note, NoteSummary, Notebook, NotesError, NotesStore, OriginalFileSummary,
};

use crate::profile_storage::{
    acquire_lock, checked_file_exists, private_directory, ProfileError, ProfileLock,
};

const UNAVAILABLE: &str = "notes_unavailable";
const INVALID: &str = "notes_invalid";
const NOT_FOUND: &str = "notes_not_found";

#[derive(Default)]
pub(crate) struct NotesHost {
    state: Mutex<Option<OpenNotes>>,
    #[cfg(test)]
    test_data_dir: std::sync::OnceLock<PathBuf>,
}

struct OpenNotes {
    notes: NotesStore,
    _lock: ProfileLock,
}

impl NotesHost {
    fn data_dir<R: Runtime>(&self, app: &AppHandle<R>) -> Result<PathBuf, &'static str> {
        #[cfg(test)]
        if let Some(path) = self.test_data_dir.get() {
            return Ok(path.clone());
        }
        app.path().app_local_data_dir().map_err(|_| UNAVAILABLE)
    }

    async fn run<R, T>(
        app: AppHandle<R>,
        create: bool,
        operation: impl FnOnce(Option<&mut NotesStore>) -> Result<T, &'static str> + Send + 'static,
    ) -> Result<T, &'static str>
    where
        R: Runtime,
        T: Send + 'static,
    {
        tauri::async_runtime::spawn_blocking(move || {
            let host = app.state::<Self>();
            let mut state = host.state.lock().map_err(|_| UNAVAILABLE)?;
            if state.is_none() {
                let root = host.data_dir(&app)?;
                let Some(directory) =
                    private_directory(&root, "notes", create).map_err(storage_error)?
                else {
                    return operation(None);
                };
                let lock = acquire_lock(&directory.join("notes.lock")).map_err(storage_error)?;
                let file = directory.join("notes.sqlite");
                checked_file_exists(&file).map_err(storage_error)?;
                let notes = match NotesStore::open_existing(&file) {
                    Ok(notes) => notes,
                    Err(NotesError::Missing) if create => {
                        NotesStore::create_new(&file).map_err(notes_error)?
                    }
                    Err(NotesError::Missing) => return operation(None),
                    Err(error) => return Err(notes_error(error)),
                };
                *state = Some(OpenNotes { notes, _lock: lock });
            }
            operation(state.as_mut().map(|state| &mut state.notes))
        })
        .await
        .map_err(|_| UNAVAILABLE)?
    }
}

fn storage_error(error: ProfileError) -> &'static str {
    match error {
        ProfileError::Invalid => INVALID,
        ProfileError::Locked => "notes_locked",
        ProfileError::Unavailable => UNAVAILABLE,
    }
}

fn notes_error(error: NotesError) -> &'static str {
    match error {
        NotesError::InvalidInput | NotesError::Unsupported | NotesError::Corrupt => INVALID,
        NotesError::Missing | NotesError::NotebookMissing => NOT_FOUND,
        NotesError::RecoveryRequired => "notes_recovery_required",
        NotesError::Locked => "notes_locked",
        NotesError::ImportConflict => "notes_import_conflict",
        NotesError::LimitExceeded => "notes_import_limit",
        _ => UNAVAILABLE,
    }
}

fn parse_id(id: &str) -> Result<i64, &'static str> {
    id.parse::<i64>()
        .ok()
        .filter(|value| *value > 0 && value.to_string() == id)
        .ok_or(INVALID)
}

fn optional_id(id: Option<String>) -> Result<Option<i64>, &'static str> {
    id.as_deref().map(parse_id).transpose()
}

fn page_limit(limit: u32) -> Result<u32, &'static str> {
    (1..=50).contains(&limit).then(|| limit + 1).ok_or(INVALID)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page<T> {
    items: Vec<T>,
    next_id: Option<String>,
}

impl<T> Page<T> {
    fn from_rows<R>(mut rows: Vec<R>, limit: u32, id: impl Fn(&R) -> i64) -> Self
    where
        T: From<R>,
    {
        let has_more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let next_id = rows
            .last()
            .filter(|_| has_more)
            .map(|row| id(row).to_string());
        Self {
            items: rows.into_iter().map(Into::into).collect(),
            next_id,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct NotebookView {
    id: String,
    title: String,
}

impl From<Notebook> for NotebookView {
    fn from(record: Notebook) -> Self {
        Self {
            id: record.id.to_string(),
            title: record.title,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NoteView {
    id: String,
    notebook_id: Option<String>,
    title: String,
}

impl From<NoteSummary> for NoteView {
    fn from(note: NoteSummary) -> Self {
        Self {
            id: note.id.to_string(),
            notebook_id: note.notebook_id.map(|id| id.to_string()),
            title: note.title,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct NoteDetail {
    #[serde(flatten)]
    summary: NoteView,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<uebox_notes::NoteFormat>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<NoteAttachment>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NoteAttachment {
    id: String,
    kind: uebox_notes::AttachmentKind,
    file_name: Option<String>,
}

impl From<AttachmentSummary> for NoteAttachment {
    fn from(attachment: AttachmentSummary) -> Self {
        Self {
            id: attachment.id.to_string(),
            kind: attachment.kind,
            file_name: attachment.file_name,
        }
    }
}

impl NoteDetail {
    fn load(note: Note, store: &NotesStore) -> Result<Self, &'static str> {
        Ok(Self {
            summary: NoteView {
                id: note.id.to_string(),
                notebook_id: note.notebook_id.map(|id| id.to_string()),
                title: note.title,
            },
            content: note.content,
            format: store.note_format(note.id).map_err(notes_error)?,
            attachments: store
                .note_attachments(note.id)
                .map_err(notes_error)?
                .into_iter()
                .map(Into::into)
                .collect(),
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceView {
    id: String,
    kind: uebox_notes::ElectronSourceKind,
    title: String,
    file_name: Option<String>,
}

impl From<OriginalFileSummary> for SourceView {
    fn from(source: OriginalFileSummary) -> Self {
        Self {
            id: source.id.to_string(),
            kind: source.kind,
            title: source.title,
            file_name: source.file_name,
        }
    }
}

#[tauri::command]
pub(crate) async fn list_notebook_sources<R: Runtime>(
    window: WebviewWindow<R>,
    notebook_id: String,
    after_id: Option<String>,
    limit: u32,
) -> Result<Page<SourceView>, &'static str> {
    if window.label() != "main" {
        return Err(UNAVAILABLE);
    }
    let parent = parse_id(&notebook_id)?;
    let cursor = optional_id(after_id)?;
    let count = page_limit(limit)?;
    NotesHost::run(window.app_handle().clone(), false, move |store| {
        let rows = store
            .ok_or(NOT_FOUND)?
            .list_original_files(parent, cursor, count)
            .map_err(notes_error)?;
        Ok(Page::from_rows(rows, limit, |source| source.id))
    })
    .await
}

#[derive(Serialize)]
pub(crate) struct SourceBody {
    id: String,
    content: String,
}

#[tauri::command]
pub(crate) async fn read_notebook_source<R: Runtime>(
    window: WebviewWindow<R>,
    notebook_id: String,
    source_id: String,
) -> Result<SourceBody, &'static str> {
    if window.label() != "main" {
        return Err(UNAVAILABLE);
    }
    let parent = parse_id(&notebook_id)?;
    let id = parse_id(&source_id)?;
    NotesHost::run(window.app_handle().clone(), false, move |store| {
        let source = store
            .ok_or(NOT_FOUND)?
            .get_original_file(parent, id)
            .map_err(notes_error)?
            .ok_or(NOT_FOUND)?;
        Ok(SourceBody {
            id: source.id.to_string(),
            content: source.content,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn list_notebooks<R: Runtime>(
    app: AppHandle<R>,
    after_id: Option<String>,
    limit: u32,
) -> Result<Page<NotebookView>, &'static str> {
    let cursor = optional_id(after_id)?;
    let count = page_limit(limit)?;
    NotesHost::run(app, false, move |store| {
        let rows = match store {
            Some(store) => store.list_notebooks(cursor, count).map_err(notes_error)?,
            None => Vec::new(),
        };
        Ok(Page::from_rows(rows, limit, |notebook| notebook.id))
    })
    .await
}

#[tauri::command]
pub(crate) async fn list_notes<R: Runtime>(
    app: AppHandle<R>,
    notebook_id: Option<String>,
    after_id: Option<String>,
    limit: u32,
) -> Result<Page<NoteView>, &'static str> {
    let parent = optional_id(notebook_id)?;
    let cursor = optional_id(after_id)?;
    let count = page_limit(limit)?;
    NotesHost::run(app, false, move |store| {
        let rows = match store {
            Some(store) => store
                .list_notes(parent, cursor, count)
                .map_err(notes_error)?,
            None if parent.is_some() => return Err(NOT_FOUND),
            None => Vec::new(),
        };
        Ok(Page::from_rows(rows, limit, |note| note.id))
    })
    .await
}

#[tauri::command]
pub(crate) async fn get_note<R: Runtime>(
    app: AppHandle<R>,
    note_id: String,
) -> Result<NoteDetail, &'static str> {
    let id = parse_id(&note_id)?;
    NotesHost::run(app, false, move |store| {
        let store = store.ok_or(NOT_FOUND)?;
        let note = store.get_note(id).map_err(notes_error)?.ok_or(NOT_FOUND)?;
        NoteDetail::load(note, store)
    })
    .await
}

fn visible_text(value: &str, max_bytes: usize) -> Result<(), &'static str> {
    if value.len() > max_bytes || value.contains('\0') {
        Err(INVALID)
    } else {
        Ok(())
    }
}

#[tauri::command]
pub(crate) async fn save_notebook<R: Runtime>(
    app: AppHandle<R>,
    id: Option<String>,
    title: String,
) -> Result<NotebookView, &'static str> {
    let id = optional_id(id)?;
    visible_text(&title, 1024)?;
    NotesHost::run(app, id.is_none(), move |store| {
        let store = store.ok_or(NOT_FOUND)?;
        let id = if let Some(id) = id {
            if !store.rename_notebook(id, &title).map_err(notes_error)? {
                return Err(NOT_FOUND);
            }
            id
        } else {
            store.create_notebook(&title).map_err(notes_error)?
        };
        Ok(NotebookView {
            id: id.to_string(),
            title,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn save_note<R: Runtime>(
    app: AppHandle<R>,
    id: Option<String>,
    notebook_id: Option<String>,
    title: String,
    content: String,
) -> Result<NoteDetail, &'static str> {
    let id = optional_id(id)?;
    let parent = optional_id(notebook_id)?;
    visible_text(&title, 1024)?;
    visible_text(&content, 4 * 1024 * 1024)?;
    NotesHost::run(app, id.is_none() && parent.is_none(), move |store| {
        let store = store.ok_or(NOT_FOUND)?;
        if let Some(id) = id {
            let old = store.get_note(id).map_err(notes_error)?.ok_or(NOT_FOUND)?;
            if old.notebook_id != parent {
                return Err("notes_parent_changed");
            }
            let mut view = NoteDetail::load(old, store)?;
            if !store
                .update_note(id, &title, &content)
                .map_err(notes_error)?
            {
                return Err(NOT_FOUND);
            }
            view.summary.title = title;
            view.content = content;
            Ok(view)
        } else {
            let id = store
                .create_note(parent, &title, &content)
                .map_err(notes_error)?;
            Ok(NoteDetail {
                summary: NoteView {
                    id: id.to_string(),
                    notebook_id: parent.map(|id| id.to_string()),
                    title,
                },
                content,
                format: None,
                attachments: Vec::new(),
            })
        }
    })
    .await
}

#[tauri::command]
pub(crate) async fn remove_note<R: Runtime>(
    app: AppHandle<R>,
    note_id: String,
) -> Result<bool, &'static str> {
    let id = parse_id(&note_id)?;
    NotesHost::run(app, false, move |store| match store {
        Some(store) => store.delete_note(id).map_err(notes_error),
        None => Ok(false),
    })
    .await
}

#[tauri::command]
pub(crate) async fn remove_notebook<R: Runtime>(
    app: AppHandle<R>,
    notebook_id: String,
) -> Result<bool, &'static str> {
    let id = parse_id(&notebook_id)?;
    NotesHost::run(app, false, move |store| match store {
        Some(store) => store.delete_notebook(id).map_err(notes_error),
        None => Ok(false),
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
    fn notebook_commands_keep_standalone_notes_and_import_retries_consistent() {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path().join("profile");
        let app = crate::configure_commands(mock_builder())
            .build(mock_context(noop_assets()))
            .unwrap();
        app.state::<NotesHost>()
            .test_data_dir
            .set(profile.clone())
            .unwrap();
        let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let invoke = |command: &str, args: Value| {
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
            .map(|body| body.deserialize::<Value>().unwrap())
        };
        let list = |limit, after_id| {
            invoke(
                "list_notebooks",
                json!({"limit": limit, "afterId": after_id}),
            )
        };
        assert_eq!(
            list(2, Value::Null).unwrap(),
            json!({"items": [], "nextId": null})
        );
        assert_eq!(list(u32::MAX, Value::Null).unwrap_err(), INVALID);
        assert!(!profile.exists());

        let notebook =
            |title| invoke("save_notebook", json!({"id": null, "title": title})).unwrap();
        let first = notebook("First");
        let second = notebook("Second");
        let page = list(1, Value::Null).unwrap();
        assert_eq!(page["items"], json!([first]));
        assert_eq!(page["nextId"], first["id"]);
        assert_eq!(
            list(1, page["nextId"].clone()).unwrap()["items"],
            json!([second])
        );
        let raw_text = "# Fixture\n\n<script>literal</script>\n";
        let note = |parent| {
            invoke(
                "save_note",
                json!({
                    "id": null, "notebookId": parent, "title": "Example", "content": raw_text
                }),
            )
            .unwrap()
        };
        let child = note(first["id"].clone());
        let standalone = note(Value::Null);
        let list_children = invoke(
            "list_notes",
            json!({
                "notebookId": first["id"], "afterId": null, "limit": 1
            }),
        )
        .unwrap();
        assert!(list_children["items"][0].get("content").is_none());
        assert_eq!(list_children["items"][0]["id"], child["id"]);
        assert_eq!(
            invoke("save_note", json!({
                "id": child["id"], "notebookId": second["id"], "title": "Stale", "content": "rejected"
            })).unwrap_err(),
            "notes_parent_changed"
        );
        assert_eq!(
            invoke("remove_notebook", json!({"notebookId": first["id"]})).unwrap(),
            true
        );
        assert_eq!(
            invoke("get_note", json!({"noteId": child["id"]})).unwrap_err(),
            NOT_FOUND
        );
        let loaded = invoke("get_note", json!({"noteId": standalone["id"]})).unwrap();
        assert_eq!(loaded["content"], raw_text);
        assert_eq!(loaded["notebookId"], Value::Null);

        let manifest = json!({
            "format": "uebox-notes", "version": 1, "source": "electron-public-notes",
            "notebooks": [{ "identity": "old-book", "title": "Legacy notebook" }],
            "notes": [{
                "identity": "note:1", "notebookIdentity": "old-book",
                "title": "Legacy note", "content": "visible text"
            }]
        });
        let import = |manifest: &Value| {
            invoke(
                "import_legacy_notes",
                json!({ "manifest": manifest.to_string() }),
            )
        };
        assert_eq!(
            import(&manifest).unwrap(),
            json!({
                "notebooksInserted": 1, "notebooksExisting": 0,
                "notesInserted": 1, "notesExisting": 0
            })
        );
        assert_eq!(
            import(&manifest).unwrap(),
            json!({
                "notebooksInserted": 0, "notebooksExisting": 1,
                "notesInserted": 0, "notesExisting": 1
            })
        );
        let mut conflict = manifest;
        conflict["notes"][0]["content"] = json!("different");
        assert_eq!(import(&conflict).unwrap_err(), "notes_import_conflict");

        let rich = json!({
            "format":"uebox-notes", "version":2, "source":"electron-public-notes",
            "notebooks":[{"identity":"sources-fixture","title":"Imported source notebook"}],
            "originalFiles":[{
                "identity":"source-fixture","notebookIdentity":"sources-fixture","kind":"file",
                "title":"Imported source","content":"<script>inert</script>","rawContent":"private raw",
                "fileName":"sample.txt","filePath":"/absolute/sample.txt","sourceUrl":null,"mediaUrl":null
            }],
            "notes":[{
                "identity":"note:rich", "notebookIdentity":null, "title":"Rich fixture",
                "format":"html", "content":"<p><strong>Visible text</strong></p>",
                "attachments":[{
                    "kind":"binary","mediaType":"text/plain","fileName":"fixture.txt","bytes":"YWJj",
                    "sha256":"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                }]
            }]
        });
        let result = import(&rich).unwrap();
        assert_eq!(result["notesInserted"], 1);
        assert_eq!(result["attachmentsInserted"], 1);
        assert_eq!(result["originalFilesInserted"], 1);
        let notebooks = invoke("list_notebooks", json!({"afterId":null,"limit":20})).unwrap();
        let source_book = &notebooks["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|book| book["title"] == "Imported source notebook")
            .unwrap()["id"];
        let sources = invoke(
            "list_notebook_sources",
            json!({"notebookId":source_book,"afterId":null,"limit":20}),
        )
        .unwrap();
        let source_id = &sources["items"][0]["id"];
        assert_eq!(sources["items"][0]["fileName"], "sample.txt");
        assert!(sources["items"][0].get("content").is_none());
        assert!(sources["items"][0].get("filePath").is_none());
        assert_eq!(
            invoke(
                "read_notebook_source",
                json!({"notebookId":source_book,"sourceId":source_id})
            )
            .unwrap(),
            json!({"id":source_id,"content":"<script>inert</script>"})
        );
        assert_eq!(
            invoke(
                "read_notebook_source",
                json!({"notebookId":second["id"],"sourceId":source_id})
            )
            .unwrap_err(),
            "notes_not_found"
        );
        let notes = invoke(
            "list_notes",
            json!({
                "notebookId":null, "afterId":null, "limit":20
            }),
        )
        .unwrap();
        let rich_id = &notes["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|note| note["title"] == "Rich fixture")
            .unwrap()["id"];
        let imported = invoke("get_note", json!({"noteId": rich_id})).unwrap();
        assert_eq!(imported["format"], "html");
        assert_eq!(imported["attachments"][0]["fileName"], "fixture.txt");
        assert!(imported["attachments"][0].get("bytes").is_none());
        let modified = invoke(
            "save_note",
            json!({
                "id":rich_id, "notebookId":null, "title":"Saved fixture","content":"<p>Edited</p>"
            }),
        )
        .unwrap();
        assert_eq!(modified["format"], "html");
        assert_eq!(modified["attachments"], imported["attachments"]);
    }
}
