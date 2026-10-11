//! The backend of a machine with no screen: lets a session serve files, terminals and
//! diagnostics without ever offering video.

use crate::{CaptureBackend, CaptureError, Capturer, CursorSource, MonitorInfo};

/// Reports no monitors; opening one fails with [`CaptureError::NoDisplay`].
#[derive(Debug, Default, Clone, Copy)]
pub struct HeadlessBackend;

impl CaptureBackend for HeadlessBackend {
    fn name(&self) -> &'static str {
        "headless"
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        Ok(Vec::new())
    }

    fn open(&self, _index: u8) -> Result<Box<dyn Capturer>, CaptureError> {
        Err(CaptureError::NoDisplay("this device has no screen".into()))
    }

    fn cursor_source(&self) -> Result<Option<Box<dyn CursorSource>>, CaptureError> {
        Ok(None)
    }
}
