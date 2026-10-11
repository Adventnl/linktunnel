//! The OS clipboard, behind a trait so the sync logic is testable.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::content::{ClipContent, decode_png, encode_png};
use crate::error::ClipError;

/// Something that can read and replace "the clipboard".
pub trait ClipboardBackend: Send + 'static {
    /// The current value (empty if the clipboard holds something unsupported).
    fn read(&mut self, include_image: bool) -> Result<ClipContent, ClipError>;
    /// Replaces the clipboard.
    fn write(&mut self, content: &ClipContent) -> Result<(), ClipError>;
}

/// The operating system clipboard (via `arboard`).
pub struct ArboardBackend {
    inner: arboard::Clipboard,
}

impl std::fmt::Debug for ArboardBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ArboardBackend")
    }
}

impl ArboardBackend {
    /// Connects to the clipboard of the current session.
    pub fn new() -> Result<Self, ClipError> {
        arboard::Clipboard::new()
            .map(|inner| Self { inner })
            .map_err(|e| ClipError::Backend(e.to_string()))
    }
}

impl ClipboardBackend for ArboardBackend {
    fn read(&mut self, include_image: bool) -> Result<ClipContent, ClipError> {
        let text = self.inner.get_text().ok().filter(|t| !t.is_empty());
        let image_png = if include_image && text.is_none() {
            match self.inner.get_image() {
                Ok(img) => {
                    let (w, h) = (
                        u32::try_from(img.width).unwrap_or(0),
                        u32::try_from(img.height).unwrap_or(0),
                    );
                    if w == 0 || h == 0 {
                        None
                    } else {
                        encode_png(w, h, &img.bytes).ok()
                    }
                }
                Err(_) => None,
            }
        } else {
            None
        };
        Ok(ClipContent { text, image_png })
    }

    fn write(&mut self, content: &ClipContent) -> Result<(), ClipError> {
        if let Some(text) = content.text.as_deref().filter(|t| !t.is_empty()) {
            return self
                .inner
                .set_text(text)
                .map_err(|e| ClipError::Backend(e.to_string()));
        }
        if let Some(png) = &content.image_png {
            let (w, h, rgba) = decode_png(png)?;
            return self
                .inner
                .set_image(arboard::ImageData {
                    width: w as usize,
                    height: h as usize,
                    bytes: rgba.into(),
                })
                .map_err(|e| ClipError::Backend(e.to_string()));
        }
        Ok(())
    }
}

/// An in-memory clipboard shared between handles: what the tests (and a headless box) use.
#[derive(Debug, Clone, Default)]
pub struct MemoryClipboard {
    value: Arc<Mutex<ClipContent>>,
    writes: Arc<Mutex<u64>>,
}

impl MemoryClipboard {
    /// An empty clipboard.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What the "user" copies.
    pub fn set(&self, content: ClipContent) {
        *self.value.lock() = content;
    }

    /// What a "paste" would give.
    #[must_use]
    pub fn get(&self) -> ClipContent {
        self.value.lock().clone()
    }

    /// How many times the sync wrote into it.
    #[must_use]
    pub fn sync_writes(&self) -> u64 {
        *self.writes.lock()
    }
}

impl ClipboardBackend for MemoryClipboard {
    fn read(&mut self, _include_image: bool) -> Result<ClipContent, ClipError> {
        Ok(self.get())
    }

    fn write(&mut self, content: &ClipContent) -> Result<(), ClipError> {
        *self.value.lock() = content.clone();
        *self.writes.lock() += 1;
        Ok(())
    }
}

impl ClipboardBackend for Box<dyn ClipboardBackend> {
    fn read(&mut self, include_image: bool) -> Result<ClipContent, ClipError> {
        (**self).read(include_image)
    }

    fn write(&mut self, content: &ClipContent) -> Result<(), ClipError> {
        (**self).write(content)
    }
}

/// Makes a fresh backend for one session (the OS clipboard handle is per-thread on some
/// platforms, so sessions do not share one).
pub type ClipboardFactory =
    Arc<dyn Fn() -> Result<Box<dyn ClipboardBackend>, ClipError> + Send + Sync>;

/// A factory for the OS clipboard.
#[must_use]
pub fn os_clipboard() -> ClipboardFactory {
    Arc::new(|| ArboardBackend::new().map(|b| Box::new(b) as Box<dyn ClipboardBackend>))
}

/// Clipboard sync for a session: where the clipboard is and which directions are on.
#[derive(Clone)]
pub struct ClipboardSetup {
    /// Makes the backend.
    pub factory: ClipboardFactory,
    /// Directions and limits.
    pub policy: crate::ClipboardPolicy,
}

impl std::fmt::Debug for ClipboardSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardSetup")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}
