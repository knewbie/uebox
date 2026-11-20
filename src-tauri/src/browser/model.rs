use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use url::Url;

pub(super) const UNAVAILABLE: &str = "browser_unavailable";
pub(super) const INVALID_SESSION: &str = "browser_invalid_session";

pub(super) fn approved_url(value: &str) -> Result<Url, &'static str> {
    const INVALID: &str = "browser_invalid_url";
    if value.len() > 2048 || value.chars().any(char::is_control) {
        return Err(INVALID);
    }
    let url = Url::parse(value).map_err(|_| INVALID)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(INVALID);
    }
    Ok(url)
}

#[derive(Clone, Default)]
pub(super) struct Flags {
    pub opening: bool,
    pub loading: bool,
    pub page_started: bool,
    pub navigation_blocked: bool,
    pub download_blocked: bool,
    current_load: Option<Url>,
}

pub(super) struct Session {
    pub id: u64,
    pub initial: Url,
    live: AtomicBool,
    flags: Mutex<Flags>,
}

impl Session {
    pub(super) fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }

    pub(super) fn flags(&self) -> Result<Flags, &'static str> {
        self.flags
            .lock()
            .map(|flags| flags.clone())
            .map_err(|_| UNAVAILABLE)
    }

    pub(super) fn opened(&self) {
        if let Ok(mut flags) = self.flags.lock() {
            flags.opening = false;
        }
    }

    pub(super) fn accepts(&self, url: &Url) -> bool {
        self.is_live()
            && url.origin() == self.initial.origin()
            && approved_url(url.as_str()).is_ok()
    }

    pub(super) fn navigation(&self, url: &Url) -> bool {
        let allowed = self.accepts(url);
        let Ok(mut flags) = self.flags.lock() else {
            return false;
        };
        if !allowed {
            flags.navigation_blocked = true;
        }
        allowed
    }

    pub(super) fn page_load(&self, url: &Url, started: bool) {
        if !self.accepts(url) {
            return;
        }
        if let Ok(mut flags) = self.flags.lock() {
            if started {
                flags.current_load = Some(url.clone());
                flags.page_started = true;
                flags.loading = true;
            } else if flags.current_load.as_ref() == Some(url) {
                flags.loading = false;
            }
        }
    }

    pub(super) fn block_download(&self) {
        if let Ok(mut flags) = self.flags.lock() {
            flags.download_blocked = true;
        }
    }
}

#[derive(Default)]
struct HostState {
    last_id: u64,
    current: Option<Arc<Session>>,
}

#[derive(Default)]
pub(crate) struct BrowserHost(Mutex<HostState>);

impl BrowserHost {
    pub(super) fn reserve(&self, initial: Url) -> Result<Arc<Session>, &'static str> {
        let mut state = self.0.lock().map_err(|_| UNAVAILABLE)?;
        if state.current.is_some() {
            return Err("browser_already_open");
        }
        let id = state.last_id.checked_add(1).ok_or(UNAVAILABLE)?;
        let session = Arc::new(Session {
            id,
            initial,
            live: AtomicBool::new(true),
            flags: Mutex::new(Flags {
                opening: true,
                ..Flags::default()
            }),
        });
        state.last_id = id;
        state.current = Some(session.clone());
        Ok(session)
    }

    pub(super) fn current(&self) -> Result<Option<Arc<Session>>, &'static str> {
        self.0
            .lock()
            .map(|state| state.current.clone())
            .map_err(|_| UNAVAILABLE)
    }

    pub(super) fn matching(&self, value: &str) -> Result<Arc<Session>, &'static str> {
        if value.is_empty()
            || value.len() > 20
            || value.starts_with('0')
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(INVALID_SESSION);
        }
        let id: u64 = value.parse().map_err(|_| INVALID_SESSION)?;
        self.current()?
            .filter(|session| session.id == id && session.is_live())
            .ok_or(INVALID_SESSION)
    }

    pub(super) fn clear(&self, id: u64) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| UNAVAILABLE)?;
        if let Some(session) = &state.current {
            if session.id == id {
                session.live.store(false, Ordering::Release);
                state.current = None;
            }
        }
        Ok(())
    }
}
