use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime};
use uebox_notes::{LegacyNote, LegacyNotebook, NoteImportSummary, V2Handoff, V2ImportSummary};

use super::{notes_error, NotesHost, INVALID, UNAVAILABLE};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u32,
    source: String,
    notebooks: Vec<ImportedNotebook>,
    notes: Vec<ImportedNote>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportedNotebook {
    identity: String,
    title: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ImportedNote {
    identity: String,
    notebook_identity: Option<String>,
    title: String,
    content: String,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportReport {
    notebooks_inserted: u32,
    notebooks_existing: u32,
    notes_inserted: u32,
    notes_existing: u32,
    #[serde(skip_serializing_if = "is_zero")]
    attachments_inserted: u32,
    #[serde(skip_serializing_if = "is_zero")]
    original_files_inserted: u32,
    #[serde(skip_serializing_if = "is_zero")]
    original_files_existing: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl From<NoteImportSummary> for ImportReport {
    fn from(result: NoteImportSummary) -> Self {
        Self {
            notebooks_inserted: result.notebooks_inserted,
            notebooks_existing: result.notebooks_existing,
            notes_inserted: result.notes_inserted,
            notes_existing: result.notes_existing,
            ..Self::default()
        }
    }
}

impl From<V2ImportSummary> for ImportReport {
    fn from(result: V2ImportSummary) -> Self {
        Self {
            notebooks_inserted: result.notebooks_inserted,
            notebooks_existing: result.notebooks_existing,
            notes_inserted: result.notes_inserted,
            notes_existing: result.notes_existing,
            attachments_inserted: result.attachments_inserted,
            original_files_inserted: result.original_files_inserted,
            original_files_existing: result.original_files_existing,
        }
    }
}

#[derive(Deserialize)]
struct Version {
    version: u32,
}

enum Handoff {
    Original(Manifest),
    Rich(V2Handoff),
}

fn parse_handoff(manifest: &str) -> Result<Handoff, &'static str> {
    if manifest.len() > 64 * 1024 * 1024 {
        return Err("notes_import_limit");
    }
    let Version { version } = serde_json::from_str(manifest).map_err(|_| INVALID)?;
    match version {
        1 => parse(manifest).map(Handoff::Original),
        2 => {
            let document: V2Handoff = serde_json::from_str(manifest).map_err(|_| INVALID)?;
            if document.format != "uebox-notes" || document.source != "electron-public-notes" {
                return Err(INVALID);
            }
            Ok(Handoff::Rich(document))
        }
        _ => Err("notes_import_version"),
    }
}

fn parse(manifest: &str) -> Result<Manifest, &'static str> {
    const MAX_BYTES: usize = 16 * 1024 * 1024;
    if manifest.len() > MAX_BYTES {
        return Err("notes_import_limit");
    }
    let value: serde_json::Value = serde_json::from_str(manifest).map_err(|_| INVALID)?;
    if !value.is_object()
        || ["notebooks", "notes"].iter().any(|key| {
            !value[*key]
                .as_array()
                .is_some_and(|rows| rows.iter().all(serde_json::Value::is_object))
        })
    {
        return Err(INVALID);
    }
    let document: Manifest = serde_json::from_str(manifest).map_err(|_| INVALID)?;
    if document.format != "uebox-notes" || document.version != 1 {
        return Err("notes_import_version");
    }
    if document.source != "electron-public-notes"
        || document.notebooks.len() > 4096
        || document.notes.len() > 16_384
    {
        return Err(INVALID);
    }
    Ok(document)
}

#[tauri::command]
pub(crate) async fn import_legacy_notes<R: Runtime>(
    app: AppHandle<R>,
    manifest: String,
) -> Result<ImportReport, &'static str> {
    let document = tauri::async_runtime::spawn_blocking(move || parse_handoff(&manifest))
        .await
        .map_err(|_| UNAVAILABLE)??;
    let document = match document {
        Handoff::Original(document) => document,
        Handoff::Rich(document) => {
            if document.notebooks.is_empty()
                && document.notes.is_empty()
                && document.original_files.is_empty()
            {
                return Ok(ImportReport::default());
            }
            return NotesHost::run(app, true, move |store| {
                store
                    .ok_or(UNAVAILABLE)?
                    .import_v2(&document)
                    .map(Into::into)
                    .map_err(notes_error)
            })
            .await;
        }
    };
    if document.notebooks.is_empty() && document.notes.is_empty() {
        return Ok(ImportReport::default());
    }
    NotesHost::run(app, true, move |store| {
        let notebooks: Vec<_> = document
            .notebooks
            .iter()
            .map(|notebook| LegacyNotebook {
                identity: &notebook.identity,
                title: &notebook.title,
            })
            .collect();
        let notes: Vec<_> = document
            .notes
            .iter()
            .map(|note| LegacyNote {
                identity: &note.identity,
                notebook_identity: note.notebook_identity.as_deref(),
                title: &note.title,
                content: &note.content,
            })
            .collect();
        store
            .ok_or(UNAVAILABLE)?
            .import_legacy_batch(&document.source, &notebooks, &notes)
            .map(Into::into)
            .map_err(notes_error)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn manifest_requires_version_one_objects_and_unambiguous_keys() {
        let valid = json!({
            "format": "uebox-notes",
            "version": 1,
            "source": "electron-public-notes",
            "notebooks": [{ "identity": "legacy-book", "title": "Notes" }],
            "notes": [{
                "identity": "source:1",
                "notebookIdentity": "legacy-book",
                "title": "Note",
                "content": "Plain text"
            }]
        });
        let parsed = parse(&valid.to_string()).unwrap();
        assert_eq!(
            parsed.notes[0].notebook_identity.as_deref(),
            Some("legacy-book")
        );

        let duplicate = valid
            .to_string()
            .replace("\"version\":1", "\"version\":1,\"version\":1");
        assert!(matches!(parse(&duplicate), Err(INVALID)));
        let mut unsupported = valid.clone();
        unsupported["version"] = json!(2);
        assert!(matches!(
            parse(&unsupported.to_string()),
            Err("notes_import_version")
        ));
        let mut extraneous = valid.clone();
        extraneous["notes"][0]["filePath"] = json!("/local/secret");
        assert!(matches!(parse(&extraneous.to_string()), Err(INVALID)));
        let mut tuple = valid;
        tuple["notebooks"][0] = json!(["legacy-book", "Notes"]);
        assert!(matches!(parse(&tuple.to_string()), Err(INVALID)));
    }
}
