//! Following the *input desktop* on Windows.
//!
//! A Windows session shows one of several desktops at a time: `Default` (the user's), `Winlogon`
//! (the sign-in and lock screens, and the UAC consent prompt, which is the "secure desktop"),
//! and `Screen-saver`. A thread sees and controls only the desktop it is attached to, so a
//! remote-control process that captured `Default` goes blind and deaf the moment UAC appears.
//!
//! The cure is to re-attach the capture and input threads to whichever desktop currently has the
//! user's attention ([`ThreadDesktop::sync`]) and rebuild what depended on the old one (the DXGI
//! duplication). Attaching to `Winlogon` needs the SYSTEM account running in that session, which
//! is what `nexus-winservice` arranges; for an ordinary user token `sync` reports the access
//! error and the caller keeps working on the desktop it has.
//!
//! On other platforms this crate is an inert stub with the same API, so callers need no `cfg`.

use std::fmt;

/// Why attaching to the input desktop failed.
#[derive(Debug, thiserror::Error)]
pub enum DesktopError {
    /// The OS refused (typically `ACCESS_DENIED` for a non-SYSTEM process facing the secure desktop).
    #[error("input desktop: {0}")]
    Os(String),
    /// Not a Windows machine.
    #[error("desktop switching is a Windows feature")]
    Unsupported,
}

/// The well-known desktop names.
pub mod names {
    /// The user's normal desktop.
    pub const DEFAULT: &str = "Default";
    /// Sign-in screen, lock screen, UAC consent prompt.
    pub const WINLOGON: &str = "Winlogon";
    /// The screen saver.
    pub const SCREEN_SAVER: &str = "Screen-saver";
}

/// True if `name` is a desktop that only SYSTEM can attach to.
#[must_use]
pub fn is_secure_desktop(name: &str) -> bool {
    name.eq_ignore_ascii_case(names::WINLOGON)
}

/// Keeps one thread attached to the input desktop.
///
/// Create one per thread that captures or injects, and call [`sync`](Self::sync) before each
/// capture attempt and each batch of input (it is cheap: one `OpenInputDesktop` and a name
/// compare when nothing changed; throttle it to a few times a second if called in a hot loop).
#[derive(Default)]
pub struct ThreadDesktop {
    inner: imp::Inner,
}

impl fmt::Debug for ThreadDesktop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadDesktop")
            .field("attached", &self.attached())
            .finish()
    }
}

impl ThreadDesktop {
    /// A tracker that is not attached to anything yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The name of the desktop this thread is attached to, if `sync` ever succeeded.
    #[must_use]
    pub fn attached(&self) -> Option<&str> {
        self.inner.name()
    }

    /// Attaches the calling thread to the current input desktop.
    ///
    /// Returns `Ok(Some(name))` when the thread moved to a different desktop (the caller must
    /// rebuild desktop-bound resources), `Ok(None)` when nothing changed.
    pub fn sync(&mut self) -> Result<Option<String>, DesktopError> {
        self.inner.sync()
    }
}

/// The name of the desktop that currently receives input, without attaching to it.
pub fn input_desktop_name() -> Result<String, DesktopError> {
    imp::input_desktop_name()
}

// Raw Win32 calls; every `unsafe` block states why it is sound.
#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use tracing::debug;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::StationsAndDesktops::{
        CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, GetUserObjectInformationW,
        HDESK, OpenInputDesktop, SetThreadDesktop, UOI_NAME,
    };

    use super::DesktopError;

    /// `GENERIC_ALL`: the access the capture and injection APIs need on the desktop.
    const DESKTOP_ALL_ACCESS: u32 = 0x1000_0000;

    fn os(e: impl std::fmt::Display) -> DesktopError {
        DesktopError::Os(e.to_string())
    }

    fn open_input() -> Result<HDESK, DesktopError> {
        // SAFETY: plain Win32 call; the returned handle is owned by the caller and closed with
        // `CloseDesktop`.
        unsafe {
            OpenInputDesktop(
                DESKTOP_CONTROL_FLAGS(0),
                false,
                DESKTOP_ACCESS_FLAGS(DESKTOP_ALL_ACCESS),
            )
        }
        .map_err(os)
    }

    fn name_of(desk: HDESK) -> Result<String, DesktopError> {
        let mut buf = [0u16; 256];
        let mut needed = 0u32;
        // SAFETY: `buf` is a writable buffer of the stated byte length; `desk` is a live handle.
        unsafe {
            GetUserObjectInformationW(
                HANDLE(desk.0),
                UOI_NAME,
                Some(buf.as_mut_ptr().cast()),
                u32::try_from(std::mem::size_of_val(&buf)).unwrap_or(0),
                Some(&mut needed),
            )
        }
        .map_err(os)?;
        let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        Ok(String::from_utf16_lossy(&buf[..len]))
    }

    pub(super) fn input_desktop_name() -> Result<String, DesktopError> {
        let desk = open_input()?;
        let name = name_of(desk);
        // SAFETY: `desk` was opened above and is not used afterwards.
        let _ = unsafe { CloseDesktop(desk) };
        name
    }

    #[derive(Default)]
    pub(super) struct Inner {
        current: Option<(HDESK, String)>,
    }

    // SAFETY: an HDESK is a process-wide kernel handle value; moving the tracker between threads
    // is harmless (it is only *used* to attach the calling thread).
    unsafe impl Send for Inner {}

    impl Inner {
        pub(super) fn name(&self) -> Option<&str> {
            self.current.as_ref().map(|(_, n)| n.as_str())
        }

        pub(super) fn sync(&mut self) -> Result<Option<String>, DesktopError> {
            let desk = open_input()?;
            let name = match name_of(desk) {
                Ok(n) => n,
                Err(e) => {
                    // SAFETY: opened above, not stored.
                    let _ = unsafe { CloseDesktop(desk) };
                    return Err(e);
                }
            };
            if self.name() == Some(name.as_str()) {
                // SAFETY: opened above, not stored.
                let _ = unsafe { CloseDesktop(desk) };
                return Ok(None);
            }
            // SAFETY: `desk` is live; on success the thread keeps using it until replaced.
            if let Err(e) = unsafe { SetThreadDesktop(desk) } {
                // SAFETY: opened above, not stored.
                let _ = unsafe { CloseDesktop(desk) };
                return Err(os(e));
            }
            debug!(desktop = %name, "thread attached to the input desktop");
            if let Some((old, _)) = self.current.replace((desk, name.clone())) {
                // SAFETY: the previous desktop handle is no longer used by this thread.
                let _ = unsafe { CloseDesktop(old) };
            }
            Ok(Some(name))
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            if let Some((desk, _)) = self.current.take() {
                // SAFETY: stored handle, closed exactly once.
                let _ = unsafe { CloseDesktop(desk) };
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::DesktopError;

    pub(super) fn input_desktop_name() -> Result<String, DesktopError> {
        Err(DesktopError::Unsupported)
    }

    #[derive(Default)]
    pub(super) struct Inner;

    impl Inner {
        pub(super) fn name(&self) -> Option<&str> {
            None
        }

        pub(super) fn sync(&mut self) -> Result<Option<String>, DesktopError> {
            Err(DesktopError::Unsupported)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_desktop_detection() {
        assert!(is_secure_desktop("Winlogon"));
        assert!(is_secure_desktop("winlogon"));
        assert!(!is_secure_desktop("Default"));
        assert!(!is_secure_desktop("Screen-saver"));
    }

    #[cfg(not(windows))]
    #[test]
    fn the_stub_reports_unsupported() {
        let mut t = ThreadDesktop::new();
        assert!(matches!(t.sync(), Err(DesktopError::Unsupported)));
        assert_eq!(t.attached(), None);
        assert!(input_desktop_name().is_err());
    }
}
