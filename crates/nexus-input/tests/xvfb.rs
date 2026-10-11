//! Injection tests against a real (virtual) X server: inject, then read what an X client sees.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use nexus_input::x11::X11Injector;
use nexus_input::{
    Guarded, InputEvent, KeyState, Modifiers, MonitorRect, MouseButton, ScrollPhase, ScrollUnit,
};
use nexus_testkit::Xvfb;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// A full-screen window that records the input it receives.
struct Listener {
    conn: RustConnection,
}

impl Listener {
    fn new(display: &str) -> Self {
        let (conn, screen) = RustConnection::connect(Some(display)).unwrap();
        let s = &conn.setup().roots[screen];
        let (root, w, h) = (s.root, s.width_in_pixels, s.height_in_pixels);
        let win = conn.generate_id().unwrap();
        conn.create_window(
            0,
            win,
            root,
            0,
            0,
            w,
            h,
            0,
            xproto::WindowClass::INPUT_OUTPUT,
            0,
            &xproto::CreateWindowAux::new()
                .override_redirect(1)
                .event_mask(
                    xproto::EventMask::KEY_PRESS
                        | xproto::EventMask::KEY_RELEASE
                        | xproto::EventMask::BUTTON_PRESS
                        | xproto::EventMask::BUTTON_RELEASE
                        | xproto::EventMask::POINTER_MOTION,
                ),
        )
        .unwrap();
        conn.map_window(win).unwrap();
        conn.set_input_focus(xproto::InputFocus::POINTER_ROOT, win, x11rb::CURRENT_TIME)
            .unwrap();
        conn.sync().unwrap();
        Self { conn }
    }

    /// Collects `want` input events (key, button, motion), ignoring server housekeeping such as
    /// `MappingNotify`.
    fn events(&self, want: usize) -> Vec<Event> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut out = Vec::new();
        while out.len() < want && Instant::now() < deadline {
            match self.conn.poll_for_event().unwrap() {
                Some(
                    e @ (Event::KeyPress(_)
                    | Event::KeyRelease(_)
                    | Event::ButtonPress(_)
                    | Event::ButtonRelease(_)
                    | Event::MotionNotify(_)),
                ) => out.push(e),
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        out
    }

    fn drain(&self) {
        std::thread::sleep(Duration::from_millis(50));
        while self.conn.poll_for_event().unwrap().is_some() {}
    }
}

fn key(usage: u16, state: KeyState) -> InputEvent {
    InputEvent::Key {
        page: 7,
        usage,
        state,
        modifiers: Modifiers::default(),
    }
}

fn setup() -> Option<(Xvfb, Listener, Guarded)> {
    let xvfb = Xvfb::start(800, 600)?;
    let listener = Listener::new(xvfb.display());
    let mut inj = X11Injector::connect(Some(xvfb.display())).unwrap();
    nexus_input::Injector::set_monitors(
        &mut inj,
        &[MonitorRect {
            index: 0,
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        }],
    );
    Some((xvfb, listener, Guarded::new(Box::new(inj))))
}

#[test]
fn pointer_moves_clicks_and_clamps() {
    let Some((_x, listener, mut inj)) = setup() else {
        return;
    };
    inj.inject(&InputEvent::MouseMoveAbs {
        monitor: 0,
        x: 300,
        y: 200,
    })
    .unwrap();
    inj.inject(&InputEvent::MouseButton {
        button: MouseButton::LEFT,
        pressed: true,
    })
    .unwrap();
    inj.inject(&InputEvent::MouseButton {
        button: MouseButton::LEFT,
        pressed: false,
    })
    .unwrap();
    inj.inject(&InputEvent::MouseButton {
        button: MouseButton::RIGHT,
        pressed: true,
    })
    .unwrap();
    inj.inject(&InputEvent::MouseButton {
        button: MouseButton::RIGHT,
        pressed: false,
    })
    .unwrap();
    inj.inject(&InputEvent::MouseMoveAbs {
        monitor: 0,
        x: 99_999,
        y: -50,
    })
    .unwrap(); // clamped into the monitor
    let events = listener.events(6);

    let motion: Vec<(i16, i16)> = events
        .iter()
        .filter_map(|e| {
            if let Event::MotionNotify(m) = e {
                Some((m.root_x, m.root_y))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(motion.first(), Some(&(300, 200)));
    assert_eq!(
        motion.last(),
        Some(&(799, 0)),
        "out-of-range positions are clamped to the monitor"
    );
    let buttons: Vec<(u8, bool)> = events
        .iter()
        .filter_map(|e| match e {
            Event::ButtonPress(b) => Some((b.detail, true)),
            Event::ButtonRelease(b) => Some((b.detail, false)),
            _ => None,
        })
        .collect();
    assert_eq!(buttons, vec![(1, true), (1, false), (3, true), (3, false)]);
}

#[test]
fn keys_use_hid_to_evdev_plus_8_and_keep_order() {
    let Some((_x, listener, mut inj)) = setup() else {
        return;
    };
    inj.inject(&InputEvent::MouseMoveAbs {
        monitor: 0,
        x: 10,
        y: 10,
    })
    .unwrap();
    listener.drain();
    // Shift+A, then Enter.
    inj.inject(&key(0xE1, KeyState::Down)).unwrap();
    inj.inject(&key(0x04, KeyState::Down)).unwrap();
    inj.inject(&key(0x04, KeyState::Up)).unwrap();
    inj.inject(&key(0xE1, KeyState::Up)).unwrap();
    inj.inject(&key(0x28, KeyState::Down)).unwrap();
    inj.inject(&key(0x28, KeyState::Up)).unwrap();
    let events = listener.events(6);
    let keys: Vec<(u8, bool)> = events
        .iter()
        .filter_map(|e| match e {
            Event::KeyPress(k) => Some((k.detail, true)),
            Event::KeyRelease(k) => Some((k.detail, false)),
            _ => None,
        })
        .collect();
    // evdev: LeftShift 42, A 30, Enter 28; +8 → X keycodes.
    assert_eq!(
        keys,
        vec![
            (50, true),
            (38, true),
            (38, false),
            (50, false),
            (36, true),
            (36, false)
        ]
    );
}

#[test]
fn release_all_unsticks_modifiers() {
    let Some((_x, listener, mut inj)) = setup() else {
        return;
    };
    inj.inject(&InputEvent::MouseMoveAbs {
        monitor: 0,
        x: 10,
        y: 10,
    })
    .unwrap();
    listener.drain();
    inj.inject(&key(0xE0, KeyState::Down)).unwrap(); // ctrl
    inj.inject(&InputEvent::MouseButton {
        button: MouseButton::MIDDLE,
        pressed: true,
    })
    .unwrap();
    assert_eq!(inj.held(), 2);
    inj.inject(&InputEvent::ReleaseAll).unwrap();
    assert_eq!(inj.held(), 0);
    let events = listener.events(4);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::KeyRelease(k) if k.detail == 37)),
        "ctrl released: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ButtonRelease(b) if b.detail == 2)),
        "middle button released"
    );
}

#[test]
fn scroll_becomes_wheel_buttons() {
    let Some((_x, listener, mut inj)) = setup() else {
        return;
    };
    inj.inject(&InputEvent::MouseMoveAbs {
        monitor: 0,
        x: 50,
        y: 50,
    })
    .unwrap();
    listener.drain();
    inj.inject(&InputEvent::Scroll {
        dx: 0,
        dy: 240,
        unit: ScrollUnit::Detent,
        phase: ScrollPhase::None,
    })
    .unwrap();
    inj.inject(&InputEvent::Scroll {
        dx: 0,
        dy: -120,
        unit: ScrollUnit::Detent,
        phase: ScrollPhase::None,
    })
    .unwrap();
    // 80 px of smooth scrolling is two 40-px notches.
    inj.inject(&InputEvent::Scroll {
        dx: 0,
        dy: 80,
        unit: ScrollUnit::Pixel,
        phase: ScrollPhase::Change,
    })
    .unwrap();
    let presses: Vec<u8> = listener
        .events(14)
        .iter()
        .filter_map(|e| {
            if let Event::ButtonPress(b) = e {
                Some(b.detail)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(presses, vec![4, 4, 5, 4, 4], "up, up, down, up, up");
}

#[test]
fn text_is_typed_through_a_temporary_keycode() {
    let Some((_x, listener, mut inj)) = setup() else {
        return;
    };
    inj.inject(&InputEvent::MouseMoveAbs {
        monitor: 0,
        x: 10,
        y: 10,
    })
    .unwrap();
    listener.drain();
    inj.inject(&InputEvent::Text {
        text: "hé€".into()
    })
    .unwrap();
    let events = listener.events(6);
    let presses = events
        .iter()
        .filter(|e| matches!(e, Event::KeyPress(_)))
        .count();
    assert_eq!(presses, 3, "one key press per character: {events:?}");
    // Plain ASCII needs no keymap changes at all: it uses the real keys (with Shift when needed).
    listener.drain();
    inj.inject(&InputEvent::Text { text: "aB".into() }).unwrap();
    let ascii: Vec<(u8, bool)> = listener
        .events(6)
        .iter()
        .filter_map(|e| match e {
            Event::KeyPress(k) => Some((k.detail, true)),
            Event::KeyRelease(k) => Some((k.detail, false)),
            _ => None,
        })
        .collect();
    // a = 38; B = Shift (50) + b (56).
    assert_eq!(
        ascii,
        vec![
            (38, true),
            (38, false),
            (50, true),
            (56, true),
            (56, false),
            (50, false)
        ]
    );
}

#[test]
fn unsupported_and_invalid_events_are_errors_not_panics() {
    let Some((_x, _l, mut inj)) = setup() else {
        return;
    };
    assert!(
        inj.inject(&InputEvent::MouseMoveAbs {
            monitor: 7,
            x: 0,
            y: 0
        })
        .is_err()
    );
    assert!(
        inj.inject(&InputEvent::Key {
            page: 0x55,
            usage: 1,
            state: KeyState::Down,
            modifiers: Modifiers::default()
        })
        .is_err()
    );
    assert!(inj.inject(&key(0x66, KeyState::Down)).is_err());
    assert!(
        inj.inject(&InputEvent::Touch {
            id: 0,
            phase: nexus_input::TouchPhase::Down,
            x: 1,
            y: 1,
            pressure: 1
        })
        .is_err()
    );
}

#[test]
fn a_held_injected_key_is_repeated_by_the_server() {
    // Documents the behaviour the viewer relies on: X auto-repeats a held key itself, so the
    // controller must NOT also forward its local repeat events as extra presses on X11.
    let Some((_x, listener, mut inj)) = setup() else {
        return;
    };
    inj.inject(&InputEvent::MouseMoveAbs {
        monitor: 0,
        x: 10,
        y: 10,
    })
    .unwrap();
    listener.drain();
    inj.inject(&key(0x04, KeyState::Down)).unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    inj.inject(&key(0x04, KeyState::Up)).unwrap();
    let presses = listener
        .events(100)
        .iter()
        .filter(|e| matches!(e, Event::KeyPress(_)))
        .count();
    println!("key presses seen while holding for 1.2 s: {presses}");
    assert!(
        presses > 1,
        "expected server-side auto-repeat, saw {presses} press(es)"
    );
}
