use std::{
    io::{Read, Write},
    path::Path,
    sync::Arc,
    time::Duration,
};

use reqwest::{blocking::Client, redirect::Policy, Url};
use tauri::{Runtime, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

use super::{approved_url, authorize, model::BrowserHost, Session, INVALID_SESSION};

const LIMIT: u64 = 128 * 1024 * 1024;
const FAILED: &str = "browser_download_failed";
const SIZE: &str = "browser_download_size";
const DESTINATION: &str = "browser_download_destination";
const ORIGIN: &str = "browser_download_origin";

fn selected(session: &Session, address: &str) -> Result<Url, &'static str> {
    let url = approved_url(address.trim()).map_err(|_| ORIGIN)?;
    session.accepts(&url).then_some(url).ok_or(ORIGIN)
}

fn suggested_name(url: &Url) -> &str {
    url.path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 120
                && name.trim_matches([' ', '.']) == *name
                && !name.contains(['\\', ':', '*', '?', '"', '<', '>', '|'])
        })
        .unwrap_or("download")
}

fn save(
    session: &Session,
    mut source: impl Read,
    destination: &Path,
    limit: u64,
) -> Result<(), &'static str> {
    let parent = destination
        .parent()
        .filter(|path| destination.is_absolute() && path.is_dir())
        .ok_or(DESTINATION)?;
    let mut file = tempfile::Builder::new()
        .prefix(".uebox-browser-")
        .tempfile_in(parent)
        .map_err(|_| DESTINATION)?;
    let mut copied = 0_u64;
    let mut chunk = [0; 64 * 1024];
    loop {
        if !session.is_live() {
            return Err(INVALID_SESSION);
        }
        let count = source.read(&mut chunk).map_err(|_| FAILED)?;
        if count == 0 {
            break;
        }
        copied += count as u64;
        if copied > limit {
            return Err(SIZE);
        }
        file.write_all(&chunk[..count]).map_err(|_| DESTINATION)?;
    }
    if !session.is_live() {
        return Err(INVALID_SESSION);
    }
    file.as_file().sync_all().map_err(|_| DESTINATION)?;
    if !session.is_live() {
        return Err(INVALID_SESSION);
    }
    file.persist_noclobber(destination)
        .map_err(|_| DESTINATION)?;
    Ok(())
}

fn download(session: Arc<Session>, url: Url, destination: &Path) -> Result<(), &'static str> {
    if !session.accepts(&url) {
        return Err(INVALID_SESSION);
    }
    let client = Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| FAILED)?;
    let response = client.get(url).send().map_err(|_| FAILED)?;
    if !response.status().is_success() {
        return Err(FAILED);
    }
    if response.content_length().is_some_and(|bytes| bytes > LIMIT) {
        return Err(SIZE);
    }
    save(&session, response, destination, LIMIT)
}

#[tauri::command]
pub(crate) async fn save_browser_link<R: Runtime>(
    window: WebviewWindow<R>,
    host: State<'_, BrowserHost>,
    session_id: String,
    address: String,
) -> Result<bool, &'static str> {
    authorize(&window)?;
    let session = host.matching(&session_id)?;
    let _active = session
        .download
        .try_lock()
        .map_err(|_| "browser_download_busy")?;
    let url = selected(&session, &address)?;
    let (reply, mut selection) = tauri::async_runtime::channel(1);
    let dialog = window.dialog().file().set_file_name(suggested_name(&url));
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    let dialog = dialog.set_parent(&window);
    dialog.save_file(move |result| {
        let _ = reply.try_send(result);
    });
    let Some(Some(destination)) = selection.recv().await else {
        return Ok(false);
    };
    let path = destination.into_path().map_err(|_| DESTINATION)?;
    tauri::async_runtime::spawn_blocking({
        let session = Arc::clone(&session);
        move || download(session, url, &path)
    })
    .await
    .map_err(|_| FAILED)??;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_download_requires_the_current_exact_origin() {
        let browser = BrowserHost::default();
        let session = browser
            .reserve(approved_url("https://example.test/browser").unwrap())
            .unwrap();
        let ok = selected(&session, "https://example.test/docs/demo.txt?q=fixture").unwrap();
        assert_eq!(suggested_name(&ok), "demo.txt");
        for forbidden in [
            "http://example.test/docs/demo.txt",
            "https://elsewhere.test/docs/demo.txt",
            "https://example.test:444/docs/demo.txt",
            "https://fixture:private@example.test/docs/demo.txt",
        ] {
            assert_eq!(selected(&session, forbidden), Err(ORIGIN));
        }
        browser.clear(session.id).unwrap();
        assert_eq!(selected(&session, ok.as_str()), Err(ORIGIN));
    }

    #[test]
    fn download_publish_never_replaces_a_file_or_keeps_partial_content() {
        let browser = BrowserHost::default();
        let session = browser
            .reserve(approved_url("https://example.test").unwrap())
            .unwrap();
        let target = tempfile::tempdir().unwrap();
        let path = target.path().join("doc.txt");
        assert_eq!(save(&session, b"oversized".as_slice(), &path, 3), Err(SIZE));
        assert!(!path.exists());
        assert_eq!(save(&session, b"abc".as_slice(), &path, 3), Ok(()));
        assert_eq!(
            save(&session, b"def".as_slice(), &path, 3),
            Err(DESTINATION)
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
        browser.clear(session.id).unwrap();
        assert_eq!(
            save(
                &session,
                b"abc".as_slice(),
                &target.path().join("other.txt"),
                3
            ),
            Err(INVALID_SESSION)
        );
        assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), 1);
    }
}
