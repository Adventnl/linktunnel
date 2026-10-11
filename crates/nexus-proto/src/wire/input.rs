//! Input events (controller → target, reliable ordered stream).
//!
//! Keys are sent as **physical** USB-HID usages (page `0x07` keyboard, `0x0C` consumer), never
//! as characters: the target interprets them with *its own* keyboard layout, which is what makes
//! shortcuts, dead keys and non-US layouts behave as if the user sat at the target. Committed
//! text (IME composition, emoji, pasted unicode) is sent separately as [`InputEvent::Text`].
//!
//! Packet layout (16-byte header, then `count` events):
//!
//! ```text
//!  off  size  field
//!    0     1  wire version (1)
//!    1     1  event count (<= 64)
//!    2     2  flags (reserved, 0)
//!    4     4  seq   — sequence number of the first event; event i carries seq + i
//!    8     8  base_us — controller monotonic clock at the first event
//! ```
//!
//! Each event is `tag:u8, len:u16, dt_us:u32, body[len-4]`. The explicit length lets an old
//! target *skip* tags added by a newer controller instead of desynchronising, and lets a newer
//! target accept older, shorter bodies' successors (extra trailing body bytes are ignored).

use bytes::BufMut;

use super::{Reader, WIRE_VERSION, WireError};

/// Header size.
pub const HEADER_LEN: usize = 16;
/// Most events a single packet may carry.
pub const MAX_EVENTS: usize = 64;
/// Longest `Text` event body in UTF-8 bytes.
pub const MAX_TEXT_BYTES: usize = 1024;

/// HID usage page: keyboard / keypad.
pub const PAGE_KEYBOARD: u8 = 0x07;
/// HID usage page: consumer control (media keys).
pub const PAGE_CONSUMER: u8 = 0x0C;

/// Modifier state at the time of a key event (informational; the target tracks real state from
/// the key events themselves).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers(pub u16);

impl Modifiers {
    /// Shift.
    pub const SHIFT: Self = Self(1 << 0);
    /// Control.
    pub const CTRL: Self = Self(1 << 1);
    /// Alt / Option.
    pub const ALT: Self = Self(1 << 2);
    /// Windows key / Command.
    pub const META: Self = Self(1 << 3);
    /// Caps Lock engaged.
    pub const CAPS_LOCK: Self = Self(1 << 4);
    /// Num Lock engaged.
    pub const NUM_LOCK: Self = Self(1 << 5);
    /// AltGr / right Alt on layouts that treat it as a distinct level.
    pub const ALT_GR: Self = Self(1 << 6);
    /// Fn key.
    pub const FN: Self = Self(1 << 7);

    /// True when every bit of `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Union.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Mouse button index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseButton(pub u8);

impl MouseButton {
    /// Primary button.
    pub const LEFT: Self = Self(0);
    /// Secondary button.
    pub const RIGHT: Self = Self(1);
    /// Wheel click.
    pub const MIDDLE: Self = Self(2);
    /// "Back" thumb button.
    pub const BACK: Self = Self(3);
    /// "Forward" thumb button.
    pub const FORWARD: Self = Self(4);
}

/// Unit of scroll deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollUnit {
    /// 1/120 of a wheel notch (Windows `WHEEL_DELTA` convention); notched wheels send ±120.
    Detent,
    /// Device-independent pixels (smooth trackpad scrolling).
    Pixel,
}

/// Phase of a continuous scroll gesture (macOS trackpads report these).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollPhase {
    /// Not part of a gesture (ordinary wheel).
    None,
    /// Fingers touched down.
    Begin,
    /// Fingers moving.
    Change,
    /// Fingers lifted.
    End,
    /// Inertial scrolling after lift-off.
    Momentum,
}

/// Key transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    /// Released.
    Up,
    /// Pressed.
    Down,
    /// Auto-repeat while held.
    Repeat,
}

/// Touch / pen contact phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchPhase {
    /// Contact began.
    Down,
    /// Contact moved.
    Move,
    /// Contact ended.
    Up,
    /// Contact aborted.
    Cancel,
}

/// One input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// Absolute pointer position in the target monitor's coordinate space.
    MouseMoveAbs {
        /// Monitor index.
        monitor: u8,
        /// X in monitor pixels.
        x: i32,
        /// Y in monitor pixels.
        y: i32,
    },
    /// Relative pointer motion (pointer-lock / games).
    MouseMoveRel {
        /// Horizontal delta.
        dx: i32,
        /// Vertical delta.
        dy: i32,
    },
    /// Button transition.
    MouseButton {
        /// Which button.
        button: MouseButton,
        /// True on press, false on release.
        pressed: bool,
    },
    /// Wheel / trackpad scroll.
    Scroll {
        /// Horizontal amount (positive = right).
        dx: i32,
        /// Vertical amount (positive = up, matching physical finger/wheel direction).
        dy: i32,
        /// Unit of `dx`/`dy`.
        unit: ScrollUnit,
        /// Gesture phase.
        phase: ScrollPhase,
    },
    /// Physical key transition.
    Key {
        /// HID usage page.
        page: u8,
        /// HID usage id.
        usage: u16,
        /// Transition.
        state: KeyState,
        /// Modifier snapshot.
        modifiers: Modifiers,
    },
    /// Committed text to insert (IME output, unicode the layout cannot type).
    Text {
        /// UTF-8 text, at most [`MAX_TEXT_BYTES`] bytes.
        text: String,
    },
    /// Touch contact.
    Touch {
        /// Contact id.
        id: u8,
        /// Contact phase.
        phase: TouchPhase,
        /// X in monitor pixels.
        x: i32,
        /// Y in monitor pixels.
        y: i32,
        /// Pressure, 0..=65535.
        pressure: u16,
    },
    /// Pen contact with pressure and tilt.
    Pen {
        /// Pen id.
        id: u8,
        /// Contact phase.
        phase: TouchPhase,
        /// X in monitor pixels.
        x: i32,
        /// Y in monitor pixels.
        y: i32,
        /// Pressure, 0..=65535.
        pressure: u16,
        /// Tilt around X, degrees.
        tilt_x: i8,
        /// Tilt around Y, degrees.
        tilt_y: i8,
        /// Barrel / eraser button bits.
        buttons: u8,
    },
    /// Release every key and button that is currently held (focus loss, disconnect). Prevents
    /// stuck modifiers on the target.
    ReleaseAll,
}

/// An event plus its offset from the packet's `base_us`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedInput {
    /// Microseconds after the packet base time.
    pub dt_us: u32,
    /// The event.
    pub event: InputEvent,
}

/// A batch of input events.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InputPacket {
    /// Sequence number of the first event.
    pub seq: u32,
    /// Controller clock at the first event.
    pub base_us: u64,
    /// Events in the exact order they happened.
    pub events: Vec<TimedInput>,
    /// Events whose tag this build does not know (decode only).
    pub skipped: u8,
}

const TAG_MOVE_ABS: u8 = 1;
const TAG_MOVE_REL: u8 = 2;
const TAG_BUTTON: u8 = 3;
const TAG_SCROLL: u8 = 4;
const TAG_KEY: u8 = 5;
const TAG_TEXT: u8 = 6;
const TAG_TOUCH: u8 = 7;
const TAG_PEN: u8 = 8;
const TAG_RELEASE_ALL: u8 = 9;

fn unit_to_u8(u: ScrollUnit) -> u8 {
    match u {
        ScrollUnit::Detent => 0,
        ScrollUnit::Pixel => 1,
    }
}

fn unit_from_u8(v: u8) -> Result<ScrollUnit, WireError> {
    match v {
        0 => Ok(ScrollUnit::Detent),
        1 => Ok(ScrollUnit::Pixel),
        o => Err(WireError::Unknown {
            what: "scroll unit",
            value: u32::from(o),
        }),
    }
}

fn phase_to_u8(p: ScrollPhase) -> u8 {
    match p {
        ScrollPhase::None => 0,
        ScrollPhase::Begin => 1,
        ScrollPhase::Change => 2,
        ScrollPhase::End => 3,
        ScrollPhase::Momentum => 4,
    }
}

fn phase_from_u8(v: u8) -> Result<ScrollPhase, WireError> {
    match v {
        0 => Ok(ScrollPhase::None),
        1 => Ok(ScrollPhase::Begin),
        2 => Ok(ScrollPhase::Change),
        3 => Ok(ScrollPhase::End),
        4 => Ok(ScrollPhase::Momentum),
        o => Err(WireError::Unknown {
            what: "scroll phase",
            value: u32::from(o),
        }),
    }
}

fn key_state_to_u8(s: KeyState) -> u8 {
    match s {
        KeyState::Up => 0,
        KeyState::Down => 1,
        KeyState::Repeat => 2,
    }
}

fn key_state_from_u8(v: u8) -> Result<KeyState, WireError> {
    match v {
        0 => Ok(KeyState::Up),
        1 => Ok(KeyState::Down),
        2 => Ok(KeyState::Repeat),
        o => Err(WireError::Unknown {
            what: "key state",
            value: u32::from(o),
        }),
    }
}

fn touch_to_u8(p: TouchPhase) -> u8 {
    match p {
        TouchPhase::Down => 0,
        TouchPhase::Move => 1,
        TouchPhase::Up => 2,
        TouchPhase::Cancel => 3,
    }
}

fn touch_from_u8(v: u8) -> Result<TouchPhase, WireError> {
    match v {
        0 => Ok(TouchPhase::Down),
        1 => Ok(TouchPhase::Move),
        2 => Ok(TouchPhase::Up),
        3 => Ok(TouchPhase::Cancel),
        o => Err(WireError::Unknown {
            what: "touch phase",
            value: u32::from(o),
        }),
    }
}

impl InputEvent {
    /// Writes `(tag, body)` for this event into `body`.
    fn encode_body(&self, body: &mut Vec<u8>) -> Result<u8, WireError> {
        Ok(match self {
            Self::MouseMoveAbs { monitor, x, y } => {
                body.put_u8(*monitor);
                body.put_i32_le(*x);
                body.put_i32_le(*y);
                TAG_MOVE_ABS
            }
            Self::MouseMoveRel { dx, dy } => {
                body.put_i32_le(*dx);
                body.put_i32_le(*dy);
                TAG_MOVE_REL
            }
            Self::MouseButton { button, pressed } => {
                body.put_u8(button.0);
                body.put_u8(u8::from(*pressed));
                TAG_BUTTON
            }
            Self::Scroll {
                dx,
                dy,
                unit,
                phase,
            } => {
                body.put_i32_le(*dx);
                body.put_i32_le(*dy);
                body.put_u8(unit_to_u8(*unit));
                body.put_u8(phase_to_u8(*phase));
                TAG_SCROLL
            }
            Self::Key {
                page,
                usage,
                state,
                modifiers,
            } => {
                body.put_u8(*page);
                body.put_u16_le(*usage);
                body.put_u8(key_state_to_u8(*state));
                body.put_u16_le(modifiers.0);
                TAG_KEY
            }
            Self::Text { text } => {
                if text.len() > MAX_TEXT_BYTES {
                    return Err(WireError::TooLarge {
                        what: "text event",
                        len: text.len(),
                        max: MAX_TEXT_BYTES,
                    });
                }
                let len = u16::try_from(text.len()).map_err(|_| WireError::TooLarge {
                    what: "text event",
                    len: text.len(),
                    max: MAX_TEXT_BYTES,
                })?;
                body.put_u16_le(len);
                body.put_slice(text.as_bytes());
                TAG_TEXT
            }
            Self::Touch {
                id,
                phase,
                x,
                y,
                pressure,
            } => {
                body.put_u8(*id);
                body.put_u8(touch_to_u8(*phase));
                body.put_i32_le(*x);
                body.put_i32_le(*y);
                body.put_u16_le(*pressure);
                TAG_TOUCH
            }
            Self::Pen {
                id,
                phase,
                x,
                y,
                pressure,
                tilt_x,
                tilt_y,
                buttons,
            } => {
                body.put_u8(*id);
                body.put_u8(touch_to_u8(*phase));
                body.put_i32_le(*x);
                body.put_i32_le(*y);
                body.put_u16_le(*pressure);
                body.put_i8(*tilt_x);
                body.put_i8(*tilt_y);
                body.put_u8(*buttons);
                TAG_PEN
            }
            Self::ReleaseAll => TAG_RELEASE_ALL,
        })
    }

    /// Parses a body for a known `tag`; `Ok(None)` for tags this build does not know.
    fn decode_body(tag: u8, body: &[u8]) -> Result<Option<Self>, WireError> {
        let mut r = Reader::new(body);
        let ev = match tag {
            TAG_MOVE_ABS => Self::MouseMoveAbs {
                monitor: r.u8()?,
                x: r.i32()?,
                y: r.i32()?,
            },
            TAG_MOVE_REL => Self::MouseMoveRel {
                dx: r.i32()?,
                dy: r.i32()?,
            },
            TAG_BUTTON => Self::MouseButton {
                button: MouseButton(r.u8()?),
                pressed: r.u8()? != 0,
            },
            TAG_SCROLL => Self::Scroll {
                dx: r.i32()?,
                dy: r.i32()?,
                unit: unit_from_u8(r.u8()?)?,
                phase: phase_from_u8(r.u8()?)?,
            },
            TAG_KEY => Self::Key {
                page: r.u8()?,
                usage: r.u16()?,
                state: key_state_from_u8(r.u8()?)?,
                modifiers: Modifiers(r.u16()?),
            },
            TAG_TEXT => {
                let len = usize::from(r.u16()?);
                if len > MAX_TEXT_BYTES {
                    return Err(WireError::TooLarge {
                        what: "text event",
                        len,
                        max: MAX_TEXT_BYTES,
                    });
                }
                let raw = r.bytes(len)?;
                let text =
                    std::str::from_utf8(raw).map_err(|_| WireError::Invalid("utf-8 text"))?;
                Self::Text {
                    text: text.to_owned(),
                }
            }
            TAG_TOUCH => Self::Touch {
                id: r.u8()?,
                phase: touch_from_u8(r.u8()?)?,
                x: r.i32()?,
                y: r.i32()?,
                pressure: r.u16()?,
            },
            TAG_PEN => Self::Pen {
                id: r.u8()?,
                phase: touch_from_u8(r.u8()?)?,
                x: r.i32()?,
                y: r.i32()?,
                pressure: r.u16()?,
                tilt_x: r.i8()?,
                tilt_y: r.i8()?,
                buttons: r.u8()?,
            },
            TAG_RELEASE_ALL => Self::ReleaseAll,
            _ => return Ok(None),
        };
        // Trailing bytes inside a *known* event are tolerated: a newer sender may append fields.
        Ok(Some(ev))
    }
}

impl InputPacket {
    /// Appends the encoded packet to `out`.
    pub fn encode(&self, out: &mut impl BufMut) -> Result<(), WireError> {
        if self.events.len() > MAX_EVENTS {
            return Err(WireError::TooLarge {
                what: "input event count",
                len: self.events.len(),
                max: MAX_EVENTS,
            });
        }
        out.put_u8(WIRE_VERSION);
        out.put_u8(
            u8::try_from(self.events.len()).map_err(|_| WireError::TooLarge {
                what: "input event count",
                len: self.events.len(),
                max: MAX_EVENTS,
            })?,
        );
        out.put_u16_le(0);
        out.put_u32_le(self.seq);
        out.put_u64_le(self.base_us);

        let mut body = Vec::with_capacity(16);
        for timed in &self.events {
            body.clear();
            let tag = timed.event.encode_body(&mut body)?;
            let len = u16::try_from(body.len() + 4)
                .map_err(|_| WireError::Invalid("input event length"))?;
            out.put_u8(tag);
            out.put_u16_le(len);
            out.put_u32_le(timed.dt_us);
            out.put_slice(&body);
        }
        Ok(())
    }

    /// Decodes one packet. Unknown event tags are skipped and counted in `skipped`.
    pub fn decode(buf: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(buf);
        let version = r.u8()?;
        if version != WIRE_VERSION {
            return Err(WireError::BadVersion(version));
        }
        let count = usize::from(r.u8()?);
        if count > MAX_EVENTS {
            return Err(WireError::TooLarge {
                what: "input event count",
                len: count,
                max: MAX_EVENTS,
            });
        }
        let _flags = r.u16()?;
        let seq = r.u32()?;
        let base_us = r.u64()?;

        let mut events = Vec::with_capacity(count);
        let mut skipped = 0u8;
        for _ in 0..count {
            let tag = r.u8()?;
            let len = usize::from(r.u16()?);
            if len < 4 {
                return Err(WireError::Invalid("input event length"));
            }
            let dt_us = r.u32()?;
            let body = r.bytes(len - 4)?;
            match InputEvent::decode_body(tag, body)? {
                Some(event) => events.push(TimedInput { dt_us, event }),
                None => skipped = skipped.saturating_add(1),
            }
        }
        r.finish()?;
        Ok(Self {
            seq,
            base_us,
            events,
            skipped,
        })
    }
}

/// Merges adjacent events that are superseded by a later one, without ever reordering or
/// dropping key, button, text or touch-down/up events.
///
/// The target calls this on a backlog so a slow frame does not replay hundreds of stale mouse
/// positions: only the newest absolute position (or the sum of relative deltas) survives.
/// *Adjacent* is essential: a click between two moves keeps both moves, because the pointer
/// must be at the first position when the click lands.
#[must_use]
pub fn coalesce(events: Vec<TimedInput>) -> Vec<TimedInput> {
    let mut out: Vec<TimedInput> = Vec::with_capacity(events.len());
    for next in events {
        if let Some(last) = out.last_mut() {
            match (&mut last.event, &next.event) {
                (
                    InputEvent::MouseMoveAbs { monitor: m1, .. },
                    InputEvent::MouseMoveAbs { monitor: m2, .. },
                ) if m1 == m2 => {
                    *last = next;
                    continue;
                }
                (
                    InputEvent::MouseMoveRel { dx, dy },
                    InputEvent::MouseMoveRel { dx: ndx, dy: ndy },
                ) => {
                    *dx = dx.saturating_add(*ndx);
                    *dy = dy.saturating_add(*ndy);
                    last.dt_us = next.dt_us;
                    continue;
                }
                (
                    InputEvent::Scroll {
                        dx,
                        dy,
                        unit: u1,
                        phase: p1,
                    },
                    InputEvent::Scroll {
                        dx: ndx,
                        dy: ndy,
                        unit: u2,
                        phase: p2,
                    },
                ) if u1 == u2
                    && p1 == p2
                    && matches!(
                        p1,
                        ScrollPhase::None | ScrollPhase::Change | ScrollPhase::Momentum
                    ) =>
                {
                    *dx = dx.saturating_add(*ndx);
                    *dy = dy.saturating_add(*ndy);
                    last.dt_us = next.dt_us;
                    continue;
                }
                (
                    InputEvent::Touch {
                        id: i1,
                        phase: TouchPhase::Move,
                        ..
                    },
                    InputEvent::Touch {
                        id: i2,
                        phase: TouchPhase::Move,
                        ..
                    },
                )
                | (
                    InputEvent::Pen {
                        id: i1,
                        phase: TouchPhase::Move,
                        ..
                    },
                    InputEvent::Pen {
                        id: i2,
                        phase: TouchPhase::Move,
                        ..
                    },
                ) if i1 == i2 => {
                    *last = next;
                    continue;
                }
                _ => {}
            }
        }
        out.push(next);
    }
    out
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn key(usage: u16, state: KeyState) -> InputEvent {
        InputEvent::Key {
            page: PAGE_KEYBOARD,
            usage,
            state,
            modifiers: Modifiers::SHIFT,
        }
    }

    fn timed(dt_us: u32, event: InputEvent) -> TimedInput {
        TimedInput { dt_us, event }
    }

    fn every_variant() -> Vec<InputEvent> {
        vec![
            InputEvent::MouseMoveAbs {
                monitor: 1,
                x: -5,
                y: 2160,
            },
            InputEvent::MouseMoveRel { dx: -3, dy: 4 },
            InputEvent::MouseButton {
                button: MouseButton::RIGHT,
                pressed: true,
            },
            InputEvent::Scroll {
                dx: 0,
                dy: -120,
                unit: ScrollUnit::Detent,
                phase: ScrollPhase::None,
            },
            key(0x04, KeyState::Down),
            InputEvent::Key {
                page: PAGE_CONSUMER,
                usage: 0xE9,
                state: KeyState::Up,
                modifiers: Modifiers::default(),
            },
            InputEvent::Text {
                text: "héllo 世界 🎉".into(),
            },
            InputEvent::Touch {
                id: 3,
                phase: TouchPhase::Down,
                x: 10,
                y: 20,
                pressure: 40_000,
            },
            InputEvent::Pen {
                id: 1,
                phase: TouchPhase::Move,
                x: 1,
                y: 2,
                pressure: 65_535,
                tilt_x: -30,
                tilt_y: 45,
                buttons: 2,
            },
            InputEvent::ReleaseAll,
        ]
    }

    #[test]
    fn every_variant_roundtrips() {
        let packet = InputPacket {
            seq: u32::MAX - 2,
            base_us: 987_654_321,
            events: every_variant()
                .into_iter()
                .enumerate()
                .map(|(i, e)| timed(u32::try_from(i).unwrap() * 100, e))
                .collect(),
            skipped: 0,
        };
        let mut buf = Vec::new();
        packet.encode(&mut buf).unwrap();
        assert_eq!(InputPacket::decode(&buf).unwrap(), packet);
    }

    #[test]
    fn empty_packet_roundtrips() {
        let p = InputPacket {
            seq: 1,
            base_us: 2,
            events: vec![],
            skipped: 0,
        };
        let mut buf = Vec::new();
        p.encode(&mut buf).unwrap();
        assert_eq!(buf.len(), HEADER_LEN);
        assert_eq!(InputPacket::decode(&buf).unwrap(), p);
    }

    #[test]
    fn unknown_tags_are_skipped_not_fatal() {
        let mut buf = Vec::new();
        InputPacket {
            seq: 0,
            base_us: 0,
            events: vec![
                timed(0, key(4, KeyState::Down)),
                timed(5, key(4, KeyState::Up)),
            ],
            skipped: 0,
        }
        .encode(&mut buf)
        .unwrap();
        // Splice in an unknown tag (200) with a 3-byte body between the two keys, then fix count.
        let key_event_len = 1 + 2 + 4 + 6; // tag + len + dt + body
        let split = HEADER_LEN + key_event_len;
        let mut spliced = buf[..split].to_vec();
        spliced.extend_from_slice(&[200, 7, 0, 0, 0, 0, 0, 0xaa, 0xbb, 0xcc]);
        spliced.extend_from_slice(&buf[split..]);
        spliced[1] = 3;
        let decoded = InputPacket::decode(&spliced).unwrap();
        assert_eq!(decoded.events.len(), 2);
        assert_eq!(decoded.skipped, 1);
    }

    #[test]
    fn known_events_tolerate_appended_fields() {
        // A future controller adds a byte to MouseButton; this build still understands it.
        let mut buf = vec![WIRE_VERSION, 1, 0, 0, 0, 0, 0, 0];
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf.extend_from_slice(&[TAG_BUTTON, 7, 0, 0, 0, 0, 0, 0, 1, 0xEE]);
        let p = InputPacket::decode(&buf).unwrap();
        assert_eq!(
            p.events[0].event,
            InputEvent::MouseButton {
                button: MouseButton::LEFT,
                pressed: true
            }
        );
    }

    #[test]
    fn rejects_malformed_packets() {
        let mut ok = Vec::new();
        InputPacket {
            seq: 0,
            base_us: 0,
            events: vec![timed(0, key(4, KeyState::Down))],
            skipped: 0,
        }
        .encode(&mut ok)
        .unwrap();

        let mut bad = ok.clone();
        bad[0] = 3;
        assert_eq!(InputPacket::decode(&bad), Err(WireError::BadVersion(3)));

        let mut bad = ok.clone();
        bad[1] = 65; // count > MAX_EVENTS
        assert!(matches!(
            InputPacket::decode(&bad),
            Err(WireError::TooLarge { .. })
        ));

        let mut bad = ok.clone();
        bad[HEADER_LEN + 1] = 2; // event len < 4
        bad[HEADER_LEN + 2] = 0;
        assert_eq!(
            InputPacket::decode(&bad),
            Err(WireError::Invalid("input event length"))
        );

        let mut bad = ok.clone();
        bad.push(0);
        assert_eq!(InputPacket::decode(&bad), Err(WireError::Trailing(1)));

        assert!(matches!(
            InputPacket::decode(&ok[..ok.len() - 1]),
            Err(WireError::Truncated { .. })
        ));
    }

    #[test]
    fn text_is_validated() {
        let long = InputPacket {
            seq: 0,
            base_us: 0,
            events: vec![timed(
                0,
                InputEvent::Text {
                    text: "a".repeat(MAX_TEXT_BYTES + 1),
                },
            )],
            skipped: 0,
        };
        assert!(matches!(
            long.encode(&mut Vec::new()),
            Err(WireError::TooLarge { .. })
        ));

        // Invalid UTF-8 on the wire must be rejected, not passed to the OS input layer.
        let mut buf = vec![WIRE_VERSION, 1, 0, 0, 0, 0, 0, 0];
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf.extend_from_slice(&[TAG_TEXT, 4 + 2 + 2, 0, 0, 0, 0, 0, 2, 0, 0xff, 0xfe]);
        assert_eq!(
            InputPacket::decode(&buf),
            Err(WireError::Invalid("utf-8 text"))
        );
    }

    #[test]
    fn too_many_events_cannot_be_encoded() {
        let p = InputPacket {
            seq: 0,
            base_us: 0,
            events: vec![timed(0, InputEvent::ReleaseAll); MAX_EVENTS + 1],
            skipped: 0,
        };
        assert!(matches!(
            p.encode(&mut Vec::new()),
            Err(WireError::TooLarge { .. })
        ));
    }

    #[test]
    fn coalesce_keeps_only_the_last_absolute_move() {
        let out = coalesce(vec![
            timed(
                1,
                InputEvent::MouseMoveAbs {
                    monitor: 0,
                    x: 1,
                    y: 1,
                },
            ),
            timed(
                2,
                InputEvent::MouseMoveAbs {
                    monitor: 0,
                    x: 2,
                    y: 2,
                },
            ),
            timed(
                3,
                InputEvent::MouseMoveAbs {
                    monitor: 0,
                    x: 3,
                    y: 3,
                },
            ),
        ]);
        assert_eq!(
            out,
            vec![timed(
                3,
                InputEvent::MouseMoveAbs {
                    monitor: 0,
                    x: 3,
                    y: 3
                }
            )]
        );
    }

    #[test]
    fn coalesce_does_not_merge_across_monitors() {
        let input = vec![
            timed(
                1,
                InputEvent::MouseMoveAbs {
                    monitor: 0,
                    x: 1,
                    y: 1,
                },
            ),
            timed(
                2,
                InputEvent::MouseMoveAbs {
                    monitor: 1,
                    x: 2,
                    y: 2,
                },
            ),
        ];
        assert_eq!(coalesce(input.clone()), input);
    }

    #[test]
    fn coalesce_sums_relative_motion_and_scroll() {
        let out = coalesce(vec![
            timed(1, InputEvent::MouseMoveRel { dx: 2, dy: 3 }),
            timed(2, InputEvent::MouseMoveRel { dx: -5, dy: 1 }),
            timed(
                3,
                InputEvent::Scroll {
                    dx: 0,
                    dy: 10,
                    unit: ScrollUnit::Pixel,
                    phase: ScrollPhase::Change,
                },
            ),
            timed(
                4,
                InputEvent::Scroll {
                    dx: 1,
                    dy: 15,
                    unit: ScrollUnit::Pixel,
                    phase: ScrollPhase::Change,
                },
            ),
        ]);
        assert_eq!(
            out,
            vec![
                timed(2, InputEvent::MouseMoveRel { dx: -3, dy: 4 }),
                timed(
                    4,
                    InputEvent::Scroll {
                        dx: 1,
                        dy: 25,
                        unit: ScrollUnit::Pixel,
                        phase: ScrollPhase::Change
                    }
                ),
            ]
        );
    }

    #[test]
    fn coalesce_never_reorders_or_drops_keys_and_clicks() {
        let input = vec![
            timed(
                1,
                InputEvent::MouseMoveAbs {
                    monitor: 0,
                    x: 10,
                    y: 10,
                },
            ),
            timed(
                2,
                InputEvent::MouseButton {
                    button: MouseButton::LEFT,
                    pressed: true,
                },
            ),
            timed(
                3,
                InputEvent::MouseMoveAbs {
                    monitor: 0,
                    x: 20,
                    y: 20,
                },
            ),
            timed(
                4,
                InputEvent::MouseButton {
                    button: MouseButton::LEFT,
                    pressed: false,
                },
            ),
            timed(5, key(0x04, KeyState::Down)),
            timed(6, key(0x04, KeyState::Up)),
            timed(7, key(0x05, KeyState::Down)),
            timed(8, key(0x05, KeyState::Up)),
        ];
        // Nothing here is adjacent-mergeable: the click pins the pointer position.
        assert_eq!(coalesce(input.clone()), input);
    }

    #[test]
    fn coalesce_does_not_merge_scroll_gesture_boundaries() {
        let input = vec![
            timed(
                1,
                InputEvent::Scroll {
                    dx: 0,
                    dy: 5,
                    unit: ScrollUnit::Pixel,
                    phase: ScrollPhase::Begin,
                },
            ),
            timed(
                2,
                InputEvent::Scroll {
                    dx: 0,
                    dy: 5,
                    unit: ScrollUnit::Pixel,
                    phase: ScrollPhase::Begin,
                },
            ),
            timed(
                3,
                InputEvent::Scroll {
                    dx: 0,
                    dy: 0,
                    unit: ScrollUnit::Pixel,
                    phase: ScrollPhase::End,
                },
            ),
        ];
        assert_eq!(coalesce(input.clone()), input);
    }

    fn arb_event() -> impl Strategy<Value = InputEvent> {
        let touch = prop_oneof![
            Just(TouchPhase::Down),
            Just(TouchPhase::Move),
            Just(TouchPhase::Up),
            Just(TouchPhase::Cancel)
        ];
        prop_oneof![
            (any::<u8>(), any::<i32>(), any::<i32>())
                .prop_map(|(monitor, x, y)| InputEvent::MouseMoveAbs { monitor, x, y }),
            (any::<i32>(), any::<i32>()).prop_map(|(dx, dy)| InputEvent::MouseMoveRel { dx, dy }),
            (any::<u8>(), any::<bool>()).prop_map(|(b, pressed)| InputEvent::MouseButton {
                button: MouseButton(b),
                pressed
            }),
            (any::<u8>(), any::<u16>(), 0u8..3, any::<u16>()).prop_map(|(page, usage, st, m)| {
                InputEvent::Key {
                    page,
                    usage,
                    state: key_state_from_u8(st).unwrap(),
                    modifiers: Modifiers(m),
                }
            }),
            "\\PC{0,64}".prop_map(|text| InputEvent::Text { text }),
            (
                any::<u8>(),
                touch.clone(),
                any::<i32>(),
                any::<i32>(),
                any::<u16>()
            )
                .prop_map(|(id, phase, x, y, pressure)| InputEvent::Touch {
                    id,
                    phase,
                    x,
                    y,
                    pressure
                }),
            (
                any::<u8>(),
                touch,
                any::<i32>(),
                any::<i32>(),
                any::<u16>(),
                any::<i8>(),
                any::<i8>(),
                any::<u8>()
            )
                .prop_map(|(id, phase, x, y, pressure, tilt_x, tilt_y, buttons)| {
                    InputEvent::Pen {
                        id,
                        phase,
                        x,
                        y,
                        pressure,
                        tilt_x,
                        tilt_y,
                        buttons,
                    }
                }),
            Just(InputEvent::ReleaseAll),
        ]
    }

    proptest! {
        #[test]
        fn decode_never_panics(data in proptest::collection::vec(any::<u8>(), 0..400)) {
            let _ = InputPacket::decode(&data);
        }

        #[test]
        fn roundtrip_arbitrary(
            seq in any::<u32>(), base in any::<u64>(),
            events in proptest::collection::vec((any::<u32>(), arb_event()), 0..=MAX_EVENTS),
        ) {
            let packet = InputPacket {
                seq, base_us: base, skipped: 0,
                events: events.into_iter().map(|(dt_us, event)| TimedInput { dt_us, event }).collect(),
            };
            let mut buf = Vec::new();
            packet.encode(&mut buf).unwrap();
            prop_assert_eq!(InputPacket::decode(&buf).unwrap(), packet);
        }

        /// Coalescing is idempotent, never grows the batch, and never touches key order.
        #[test]
        fn coalesce_properties(events in proptest::collection::vec((any::<u32>(), arb_event()), 0..64)) {
            let input: Vec<TimedInput> = events.into_iter().map(|(dt_us, event)| TimedInput { dt_us, event }).collect();
            let once = coalesce(input.clone());
            prop_assert!(once.len() <= input.len());
            prop_assert_eq!(coalesce(once.clone()), once.clone());
            let keys = |v: &[TimedInput]| v.iter().filter(|t| matches!(t.event, InputEvent::Key { .. } | InputEvent::Text { .. } | InputEvent::MouseButton { .. })).cloned().collect::<Vec<_>>();
            prop_assert_eq!(keys(&once), keys(&input));
        }
    }
}
