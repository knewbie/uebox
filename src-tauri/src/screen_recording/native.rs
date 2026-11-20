//! Exact capture.m ABI. Tests and non-macOS hosts cannot invoke real capture.
use std::ffi::{c_int, CStr};

type Event = extern "C" fn(u64, c_int);

#[cfg(all(target_os = "macos", not(test)))]
extern "C" {
    fn uebox_capture_available() -> bool;
    fn uebox_capture_start(path: *const std::ffi::c_char, audio: bool, token: u64, event: Event);
    fn uebox_capture_stop(token: u64);
}

pub(super) fn available() -> bool {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        // Only an OS availability check; never requests permission.
        unsafe { uebox_capture_available() }
    }
    #[cfg(any(not(target_os = "macos"), test))]
    false
}

pub(super) fn start(_path: &CStr, _audio: bool, _token: u64, _event: Event) {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        // CString lives through the call; native copies it synchronously and
        // retains the callback, which is a process-lifetime function pointer.
        unsafe { uebox_capture_start(_path.as_ptr(), _audio, _token, _event) };
    }
}

pub(super) fn stop(_token: u64) {
    #[cfg(all(target_os = "macos", not(test)))]
    unsafe {
        uebox_capture_stop(_token);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn synthetic_builds_never_report_native_support() {
        assert!(!super::available());
    }
}
