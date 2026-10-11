//! Well-known capability strings advertised in [`crate::v1::Capabilities::features`].
//!
//! Strings (not enum values) keep negotiation open-ended: a peer simply ignores identifiers it
//! does not recognise, and a new feature never requires a coordinated protocol bump.

/// Text clipboard sync.
pub const CLIPBOARD_TEXT: &str = "clipboard.text";
/// Rich text / HTML clipboard sync.
pub const CLIPBOARD_RICH: &str = "clipboard.rich";
/// Image clipboard sync.
pub const CLIPBOARD_IMAGE: &str = "clipboard.image";
/// File-list clipboard (copy/paste of files between machines).
pub const CLIPBOARD_FILES: &str = "clipboard.files";
/// Resumable, hash-verified file transfer.
pub const FILES_RESUME: &str = "files.resume";
/// Opus audio stream from target to controller.
pub const AUDIO_OPUS: &str = "audio.opus";
/// PTY terminal sessions.
pub const TERMINAL_PTY: &str = "terminal.pty";
/// Local/remote TCP port forwarding over the session.
pub const PORT_FORWARD: &str = "portfwd";
/// Blank the target's physical display and block local input during a session.
pub const PRIVACY_SCREEN: &str = "privacy.screen";
/// Capture/inject on the Windows secure desktop (UAC prompt, lock screen).
pub const SECURE_DESKTOP: &str = "windows.secure_desktop";
/// More than one monitor can be streamed.
pub const MULTI_MONITOR: &str = "display.multi_monitor";
/// HDR capture with tone-mapping (and passthrough when supported).
pub const HDR: &str = "display.hdr";
/// Headless virtual display (Xvfb / headless compositor).
pub const VIRTUAL_DISPLAY: &str = "display.virtual";
/// Touch input injection.
pub const INPUT_TOUCH: &str = "input.touch";
/// Pen (pressure/tilt) input injection.
pub const INPUT_PEN: &str = "input.pen";
/// Power actions (lock, sleep, restart, shutdown).
pub const POWER_ACTIONS: &str = "power.actions";
/// Separate cursor position/shape channel (controller renders the cursor locally).
pub const CURSOR_CHANNEL: &str = "cursor.channel";
/// Frame headers carry dirty rectangles.
pub const DIRTY_RECTS: &str = "video.dirty_rects";
/// Loss recovery via reference-frame invalidation instead of full keyframes.
pub const REF_INVALIDATION: &str = "video.ref_invalidation";
/// Keyboard, mouse, scroll and committed-text injection.
pub const INPUT_BASIC: &str = "input.basic";
