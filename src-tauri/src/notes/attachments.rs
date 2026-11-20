use std::{io::Write, path::Path};

use sha2::{Digest, Sha256};
use tauri::{Runtime, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use uebox_notes::{Attachment, AttachmentKind};

use super::{notes_error, parse_id, NotesHost, NOT_FOUND, UNAVAILABLE};

const EXPORT_ERROR: &str = "notes_attachment_export_failed";
const DESTINATION_EXISTS: &str = "notes_attachment_destination_exists";

fn suggested_name(attachment: &Attachment) -> String {
    if attachment.summary.kind == AttachmentKind::Html {
        return format!("note-attachment-{}.txt", attachment.summary.id);
    }
    attachment
        .summary
        .file_name
        .as_deref()
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 200
                && name.trim_matches([' ', '.']) == *name
                && !name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
                && !name.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .unwrap_or_else(|| format!("note-attachment-{}", attachment.summary.id))
}

fn publish(destination: &Path, bytes: &[u8]) -> Result<(), &'static str> {
    let parent = destination
        .parent()
        .filter(|path| destination.is_absolute() && path.is_dir())
        .ok_or(EXPORT_ERROR)?;
    let mut staged = tempfile::Builder::new()
        .prefix(".uebox-note-")
        .tempfile_in(parent)
        .map_err(|_| EXPORT_ERROR)?;
    staged.write_all(bytes).map_err(|_| EXPORT_ERROR)?;
    staged.as_file().sync_all().map_err(|_| EXPORT_ERROR)?;
    staged
        .persist_noclobber(destination)
        .map_err(|error| match error.error.kind() {
            std::io::ErrorKind::AlreadyExists => DESTINATION_EXISTS,
            _ => EXPORT_ERROR,
        })?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn export_note_attachment<R: Runtime>(
    window: WebviewWindow<R>,
    note_id: String,
    attachment_id: String,
) -> Result<bool, &'static str> {
    use tauri::Manager;

    if window.label() != "main" {
        return Err(UNAVAILABLE);
    }
    let note = parse_id(&note_id)?;
    let attachment = parse_id(&attachment_id)?;
    let saved = NotesHost::run(window.app_handle().clone(), false, move |store| {
        let store = store.ok_or(NOT_FOUND)?;
        if !store
            .note_attachments(note)
            .map_err(notes_error)?
            .iter()
            .any(|entry| entry.id == attachment)
        {
            return Err(NOT_FOUND);
        }
        let saved = store
            .get_attachment(attachment)
            .map_err(notes_error)?
            .ok_or(NOT_FOUND)?;
        let hash: [u8; 32] = Sha256::digest(&saved.bytes).into();
        if hash != saved.summary.sha256 {
            return Err("notes_invalid");
        }
        Ok(saved)
    })
    .await?;
    let name = suggested_name(&saved);
    let (reply, mut selection) = tauri::async_runtime::channel(1);
    let dialog = window.dialog().file().set_file_name(name);
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    let dialog = dialog.set_parent(&window);
    dialog.save_file(move |result| {
        let _ = reply.try_send(result);
    });
    let Some(Some(destination)) = selection.recv().await else {
        return Ok(false);
    };
    let path = destination.into_path().map_err(|_| EXPORT_ERROR)?;
    tauri::async_runtime::spawn_blocking(move || publish(&path, &saved.bytes))
        .await
        .map_err(|_| EXPORT_ERROR)??;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_export_is_explicit_and_never_replaces_an_existing_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("note.txt");
        assert!(publish(&path, b"original bytes").is_ok());
        assert_eq!(publish(&path, b"replacement"), Err(DESTINATION_EXISTS));
        assert_eq!(std::fs::read(path).unwrap(), b"original bytes");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
