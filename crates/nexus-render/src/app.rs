//! The window: presents pictures, shows the remote cursor shape as the local pointer, and turns
//! window events into input events.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use nexus_media::viewer::Picture;
use nexus_proto::v1::CursorShape;
use nexus_proto::wire::input::{
    InputEvent, KeyState, Modifiers, MouseButton, ScrollPhase, ScrollUnit,
};
use softbuffer::{Context, Surface};
use tokio::sync::mpsc::UnboundedSender;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, Ime, MouseButton as WinButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{ModifiersState, PhysicalKey};
use winit::window::{CursorIcon, CustomCursor, Fullscreen, Icon, Window, WindowId};

use crate::keymap::hid_for;
use crate::layout::{self, Rect, ScaleMode, dest_rect, source_rect, window_to_source};
use crate::session::{Command, UiEvent, ViewerOptions, spawn};

type Surf = Surface<Arc<Window>, Arc<Window>>;

struct App {
    options: ViewerOptions,
    commands: UnboundedSender<Command>,
    window: Option<Arc<Window>>,
    surface: Option<Surf>,
    gpu: Option<crate::gpu::Gpu>,
    picture: Option<Picture>,
    mailbox: Arc<crate::mailbox::Mailbox<Box<Picture>>>,
    embedded: bool,
    last_window_size: (u32, u32),
    reported_picture_size: Option<(u32, u32)>,
    viewport: Option<crate::embed::Viewport>,
    scale: ScaleMode,
    dest: Rect,
    mods: ModifiersState,
    shapes: HashMap<u32, CustomCursor>,
    pending_shapes: HashMap<u32, CursorShape>,
    shape_in_use: Option<u32>,
    title_stats: String,
    exit_reason: Option<String>,
    on_ready: Option<Box<dyn FnOnce()>>,
    last_pos: Option<PhysicalPosition<f64>>,
    /// Native size of the viewed monitor (pointer coordinates are in these pixels).
    native: Option<(u32, u32)>,
    /// The monitor being viewed, and how many there are.
    monitor: u32,
    monitor_count: u32,
    privacy: bool,
    touch_ids: HashMap<u64, u8>,
    #[cfg(windows)]
    keyboard_capture: Option<crate::windows_keyboard::KeyboardCapture>,
}

/// Opens the viewer window and runs until it closes. Returns why the session ended.
pub fn run(options: ViewerOptions) -> Result<String, String> {
    run_with_ready(options, || {})
}

/// Like [`run`], notifying the launcher once the first remote picture is decoded and uploaded.
pub fn run_with_ready(
    options: ViewerOptions,
    on_ready: impl FnOnce() + 'static,
) -> Result<String, String> {
    let event_loop: EventLoop<UiEvent> = EventLoop::with_user_event()
        .build()
        .map_err(|e| format!("cannot open a window: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy: EventLoopProxy<UiEvent> = event_loop.create_proxy();
    let mailbox = Arc::new(crate::mailbox::Mailbox::default());
    let outgoing = Arc::clone(&mailbox);
    let net_proxy = proxy.clone();
    let handle = spawn(options.clone(), move |ev| match ev {
        UiEvent::Picture(pic) => {
            if outgoing.put(pic) {
                let _ = net_proxy.send_event(UiEvent::PaintLatest);
            }
        }
        event => {
            let _ = net_proxy.send_event(event);
        }
    });
    let parent_available = cfg!(windows) && std::env::var_os("NEXUS_VIEWER_PARENT").is_some();
    let viewport = std::env::var("NEXUS_VIEWER_VIEWPORT")
        .ok()
        .and_then(|value| serde_json::from_str::<crate::embed::Viewport>(&value).ok())
        .filter(crate::embed::Viewport::valid);
    let embedded = parent_available && !viewport.as_ref().is_some_and(|v| v.detached);
    if std::env::var_os("NEXUS_VIEWER_STATUS").is_some() {
        let controls = proxy.clone();
        std::thread::Builder::new()
            .name("viewer-layout".into())
            .spawn(move || {
                crate::embed::read_controls(std::io::stdin(), |viewport| {
                    let _ = controls.send_event(UiEvent::Viewport(viewport));
                });
                let _ = controls.send_event(UiEvent::Ended("dashboard closed".into()));
            })
            .map_err(|e| format!("cannot start viewer controls: {e}"))?;
    }
    let initial_monitor = options.monitor;
    let initial_scale = options.scale;
    let mut app = App {
        options,
        commands: handle.commands,
        window: None,
        surface: None,
        gpu: None,
        picture: None,
        mailbox,
        embedded,
        last_window_size: (1024, 576),
        reported_picture_size: None,
        viewport,
        scale: initial_scale,
        dest: Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        },
        mods: ModifiersState::empty(),
        shapes: HashMap::new(),
        pending_shapes: HashMap::new(),
        shape_in_use: None,
        title_stats: String::new(),
        exit_reason: None,
        on_ready: Some(Box::new(on_ready)),
        last_pos: None,
        native: None,
        monitor: initial_monitor,
        monitor_count: 1,
        privacy: false,
        touch_ids: HashMap::new(),
        #[cfg(windows)]
        keyboard_capture: None,
    };
    event_loop
        .run_app(&mut app)
        .map_err(|e| format!("window event loop failed: {e}"))?;
    Ok(app.exit_reason.unwrap_or_else(|| "closed".into()))
}

impl App {
    fn send(&self, event: InputEvent) {
        let _ = self.commands.send(Command::Input(event));
    }

    fn picture_size(&self) -> Option<(u32, u32)> {
        self.picture.as_ref().map(|p| (p.width, p.height))
    }

    fn update_title(&self) {
        if let Some(w) = &self.window {
            let title = if self.title_stats.is_empty() {
                format!("linktunnel — {}", self.options.device_name)
            } else {
                self.title_stats.clone()
            };
            w.set_title(&title);
            if self.embedded {
                use std::io::Write as _;
                let mut output = std::io::stdout().lock();
                let _ = writeln!(
                    output,
                    "{}",
                    serde_json::json!({"nexusViewer": "stats", "text": title})
                );
                let _ = output.flush();
            }
        }
    }

    fn presentation_scale(&self) -> ScaleMode {
        if !self.embedded
            && self
                .window
                .as_ref()
                .is_some_and(|w| w.fullscreen().is_none())
            && self.scale == ScaleMode::Fit
        {
            // Rounding and in-flight native resize events must not introduce black strips.
            ScaleMode::Fill
        } else {
            self.scale
        }
    }

    fn size_window_to_remote(&mut self, remote: (u32, u32)) {
        if self.embedded {
            return;
        }
        if let Some(window) = &self.window {
            if window.fullscreen().is_some() {
                return;
            }
            let monitor = window.current_monitor().map(|m| m.size());
            let bounds = monitor.map_or((1280, 800), |s| (s.width * 4 / 5, s.height * 4 / 5));
            let size = layout::windowed_size(remote, bounds);
            self.last_window_size = size;
            let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(size.0, size.1));
        }
    }

    fn paint(&mut self) {
        let scale = self.presentation_scale();
        if let (Some(window), Some(gpu)) = (&self.window, &mut self.gpu) {
            let size = window.inner_size();
            if let Some(pic) = &self.picture {
                self.dest = dest_rect((pic.width, pic.height), (size.width, size.height), scale);
            }
            if let Err(e) = gpu.paint(
                size.width,
                size.height,
                self.dest,
                self.picture.as_ref().map(|pic| {
                    source_rect((pic.width, pic.height), (size.width, size.height), scale)
                }),
            ) {
                tracing::error!(error = %e, "GPU presentation failed");
            }
            return;
        }
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return;
        };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        if surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };
        buffer.fill(0);
        if let Some(pic) = &self.picture {
            let dest = dest_rect((pic.width, pic.height), (size.width, size.height), scale);
            self.dest = dest;
            let source = source_rect((pic.width, pic.height), (size.width, size.height), scale);
            blit(&mut buffer, size.width, size.height, pic, dest, source);
        }
        let _ = buffer.present();
    }

    fn toggle_fullscreen(&self) {
        if self.embedded {
            use std::io::Write as _;
            let mut output = std::io::stdout().lock();
            let _ = writeln!(
                output,
                "{}",
                serde_json::json!({"nexusViewer": "fullscreen"})
            );
            let _ = output.flush();
            return;
        }
        if let Some(w) = &self.window {
            w.set_fullscreen(if w.fullscreen().is_some() {
                None
            } else {
                Some(Fullscreen::Borderless(None))
            });
        }
    }

    fn apply_shape(&mut self, id: u32) {
        let (Some(window), Some(cursor)) = (&self.window, self.shapes.get(&id)) else {
            return;
        };
        window.set_cursor(cursor.clone());
        self.shape_in_use = Some(id);
    }
}

/// Copies `pic` into the window buffer (0RGB u32 per pixel) scaled into `dest`.
fn blit(buffer: &mut [u32], win_w: u32, win_h: u32, pic: &Picture, dest: Rect, source: Rect) {
    let (pw, ph) = (pic.width as usize, pic.height as usize);
    if pic.bgra.len() < pw * ph * 4 || pw == 0 || ph == 0 {
        return;
    }
    let (ww, dw, dh) = (win_w as usize, dest.w as usize, dest.h as usize);
    // Map each destination column to a source column once per frame.
    let xmap: Vec<usize> = (0..dw)
        .map(|x| (source.x as usize + x * source.w as usize / dw.max(1)).min(pw - 1))
        .collect();
    for dy in 0..dh {
        let wy = dest.y as usize + dy;
        if wy >= win_h as usize {
            break;
        }
        let sy = (source.y as usize + dy * source.h as usize / dh.max(1)).min(ph - 1);
        let src_row = &pic.bgra[sy * pw * 4..(sy + 1) * pw * 4];
        let row = &mut buffer[wy * ww..(wy + 1) * ww];
        for (dx, sx) in xmap.iter().enumerate() {
            let wx = dest.x as usize + dx;
            if wx >= ww {
                break;
            }
            let i = sx * 4;
            row[wx] = u32::from(src_row[i])
                | (u32::from(src_row[i + 1]) << 8)
                | (u32::from(src_row[i + 2]) << 16);
        }
    }
}

impl ApplicationHandler<UiEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(format!("linktunnel — {}", self.options.device_name))
            .with_window_icon(
                Icon::from_rgba(
                    include_bytes!("../../../assets/branding/linktunnel-32.rgba").to_vec(),
                    32,
                    32,
                )
                .ok(),
            )
            .with_inner_size(LogicalSize::new(1024.0, 576.0));
        #[cfg(windows)]
        let attrs = if self.embedded {
            let parent = std::env::var("NEXUS_VIEWER_PARENT")
                .ok()
                .and_then(|v| v.parse::<isize>().ok())
                .unwrap_or(0);
            match crate::windows_embed::attributes(attrs, parent) {
                Ok(attrs) => attrs,
                Err(e) => {
                    self.exit_reason = Some(e);
                    event_loop.exit();
                    return;
                }
            }
        } else {
            attrs
        };
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.exit_reason = Some(format!("cannot create the window: {e}"));
                event_loop.exit();
                return;
            }
        };
        window.set_ime_allowed(true);
        if self.options.fullscreen && !self.embedded {
            window.set_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        if self.embedded
            && let Some(viewport) = &self.viewport
        {
            window.set_outer_position(winit::dpi::PhysicalPosition::new(viewport.x, viewport.y));
            let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(
                viewport.width,
                viewport.height,
            ));
            self.scale = viewport.scale;
        }
        #[cfg(windows)]
        match crate::windows_keyboard::KeyboardCapture::start(&window, self.commands.clone()) {
            Ok(capture) => self.keyboard_capture = Some(capture),
            Err(e) => tracing::warn!(error = %e, "system shortcut capture unavailable"),
        }
        if std::env::var_os("NEXUS_SOFTWARE_VIEWER").is_none() {
            match pollster::block_on(crate::gpu::Gpu::new(Arc::clone(&window))) {
                Ok(gpu) => {
                    self.gpu = Some(gpu);
                    self.window = Some(window);
                    return;
                }
                Err(e) => tracing::warn!(error = %e, "GPU unavailable; using software presenter"),
            }
        }
        let surface =
            Context::new(Arc::clone(&window)).and_then(|c| Surface::new(&c, Arc::clone(&window)));
        match surface {
            Ok(s) => self.surface = Some(s),
            Err(e) => {
                self.exit_reason = Some(format!("cannot create a drawing surface: {e}"));
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UiEvent) {
        match event {
            UiEvent::Opened {
                width,
                height,
                encoder,
                native,
                monitors,
            } => {
                self.native = Some(native);
                self.monitor_count = monitors.max(1);
                tracing::info!(width, height, %encoder, "viewer connected");
                self.size_window_to_remote((width, height));
            }
            UiEvent::DisplaysChanged(monitors) => {
                self.monitor_count = u32::try_from(monitors.len()).unwrap_or(1).max(1);
                if let Some(monitor) = monitors.iter().find(|m| m.index == self.monitor) {
                    self.native = Some((monitor.width, monitor.height));
                } else if let Some(monitor) = monitors.first() {
                    self.send(InputEvent::ReleaseAll);
                    let _ = self.commands.send(Command::SwitchMonitor(monitor.index));
                }
            }
            UiEvent::MonitorChanged { monitor, native } => {
                self.monitor = monitor;
                self.native = Some(native);
                // The previous monitor's cursor bitmap and picture are meaningless now.
                self.picture = None;
            }
            UiEvent::Viewport(viewport) => {
                self.scale = viewport.scale;
                #[cfg(windows)]
                if std::env::var_os("NEXUS_VIEWER_PARENT").is_some()
                    && self.embedded == viewport.detached
                {
                    if let Some(window) = &self.window {
                        let parent = std::env::var("NEXUS_VIEWER_PARENT")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        match crate::windows_embed::set_detached(window, parent, viewport.detached)
                        {
                            Ok(()) => {
                                self.embedded = !viewport.detached;
                                if viewport.detached {
                                    self.size_window_to_remote(
                                        self.picture_size().unwrap_or((1920, 1080)),
                                    );
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error, "cannot change viewer presentation")
                            }
                        }
                    }
                }
                if let Some(window) = &self.window {
                    if self.embedded {
                        window.set_visible(viewport.visible && self.picture.is_some());
                        window.set_outer_position(winit::dpi::PhysicalPosition::new(
                            viewport.x, viewport.y,
                        ));
                        let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(
                            viewport.width,
                            viewport.height,
                        ));
                    }
                    window.request_redraw();
                }
                self.viewport = Some(viewport);
            }
            UiEvent::Picture(_) => {}
            UiEvent::PaintLatest => {
                let Some(pic) = self.mailbox.take() else {
                    return;
                };
                if let Some(gpu) = &mut self.gpu
                    && let Err(e) = gpu.upload(&pic)
                {
                    self.exit_reason = Some(e);
                    event_loop.exit();
                    return;
                }
                let picture_size = (pic.width, pic.height);
                if self.reported_picture_size != Some(picture_size) {
                    // Adaptive stream resolution can change without changing the screen shape.
                    // Preserve the user's window size through those quality adjustments.
                    let aspect_changed = self.reported_picture_size.is_none_or(|previous| {
                        u64::from(previous.0) * u64::from(picture_size.1)
                            != u64::from(picture_size.0) * u64::from(previous.1)
                    });
                    self.reported_picture_size = Some(picture_size);
                    if aspect_changed {
                        self.size_window_to_remote(picture_size);
                    }
                    if std::env::var_os("NEXUS_VIEWER_STATUS").is_some() {
                        use std::io::Write as _;
                        let mut output = std::io::stdout().lock();
                        let _ = writeln!(
                            output,
                            "{}",
                            serde_json::json!({"nexusViewer": "geometry", "width": pic.width, "height": pic.height})
                        );
                        let _ = output.flush();
                    }
                }
                self.picture = Some(*pic);
                if let Some(ready) = self.on_ready.take() {
                    if self.embedded
                        && let Some(window) = &self.window
                    {
                        window.set_visible(self.viewport.as_ref().is_none_or(|v| v.visible));
                    }
                    ready();
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            UiEvent::Shape(shape) => {
                let id = shape.shape_id;
                if shape.width == 0 || shape.height == 0 || shape.width > 256 || shape.height > 256
                {
                    return;
                }
                // Premultiplied BGRA → straight RGBA.
                let mut rgba = Vec::with_capacity(shape.bgra.len());
                for px in shape.bgra.chunks_exact(4) {
                    let a = u32::from(px[3]);
                    let un = |c: u8| {
                        (u32::from(c) * 255)
                            .checked_div(a)
                            .map_or(0, |v| u8::try_from(v.min(255)).unwrap_or(255))
                    };
                    rgba.extend_from_slice(&[un(px[2]), un(px[1]), un(px[0]), px[3]]);
                }
                let hot = |v: u32| u16::try_from(v).unwrap_or(0);
                if let Ok(source) = CustomCursor::from_rgba(
                    rgba,
                    hot(shape.width),
                    hot(shape.height),
                    hot(shape.hot_x),
                    hot(shape.hot_y),
                ) {
                    let cursor = event_loop.create_custom_cursor(source);
                    self.shapes.insert(id, cursor);
                    if self.shape_in_use.is_none() || self.pending_shapes.remove(&id).is_some() {
                        self.apply_shape(id);
                    }
                }
            }
            UiEvent::Cursor(update) => {
                if self.shape_in_use != Some(update.shape_id)
                    && self.shapes.contains_key(&update.shape_id)
                {
                    self.apply_shape(update.shape_id);
                }
                if let Some(w) = &self.window
                    && !update.visible
                {
                    w.set_cursor(CursorIcon::Default);
                    self.shape_in_use = None;
                }
            }
            UiEvent::Privacy(enabled) => self.privacy = enabled,
            UiEvent::Stats(s) => {
                self.title_stats = s;
                self.update_title();
            }
            UiEvent::Reconnecting {
                attempt,
                reason,
                in_secs,
            } => {
                self.title_stats =
                    format!("reconnecting (attempt {attempt}, next in {in_secs}s): {reason}");
                self.update_title();
            }
            UiEvent::Ended(reason) => {
                self.exit_reason = Some(reason);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                let _ = self.commands.send(Command::Quit);
                event_loop.exit();
            }
            WindowEvent::RedrawRequested | WindowEvent::Resized(_) => {
                if let WindowEvent::Resized(size) = &event {
                    if !self.embedded && size.width > 0 && size.height > 0 {
                        if let Some(window) = &self.window {
                            if window.fullscreen().is_none() && !window.is_maximized() {
                                if let Some(remote) = self.picture_size() {
                                    let next = layout::aspect_resize(
                                        remote,
                                        (size.width, size.height),
                                        self.last_window_size,
                                    );
                                    self.last_window_size = next;
                                    if next != (size.width, size.height) {
                                        let _ = window.request_inner_size(
                                            winit::dpi::PhysicalSize::new(next.0, next.1),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                self.paint();
                if matches!(event, WindowEvent::Resized(_))
                    && let Some(w) = &self.window
                {
                    w.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::Focused(false) => {
                self.touch_ids.clear();
                self.send(InputEvent::ReleaseAll);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                let pressed = event.state == ElementState::Pressed;
                // Local hotkeys (Ctrl+Alt+Shift+…): never forwarded.
                let local = self.mods.control_key() && self.mods.alt_key() && self.mods.shift_key();
                if pressed && local {
                    use winit::keyboard::KeyCode::{KeyB, KeyD, KeyF, KeyM, KeyP, KeyQ};
                    match code {
                        KeyD if !event.repeat => {
                            self.send(InputEvent::ReleaseAll);
                            // A normal input grant/consent applies to this chord; the Windows
                            // injector routes it to the service's secure-attention broker.
                            // The outgoing agent applies the optional Cmd/Ctrl mapping. Use
                            // the inverse source modifier so the target always receives Ctrl.
                            let control = if self.options.key_remap && cfg!(target_os = "macos") {
                                0xe3
                            } else {
                                0xe0
                            };
                            for (usage, state) in [
                                (control, KeyState::Down),
                                (0xe2, KeyState::Down),
                                (0x4c, KeyState::Down),
                                (0x4c, KeyState::Up),
                                (0xe2, KeyState::Up),
                                (control, KeyState::Up),
                            ] {
                                self.send(InputEvent::Key {
                                    page: nexus_proto::wire::input::PAGE_KEYBOARD,
                                    usage,
                                    state,
                                    modifiers: Default::default(),
                                });
                            }
                            return;
                        }
                        KeyB => {
                            self.send(InputEvent::ReleaseAll);
                            self.privacy = !self.privacy;
                            let _ = self.commands.send(Command::Privacy(self.privacy));
                            return;
                        }
                        KeyM if self.monitor_count > 1 => {
                            self.send(InputEvent::ReleaseAll);
                            let next = (self.monitor + 1) % self.monitor_count;
                            let _ = self.commands.send(Command::SwitchMonitor(next));
                            return;
                        }
                        KeyQ => {
                            self.send(InputEvent::ReleaseAll);
                            let _ = self.commands.send(Command::Quit);
                            event_loop.exit();
                            return;
                        }
                        KeyF => {
                            self.send(InputEvent::ReleaseAll);
                            self.toggle_fullscreen();
                            return;
                        }
                        KeyP => {
                            self.scale = if self.scale == ScaleMode::Fit {
                                ScaleMode::Pixel
                            } else {
                                ScaleMode::Fit
                            };
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                            return;
                        }
                        _ => {}
                    }
                }
                let Some((page, usage)) = hid_for(code) else {
                    return;
                };
                let state = match (pressed, event.repeat) {
                    (true, true) => KeyState::Repeat,
                    (true, false) => KeyState::Down,
                    (false, _) => KeyState::Up,
                };
                let mut m = Modifiers::default();
                if self.mods.shift_key() {
                    m = m.with(Modifiers::SHIFT);
                }
                if self.mods.control_key() {
                    m = m.with(Modifiers::CTRL);
                }
                if self.mods.alt_key() {
                    m = m.with(Modifiers::ALT);
                }
                if self.mods.super_key() {
                    m = m.with(Modifiers::META);
                }
                self.send(InputEvent::Key {
                    page,
                    usage,
                    state,
                    modifiers: m,
                });
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                // Plain ASCII typing arrives as key events; only composed text needs this path.
                if !text.is_ascii() {
                    self.send(InputEvent::Text { text });
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.last_pos = Some(position);
                if let (Some(size), Some(window)) = (self.picture_size(), &self.window) {
                    let win = window.inner_size();
                    let dest = dest_rect(size, (win.width, win.height), self.presentation_scale());
                    if let Some((px, py)) = window_to_source(
                        (position.x, position.y),
                        dest,
                        source_rect(size, (win.width, win.height), self.presentation_scale()),
                    ) {
                        // The picture may be a scaled-down version of the monitor.
                        let (x, y) = layout::to_native((px, py), size, self.native);
                        let monitor = u8::try_from(self.monitor).unwrap_or(0);
                        self.send(InputEvent::MouseMoveAbs { monitor, x, y });
                    }
                }
            }
            WindowEvent::Touch(touch) => {
                use winit::event::TouchPhase as Phase;
                if touch.phase == Phase::Started {
                    let Some(id) =
                        (0u8..10).find(|id| !self.touch_ids.values().any(|held| held == id))
                    else {
                        return;
                    };
                    self.touch_ids.insert(touch.id, id);
                }
                let Some(id) = self.touch_ids.get(&touch.id).copied() else {
                    return;
                };
                let phase = match touch.phase {
                    Phase::Started => nexus_proto::wire::input::TouchPhase::Down,
                    Phase::Moved => nexus_proto::wire::input::TouchPhase::Move,
                    Phase::Ended => nexus_proto::wire::input::TouchPhase::Up,
                    Phase::Cancelled => nexus_proto::wire::input::TouchPhase::Cancel,
                };
                if let (Some(size), Some(window)) = (self.picture_size(), &self.window) {
                    let win = window.inner_size();
                    let dest = dest_rect(size, (win.width, win.height), self.presentation_scale());
                    let position = (
                        touch.location.x.clamp(
                            f64::from(dest.x),
                            f64::from(dest.x) + f64::from(dest.w.saturating_sub(1)),
                        ),
                        touch.location.y.clamp(
                            f64::from(dest.y),
                            f64::from(dest.y) + f64::from(dest.h.saturating_sub(1)),
                        ),
                    );
                    if let Some(pixel) = window_to_source(
                        position,
                        dest,
                        source_rect(size, (win.width, win.height), self.presentation_scale()),
                    ) {
                        let (x, y) = layout::to_native(pixel, size, self.native);
                        let monitor = u8::try_from(self.monitor).unwrap_or(0);
                        // Touch wire events are monitor-local; identify the current monitor first.
                        self.send(InputEvent::MouseMoveAbs { monitor, x, y });
                        // Quantize normalized pressure after clamping it to the u16 wire range.
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let pressure = (touch
                            .force
                            .as_ref()
                            .map_or(0.5, |force| force.normalized())
                            .clamp(0.0, 1.0)
                            * 65535.0) as u16;
                        self.send(InputEvent::Touch {
                            id,
                            phase,
                            x,
                            y,
                            pressure,
                        });
                    }
                }
                if matches!(touch.phase, Phase::Ended | Phase::Cancelled) {
                    self.touch_ids.remove(&touch.id);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    WinButton::Left => MouseButton::LEFT,
                    WinButton::Right => MouseButton::RIGHT,
                    WinButton::Middle => MouseButton::MIDDLE,
                    WinButton::Back => MouseButton::BACK,
                    WinButton::Forward => MouseButton::FORWARD,
                    WinButton::Other(_) => return,
                };
                self.send(InputEvent::MouseButton {
                    button,
                    pressed: state == ElementState::Pressed,
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                #[allow(clippy::cast_possible_truncation)]
                let (dx, dy, unit) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        ((x * 120.0) as i32, (y * 120.0) as i32, ScrollUnit::Detent)
                    }
                    MouseScrollDelta::PixelDelta(p) => (p.x as i32, p.y as i32, ScrollUnit::Pixel),
                };
                if dx != 0 || dy != 0 {
                    self.send(InputEvent::Scroll {
                        dx,
                        dy,
                        unit,
                        phase: ScrollPhase::None,
                    });
                }
            }
            _ => {}
        }
    }
}
