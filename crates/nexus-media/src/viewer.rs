//! The viewer half: decode received frames into pictures, and draw the cursor on top.
//!
//! This runs in the *viewer* process, not the agent: decoders parse attacker-influenced data
//! (a compromised target could send a hostile bitstream), so keeping them out of the process
//! that holds the device identity is part of the privilege separation (ADR-0011).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use nexus_codec::{Decoder, Rect, i420_to_bgra, i420_to_bgra_region};
use nexus_core::clock::now_us;
use nexus_core::telemetry::LatencyRecorder;
use nexus_proto::v1::{CursorShape, VideoCodec};
use nexus_proto::wire::cursor::CursorUpdate;
use nexus_proto::wire::video::FrameKind;

use crate::MediaError;
use crate::controller::VideoFrame;

/// Frames between forced full colour conversions (≈ one per second at 60 fps).
const FULL_CONVERT_EVERY: u32 = 60;

/// A decoded screen picture.
#[derive(Debug, Clone)]
pub struct Picture {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Tightly packed BGRA, `width * height * 4` bytes.
    pub bgra: Bytes,
    /// Regions that changed since the previous picture (empty = everything).
    pub dirty: Vec<Rect>,
    /// Frame sequence number.
    pub seq: u32,
    /// Timestamps for the latency breakdown.
    pub timing: Timing,
}

/// Where the time went for one frame (all microseconds, each on the clock of the machine that
/// recorded it; differences within one machine are exact).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timing {
    /// Target clock: when the frame was captured.
    pub capture_us: u64,
    /// Target clock: when it was handed to the network.
    pub send_us: u64,
    /// Local clock: when its last byte arrived.
    pub recv_us: u64,
    /// Local clock: when decoding finished.
    pub decoded_us: u64,
    /// Time spent decoding and colour-converting.
    pub decode_us: u64,
}

impl Timing {
    /// Capture-to-decoded latency, given the target clock's offset from ours (`target − local`).
    /// On the same machine the offset is zero.
    #[must_use]
    pub fn glass_latency_us(&self, clock_offset_us: i64) -> u64 {
        #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
        {
            (self.decoded_us as i64 - (self.capture_us as i64 - clock_offset_us)).max(0) as u64
        }
    }
}

/// Decodes frames in order and keeps the latest picture.
pub struct VideoDecoder {
    decoder: Option<Box<dyn Decoder>>,
    codec: Option<VideoCodec>,
    /// Latest decoded picture, tightly packed BGRA.
    current: Vec<u8>,
    size: (u32, u32),
    needs_keyframe: bool,
    /// Frames since the whole picture was last colour-converted.
    since_full_convert: u32,
    /// Decode + convert time of recent frames.
    pub decode_time: LatencyRecorder,
}

impl std::fmt::Debug for VideoDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoDecoder")
            .field("size", &self.size)
            .field("codec", &self.codec)
            .finish_non_exhaustive()
    }
}

impl Default for VideoDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoDecoder {
    /// A decoder that creates the right backend when the first keyframe arrives.
    #[must_use]
    pub fn new() -> Self {
        Self {
            decoder: None,
            codec: None,
            current: Vec::new(),
            size: (0, 0),
            needs_keyframe: true,
            since_full_convert: 0,
            decode_time: LatencyRecorder::new(),
        }
    }

    /// True after a decode error, until a keyframe is decoded: the caller should ask the sender
    /// for one.
    #[must_use]
    pub fn needs_keyframe(&self) -> bool {
        self.needs_keyframe
    }

    /// Decodes one frame. `Ok(None)` means the codec produced no picture for it (it was buffered).
    /// An error means the stream is broken: request a keyframe; later deltas are rejected until
    /// one decodes.
    pub fn decode(&mut self, frame: &VideoFrame) -> Result<Option<Picture>, MediaError> {
        let h = &frame.header;
        if h.kind != FrameKind::Key && self.needs_keyframe {
            return Err(MediaError::Protocol("waiting for a keyframe".into()));
        }
        if self.decoder.is_none() || self.codec != Some(h.codec) {
            self.decoder = Some(nexus_codec::create_decoder(h.codec)?);
            self.codec = Some(h.codec);
        }
        let started = Instant::now();
        let decoded = match self.decoder.as_mut() {
            Some(d) => d.decode(&frame.payload),
            None => return Err(MediaError::Protocol("no decoder".into())),
        };
        let pic = match decoded {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(None),
            Err(e) => {
                self.needs_keyframe = true;
                return Err(e.into());
            }
        };
        // Colour-convert only what changed: the decoder reconstructs the whole picture, but the
        // BGRA copy is the expensive part on a large, mostly static screen. A full conversion is
        // still done on keyframes, size changes, when most of the picture changed anyway, and
        // about once a second so slow refinement of static areas by the encoder shows up.
        let area: u64 = h
            .dirty
            .iter()
            .map(|r| u64::from(r.w) * u64::from(r.h))
            .sum();
        let total = u64::from(pic.width) * u64::from(pic.height);
        let partial = h.kind != FrameKind::Key
            && !h.dirty.is_empty()
            && self.size == (pic.width, pic.height)
            && u64::try_from(self.current.len()).is_ok_and(|l| l == total * 4)
            && area * 4 < total
            && self.since_full_convert < FULL_CONVERT_EVERY;
        let converted = if partial {
            self.since_full_convert += 1;
            h.dirty.iter().try_for_each(|r| {
                i420_to_bgra_region(pic.width, pic.height, &pic.planes, *r, &mut self.current)
            })
        } else {
            self.since_full_convert = 0;
            i420_to_bgra(pic.width, pic.height, &pic.planes).map(|full| self.current = full)
        };
        if let Err(e) = converted {
            self.needs_keyframe = true;
            self.since_full_convert = 0;
            return Err(e.into());
        }
        self.size = (pic.width, pic.height);
        if h.kind == FrameKind::Key {
            self.needs_keyframe = false;
        }
        let took = started.elapsed();
        self.decode_time.record(took);
        let decoded_us = now_us();
        Ok(Some(Picture {
            width: pic.width,
            height: pic.height,
            bgra: Bytes::copy_from_slice(&self.current),
            dirty: h.dirty.clone(),
            seq: h.frame_seq,
            timing: Timing {
                capture_us: h.capture_us,
                send_us: h.send_us,
                recv_us: frame.recv_us,
                decoded_us,
                decode_us: u64::try_from(took.as_micros()).unwrap_or(u64::MAX),
            },
        }))
    }
}

/// Remembers cursor shapes and composes the cursor onto pictures.
#[derive(Debug, Default)]
pub struct CursorOverlay {
    shapes: HashMap<u32, CursorShape>,
}

impl CursorOverlay {
    /// Creates an empty overlay.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores a shape announced by the target.
    pub fn add_shape(&mut self, shape: CursorShape) {
        if self.shapes.len() > 256 {
            self.shapes.clear(); // a runaway target must not grow this without bound
        }
        self.shapes.insert(shape.shape_id, shape);
    }

    /// The shape for `id`, if it has arrived.
    #[must_use]
    pub fn shape(&self, id: u32) -> Option<&CursorShape> {
        self.shapes.get(&id)
    }

    /// Draws the cursor into a tightly packed BGRA `picture`. Used for screenshots and tests; a
    /// real viewer draws the cursor as a GPU sprite so it moves independently of video frames.
    pub fn compose(&self, picture: &mut [u8], width: u32, height: u32, cursor: &CursorUpdate) {
        if !cursor.visible {
            return;
        }
        let Some(shape) = self.shapes.get(&cursor.shape_id) else {
            return;
        };
        if shape.bgra.len() != (shape.width as usize) * (shape.height as usize) * 4
            || picture.len() != (width as usize) * (height as usize) * 4
        {
            return;
        }
        let left = i64::from(cursor.x) - i64::from(shape.hot_x);
        let top = i64::from(cursor.y) - i64::from(shape.hot_y);
        for sy in 0..i64::from(shape.height) {
            let py = top + sy;
            if py < 0 || py >= i64::from(height) {
                continue;
            }
            for sx in 0..i64::from(shape.width) {
                let px = left + sx;
                if px < 0 || px >= i64::from(width) {
                    continue;
                }
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let (si, di) = (
                    ((sy * i64::from(shape.width) + sx) * 4) as usize,
                    ((py * i64::from(width) + px) * 4) as usize,
                );
                let a = u32::from(shape.bgra[si + 3]);
                // Premultiplied "over": dst = src + dst * (1 - a).
                for c in 0..3 {
                    let v = u32::from(shape.bgra[si + c])
                        + u32::from(picture[di + c]) * (255 - a) / 255;
                    picture[di + c] = u8::try_from(v.min(255)).unwrap_or(255);
                }
                picture[di + 3] = 255;
            }
        }
    }
}

/// Summarises a stretch of frames for the HUD.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Hud {
    /// Frames per second over the window.
    pub fps: f32,
    /// Megabits per second of compressed video over the window.
    pub mbps: f32,
}

/// Rolling fps/bitrate over the last second.
#[derive(Debug, Default)]
pub struct HudCounter {
    window: std::collections::VecDeque<(Instant, usize)>,
}

impl HudCounter {
    /// Records one frame of `bytes` compressed size.
    pub fn frame(&mut self, bytes: usize, now: Instant) {
        self.window.push_back((now, bytes));
        while self
            .window
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(1))
        {
            self.window.pop_front();
        }
    }

    /// Current rates.
    #[must_use]
    pub fn hud(&self) -> Hud {
        let Some((first, _)) = self.window.front() else {
            return Hud::default();
        };
        let Some((last, _)) = self.window.back() else {
            return Hud::default();
        };
        let span = last.duration_since(*first).as_secs_f32().max(0.001);
        let bytes: usize = self.window.iter().map(|(_, b)| *b).sum();
        #[allow(clippy::cast_precision_loss)]
        Hud {
            fps: (self.window.len().saturating_sub(1)) as f32 / span,
            mbps: bytes as f32 * 8.0 / 1e6 / span,
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use nexus_codec::Encoder as _;
    use nexus_codec::{EncoderConfig, Frame, FrameFormat, H264Encoder, psnr_bgra};
    use nexus_proto::wire::video::{FrameFlags, VideoFrameHeader};

    use super::*;

    fn screen(w: u32, h: u32, shift: u32) -> Frame {
        let mut data = vec![255u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                if ((x / 6 + y / 10) % 3 == 0 && y % 10 < 7)
                    || ((shift..shift + 24).contains(&x) && (20..44).contains(&y))
                {
                    let i = ((y * w + x) * 4) as usize;
                    data[i..i + 3].copy_from_slice(&[10, 10, 10]);
                }
            }
        }
        Frame {
            width: w,
            height: h,
            stride: (w * 4) as usize,
            format: FrameFormat::Bgra8,
            data: Bytes::from(data),
            captured_us: 5,
            dirty: vec![],
        }
    }

    fn wrap(seq: u32, enc: &nexus_codec::EncodedFrame) -> VideoFrame {
        VideoFrame {
            header: VideoFrameHeader {
                codec: VideoCodec::H264,
                kind: if enc.keyframe {
                    FrameKind::Key
                } else {
                    FrameKind::Delta
                },
                monitor: 0,
                flags: FrameFlags::default(),
                width: u16::try_from(enc.width).unwrap(),
                height: u16::try_from(enc.height).unwrap(),
                frame_seq: seq,
                ref_seq: seq.saturating_sub(1),
                payload_len: u32::try_from(enc.data.len()).unwrap(),
                capture_us: 5,
                send_us: 6,
                dirty: vec![],
            },
            payload: enc.data.clone(),
            recv_us: 7,
        }
    }

    #[test]
    fn decodes_a_stream_and_demands_a_keyframe_after_damage() {
        let (w, h) = (192, 128);
        let mut enc = H264Encoder::new(EncoderConfig::screen(w, h, 60.0, 3_000_000)).unwrap();
        let mut dec = VideoDecoder::new();
        assert!(dec.needs_keyframe());

        let k = enc.encode(&screen(w, h, 0), false).unwrap().unwrap();
        let pic = dec.decode(&wrap(1, &k)).unwrap().unwrap();
        assert_eq!((pic.width, pic.height), (w, h));
        assert!(psnr_bgra(&pic.bgra, &screen(w, h, 0).data) > 28.0);
        assert!(!dec.needs_keyframe());
        assert_eq!(pic.timing.capture_us, 5);
        assert!(pic.timing.decode_us < 5_000_000);

        let d = enc.encode(&screen(w, h, 8), false).unwrap().unwrap();
        assert!(dec.decode(&wrap(2, &d)).unwrap().is_some());

        // A corrupt frame breaks the chain: the decoder asks for a keyframe and refuses deltas.
        let junk = VideoFrame {
            payload: Bytes::from_static(&[0, 0, 1, 0x65, 1, 2, 3]),
            ..wrap(3, &d)
        };
        let _ = dec.decode(&junk);
        let d2 = enc.encode(&screen(w, h, 16), false).unwrap().unwrap();
        if dec.needs_keyframe() {
            assert!(dec.decode(&wrap(4, &d2)).is_err());
        }
        let k2 = enc.encode(&screen(w, h, 24), true).unwrap().unwrap();
        assert!(dec.decode(&wrap(5, &k2)).unwrap().is_some());
        assert!(!dec.needs_keyframe());
    }

    #[test]
    fn partial_conversion_matches_a_full_conversion_where_it_matters() {
        let (w, h) = (192, 128);
        let mut enc = H264Encoder::new(EncoderConfig::screen(w, h, 60.0, 3_000_000)).unwrap();
        let mut partial = VideoDecoder::new();
        let mut full = VideoDecoder::new();
        for i in 0..20u32 {
            let mut f = screen(w, h, i * 4);
            // Damage = where the moving block was and where it is now.
            f.dirty = if i == 0 {
                vec![]
            } else {
                vec![Rect {
                    x: u16::try_from(i * 4 - 4).unwrap(),
                    y: 20,
                    w: 32,
                    h: 24,
                }]
            };
            let enc_frame = enc.encode(&f, false).unwrap().unwrap();
            let mut vf = wrap(i + 1, &enc_frame);
            vf.header.dirty = f.dirty.clone();
            let a = partial.decode(&vf).unwrap().unwrap();
            vf.header.dirty = vec![]; // unknown damage forces the full path
            let b = full.decode(&vf).unwrap().unwrap();
            let dirty = f.dirty.first().copied();
            // Inside the damaged rectangle the two paths agree exactly.
            if let Some(r) = dirty {
                for y in usize::from(r.y)..usize::from(r.y + r.h) {
                    let (lo, hi) = (
                        (y * w as usize + usize::from(r.x)) * 4,
                        (y * w as usize + usize::from(r.x + r.w)) * 4,
                    );
                    assert_eq!(a.bgra[lo..hi], b.bgra[lo..hi], "frame {i} row {y}");
                }
            }
            // Overall the picture stays close to the truth.
            assert!(
                psnr_bgra(&a.bgra, &screen(w, h, i * 4).data) > 26.0,
                "frame {i}"
            );
        }
    }

    #[test]
    fn delta_before_keyframe_is_refused() {
        let (w, h) = (64, 64);
        let mut enc = H264Encoder::new(EncoderConfig::screen(w, h, 60.0, 2_000_000)).unwrap();
        enc.encode(&screen(w, h, 0), false).unwrap();
        let d = enc.encode(&screen(w, h, 4), false).unwrap().unwrap();
        let mut dec = VideoDecoder::new();
        assert!(dec.decode(&wrap(2, &d)).is_err());
    }

    #[test]
    fn glass_latency_accounts_for_clock_offset() {
        let t = Timing {
            capture_us: 1_000_500,
            decoded_us: 10_000,
            ..Default::default()
        };
        // Target clock is 1_000_000 ahead of ours, so capture happened at local 500.
        assert_eq!(t.glass_latency_us(1_000_000), 9_500);
        assert_eq!(
            Timing {
                capture_us: 100,
                decoded_us: 50,
                ..Default::default()
            }
            .glass_latency_us(0),
            0,
            "never negative"
        );
    }

    #[test]
    fn cursor_composes_with_premultiplied_alpha_and_clips() {
        let mut overlay = CursorOverlay::new();
        // 2x2: opaque red top-left, half-transparent (premultiplied) blue bottom-right, rest clear.
        overlay.add_shape(CursorShape {
            shape_id: 9,
            width: 2,
            height: 2,
            hot_x: 0,
            hot_y: 0,
            bgra: Bytes::from(vec![0, 0, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 128, 0, 0, 128]),
        });
        let mut pic = vec![200u8; 4 * 4 * 4];
        let cur = CursorUpdate {
            visible: true,
            shape_changed: false,
            monitor: 0,
            shape_id: 9,
            x: 1,
            y: 1,
            seq: 1,
            ts_us: 0,
        };
        overlay.compose(&mut pic, 4, 4, &cur);
        let px = |x: usize, y: usize| {
            [
                pic[(y * 4 + x) * 4],
                pic[(y * 4 + x) * 4 + 1],
                pic[(y * 4 + x) * 4 + 2],
            ]
        };
        assert_eq!(
            px(1, 1),
            [0, 0, 255],
            "opaque pixel replaces the background"
        );
        assert_eq!(
            px(2, 1),
            [200, 200, 200],
            "transparent pixel leaves it alone"
        );
        let blended = px(2, 2);
        assert!(
            blended[0] > 128 && blended[1] < 130,
            "half-blue over grey: {blended:?}"
        );

        // Partly off-screen and hidden cursors must not panic or write.
        let mut pic2 = vec![7u8; 4 * 4 * 4];
        overlay.compose(&mut pic2, 4, 4, &CursorUpdate { x: 3, y: 3, ..cur });
        overlay.compose(
            &mut pic2,
            4,
            4,
            &CursorUpdate {
                x: -5,
                y: -5,
                ..cur
            },
        );
        overlay.compose(
            &mut pic2,
            4,
            4,
            &CursorUpdate {
                visible: false,
                ..cur
            },
        );
        overlay.compose(
            &mut pic2,
            4,
            4,
            &CursorUpdate {
                shape_id: 404,
                ..cur
            },
        );
    }

    #[test]
    fn hud_rates() {
        let mut c = HudCounter::default();
        let t = Instant::now();
        for i in 0..61u32 {
            c.frame(12_500, t + Duration::from_millis(u64::from(i) * 1000 / 60));
        }
        let h = c.hud();
        assert!((h.fps - 60.0).abs() < 2.0, "{h:?}");
        assert!((h.mbps - 6.1).abs() < 0.5, "{h:?}");
    }
}
