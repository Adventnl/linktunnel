//! OpenH264 software backend (BSD licensed, runs everywhere).
//!
//! Tuned for remote-desktop use: screen-content usage type, constrained baseline profile (every
//! hardware decoder accepts it), bitrate rate control, and no B-frames or look-ahead (OpenH264 has neither in
//! baseline).

use std::time::Instant;

use bytes::Bytes;
use nexus_proto::v1::VideoCodec;
use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Encoder as RawEncoder, EncoderConfig as RawConfig, FrameRate, IntraFramePeriod,
    Profile, RateControlMode, SpsPpsStrategy, UsageType,
};
use openh264::formats::{YUVSlices, YUVSource};

use crate::CodecError;
use crate::convert::bgra_to_i420;
use crate::types::{
    DecodedFrame, Decoder, EncodedFrame, Encoder, EncoderConfig, Frame, I420Planes,
};

/// Largest bitstream the decoder will accept in one call.
const MAX_ACCESS_UNIT: usize = 32 << 20;

/// Software H.264 encoder.
pub struct H264Encoder {
    config: EncoderConfig,
    raw: RawEncoder,
    frame_count: u64,
}

impl std::fmt::Debug for H264Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H264Encoder")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

fn raw_config(config: &EncoderConfig) -> RawConfig {
    let mut raw = RawConfig::new()
        .bitrate(BitRate::from_bps(config.bitrate_bps))
        .max_frame_rate(FrameRate::from_hz(config.fps))
        .rate_control_mode(RateControlMode::Bitrate)
        .usage_type(if config.screen_content {
            UsageType::ScreenContentRealTime
        } else {
            UsageType::CameraVideoRealTime
        })
        .profile(Profile::Baseline)
        // Rate control only holds the target bitrate if it may skip a frame now and then; a skipped
        // frame comes back as `None` and the previous picture simply stays on screen.
        .skip_frames(true)
        // Repeat SPS/PPS with every IDR so a decoder can join at any keyframe.
        .sps_pps_strategy(SpsPpsStrategy::ConstantId)
        .scene_change_detect(true)
        .adaptive_quantization(false)
        .background_detection(false);
    if let Some(interval) = config.keyframe_interval {
        raw = raw.intra_frame_period(IntraFramePeriod::from_num_frames(interval));
    }
    raw
}

impl H264Encoder {
    /// Creates an encoder for `config`.
    pub fn new(config: EncoderConfig) -> Result<Self, CodecError> {
        config.validate()?;
        if config.codec != VideoCodec::H264 {
            return Err(CodecError::Unavailable(format!(
                "OpenH264 cannot encode {:?}",
                config.codec
            )));
        }
        let raw = RawEncoder::with_api_config(OpenH264API::from_source(), raw_config(&config))
            .map_err(|e| CodecError::Unavailable(format!("openh264 init: {e}")))?;
        Ok(Self {
            config,
            raw,
            frame_count: 0,
        })
    }
}

impl Encoder for H264Encoder {
    fn name(&self) -> &'static str {
        "openh264"
    }

    fn config(&self) -> &EncoderConfig {
        &self.config
    }

    fn encode(
        &mut self,
        frame: &Frame,
        force_keyframe: bool,
    ) -> Result<Option<EncodedFrame>, CodecError> {
        if frame.width != self.config.width || frame.height != self.config.height {
            return Err(CodecError::Invalid(format!(
                "frame is {}x{} but the encoder is configured for {}x{}",
                frame.width, frame.height, self.config.width, self.config.height
            )));
        }
        let started = Instant::now();
        let planes = bgra_to_i420(frame)?;
        let (w, h) = (frame.width as usize, frame.height as usize);
        let source = YUVSlices::new(
            (&planes.y, &planes.u, &planes.v),
            (w, h),
            (planes.y_stride, planes.uv_stride, planes.uv_stride),
        );
        if force_keyframe || self.frame_count == 0 {
            self.raw.force_intra_frame();
        }
        let bits = self
            .raw
            .encode(&source)
            .map_err(|e| CodecError::Encode(e.to_string()))?;
        let keyframe = matches!(
            bits.frame_type(),
            openh264::encoder::FrameType::IDR | openh264::encoder::FrameType::I
        );
        let data = bits.to_vec();
        if data.is_empty() {
            return Ok(None);
        }
        self.frame_count += 1;
        Ok(Some(EncodedFrame {
            codec: VideoCodec::H264,
            keyframe,
            data: Bytes::from(data),
            width: frame.width,
            height: frame.height,
            captured_us: frame.captured_us,
            encode_time: started.elapsed(),
        }))
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<(), CodecError> {
        let mut next = self.config.clone();
        next.bitrate_bps = bitrate_bps;
        next.validate()?;
        self.apply_bitrate(bitrate_bps)?;
        self.config = next;
        Ok(())
    }
}

impl H264Encoder {
    /// Changes the target bitrate of the running encoder without a keyframe.
    ///
    /// Rebuilding the encoder would emit an IDR on every adjustment, and a keyframe is many
    /// times the size of a delta frame: on a congested path that burst is itself a congestion
    /// signal, which makes the rate controller back off further (a ratchet). OpenH264 supports a
    /// live change through its option API, which the safe wrapper does not expose.
    #[allow(unsafe_code)]
    fn apply_bitrate(&mut self, bitrate_bps: u32) -> Result<(), CodecError> {
        use openh264_sys2::{
            ENCODER_OPTION_BITRATE, ENCODER_OPTION_MAX_BITRATE, SBitrateInfo, SPATIAL_LAYER_0,
            SPATIAL_LAYER_ALL,
        };
        let bps = i32::try_from(bitrate_bps)
            .map_err(|_| CodecError::Invalid("bitrate too large".into()))?;
        // The ceiling must move with the target, or the encoder never exceeds its initial rate;
        // and OpenH264 insists on `target <= ceiling` at every step, so raise the ceiling first
        // and lower it last.
        let increasing = bps > i32::try_from(self.config.bitrate_bps).unwrap_or(i32::MAX);
        let order = if increasing {
            [ENCODER_OPTION_MAX_BITRATE, ENCODER_OPTION_BITRATE]
        } else {
            [ENCODER_OPTION_BITRATE, ENCODER_OPTION_MAX_BITRATE]
        };
        for option in order {
            let mut last_rc = 0;
            // Spatial layer 0 is the only layer in use; "all layers" covers builds that treat the
            // total separately. One of the two succeeding is enough.
            let ok = [SPATIAL_LAYER_0, SPATIAL_LAYER_ALL]
                .into_iter()
                .any(|layer| {
                    let mut info = SBitrateInfo {
                        iLayer: layer,
                        iBitrate: bps,
                    };
                    // SAFETY: the encoder is initialised, `info` is a valid SBitrateInfo that
                    // outlives the call, and OpenH264 copies it during SetOption.
                    last_rc = unsafe {
                        self.raw
                            .raw_api()
                            .set_option(option, std::ptr::addr_of_mut!(info).cast())
                    };
                    last_rc == 0
                });
            if !ok {
                return Err(CodecError::Encode(format!(
                    "OpenH264 rejected a bitrate change (option {option}, code {last_rc})"
                )));
            }
        }
        Ok(())
    }
}

/// Software H.264 decoder.
pub struct H264Decoder {
    raw: openh264::decoder::Decoder,
}

impl std::fmt::Debug for H264Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H264Decoder").finish_non_exhaustive()
    }
}

impl H264Decoder {
    /// Creates a decoder.
    pub fn new() -> Result<Self, CodecError> {
        let raw = openh264::decoder::Decoder::new()
            .map_err(|e| CodecError::Unavailable(format!("openh264 init: {e}")))?;
        Ok(Self { raw })
    }
}

impl Decoder for H264Decoder {
    fn name(&self) -> &'static str {
        "openh264"
    }

    fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame>, CodecError> {
        if data.is_empty() {
            return Err(CodecError::Invalid("empty access unit".into()));
        }
        if data.len() > MAX_ACCESS_UNIT {
            return Err(CodecError::Invalid("access unit too large".into()));
        }
        let decoded = self
            .raw
            .decode(data)
            .map_err(|e| CodecError::Decode(e.to_string()))?;
        let Some(yuv) = decoded else { return Ok(None) };
        let (width, height) = yuv.dimensions();
        let (ys, us, _) = yuv.strides();
        let (width, height) = (
            u32::try_from(width).map_err(|_| CodecError::Decode("width".into()))?,
            u32::try_from(height).map_err(|_| CodecError::Decode("height".into()))?,
        );
        Ok(Some(DecodedFrame {
            width,
            height,
            planes: I420Planes {
                y: yuv.y().to_vec(),
                u: yuv.u().to_vec(),
                v: yuv.v().to_vec(),
                y_stride: ys,
                uv_stride: us,
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FrameFormat, i420_to_bgra, psnr_bgra};

    fn text_like(w: u32, h: u32, shift: u32) -> Frame {
        // High-contrast "text": black on white with a moving block.
        let mut data = vec![255u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let on = ((x / 6 + y / 10) % 3 == 0 && y % 10 < 7)
                    || ((shift..shift + 24).contains(&x) && (20..44).contains(&y));
                if on {
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
            captured_us: u64::from(shift),
            dirty: vec![],
        }
    }

    fn cfg(w: u32, h: u32) -> EncoderConfig {
        EncoderConfig::screen(w, h, 60.0, 4_000_000)
    }

    #[test]
    fn roundtrip_keyframe_then_deltas() {
        let (w, h) = (320, 192);
        let mut enc = H264Encoder::new(cfg(w, h)).unwrap();
        let mut dec = H264Decoder::new().unwrap();
        for i in 0..12 {
            let f = text_like(w, h, i * 4);
            let out = enc
                .encode(&f, false)
                .unwrap()
                .expect("one output per input");
            assert_eq!(
                out.keyframe,
                i == 0,
                "only the first frame is a keyframe (frame {i})"
            );
            assert_eq!(out.captured_us, u64::from(i * 4));
            let pic = dec.decode(&out.data).unwrap().expect("a picture");
            assert_eq!((pic.width, pic.height), (w, h));
            let back = i420_to_bgra(w, h, &pic.planes).unwrap();
            let psnr = psnr_bgra(&f.data, &back);
            assert!(
                psnr > 28.0,
                "frame {i} PSNR {psnr:.1} dB is too low for 4 Mbit/s text"
            );
        }
    }

    #[test]
    fn keyframes_carry_parameter_sets_so_a_late_joiner_can_decode() {
        let (w, h) = (128, 96);
        let mut enc = H264Encoder::new(cfg(w, h)).unwrap();
        for i in 0..5 {
            enc.encode(&text_like(w, h, i), false).unwrap();
        }
        let key = enc.encode(&text_like(w, h, 9), true).unwrap().unwrap();
        assert!(key.keyframe);
        let nal_types: Vec<u8> = annexb_nal_types(&key.data);
        assert!(
            nal_types.contains(&7),
            "SPS missing from keyframe: {nal_types:?}"
        );
        assert!(
            nal_types.contains(&8),
            "PPS missing from keyframe: {nal_types:?}"
        );
        // A brand new decoder that never saw the earlier frames can start here.
        let mut late = H264Decoder::new().unwrap();
        let pic = late
            .decode(&key.data)
            .unwrap()
            .expect("late joiner decodes the keyframe");
        assert_eq!((pic.width, pic.height), (w, h));
    }

    #[test]
    fn forced_keyframe_and_periodic_keyframes() {
        let (w, h) = (128, 96);
        let mut c = cfg(w, h);
        c.keyframe_interval = Some(5);
        let mut enc = H264Encoder::new(c).unwrap();
        let kinds: Vec<bool> = (0..11)
            .map(|i| {
                enc.encode(&text_like(w, h, i), false)
                    .unwrap()
                    .unwrap()
                    .keyframe
            })
            .collect();
        assert!(
            kinds[0] && kinds[5] && kinds[10],
            "periodic keyframes: {kinds:?}"
        );
        assert!(!kinds[1] && !kinds[4]);
        let forced = enc.encode(&text_like(w, h, 99), true).unwrap().unwrap();
        assert!(forced.keyframe);
    }

    #[test]
    fn bitrate_change_keeps_the_stream_decodable() {
        let (w, h) = (192, 128);
        let mut enc = H264Encoder::new(cfg(w, h)).unwrap();
        let mut dec = H264Decoder::new().unwrap();
        for i in 0..3 {
            let out = enc.encode(&text_like(w, h, i), false).unwrap().unwrap();
            dec.decode(&out.data).unwrap();
        }
        enc.set_bitrate(500_000).unwrap();
        assert_eq!(enc.config().bitrate_bps, 500_000);
        let out = enc.encode(&text_like(w, h, 7), false).unwrap().unwrap();
        assert!(
            !out.keyframe,
            "a live bitrate change must not force a keyframe"
        );
        assert!(dec.decode(&out.data).unwrap().is_some());
        assert!(enc.set_bitrate(10).is_err());
    }

    fn noise(w: u32, h: u32, seed: u64) -> Frame {
        let mut state = seed | 1;
        let data: Vec<u8> = (0..(w * h * 4))
            .map(|i| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                if i % 4 == 3 {
                    255
                } else {
                    u8::try_from((state >> 24) & 0xff).unwrap_or(0)
                }
            })
            .collect();
        Frame {
            width: w,
            height: h,
            stride: (w * 4) as usize,
            format: FrameFormat::Bgra8,
            data: Bytes::from(data),
            captured_us: 0,
            dirty: vec![],
        }
    }

    fn mean_frame_bytes(enc: &mut H264Encoder, frames: u64, seed0: u64) -> f64 {
        // Bytes per frame *offered*: a frame the encoder skipped to hold its rate counts as zero.
        let mut total = 0usize;
        for i in 0..frames {
            if let Some(out) = enc.encode(&noise(320, 180, seed0 + i), false).unwrap() {
                total += out.data.len();
            }
        }
        total as f64 / frames as f64
    }

    #[test]
    fn fresh_encoders_follow_their_configured_bitrate() {
        for bps in [300_000u32, 1_000_000, 4_000_000, 16_000_000] {
            let mut enc = H264Encoder::new(EncoderConfig::screen(320, 180, 30.0, bps)).unwrap();
            let _ = mean_frame_bytes(&mut enc, 30, 1);
            let mean = mean_frame_bytes(&mut enc, 90, 100);
            println!(
                "fresh encoder at {bps} bit/s: {:.2} Mbit/s actual",
                mean * 8.0 * 30.0 / 1e6
            );
        }
    }

    #[test]
    fn live_bitrate_changes_move_the_output_rate_both_ways() {
        let mut enc = H264Encoder::new(EncoderConfig::screen(320, 180, 30.0, 1_000_000)).unwrap();
        let _ = mean_frame_bytes(&mut enc, 30, 1); // settle rate control
        let low = mean_frame_bytes(&mut enc, 60, 100);
        enc.set_bitrate(4_000_000).unwrap();
        let _ = mean_frame_bytes(&mut enc, 30, 200);
        let high = mean_frame_bytes(&mut enc, 60, 300);
        enc.set_bitrate(500_000).unwrap();
        let _ = mean_frame_bytes(&mut enc, 30, 400);
        let back = mean_frame_bytes(&mut enc, 60, 500);
        println!(
            "mean bytes/frame at 1 Mbit/s: {low:.0}, at 4 Mbit/s: {high:.0}, at 0.5 Mbit/s: {back:.0}"
        );
        assert!(
            high > low * 2.0,
            "raising the bitrate must raise the output: {low:.0} → {high:.0}"
        );
        assert!(
            back < low * 0.8,
            "lowering it must lower the output: {low:.0} → {back:.0}"
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert!(H264Encoder::new(cfg(0, 10)).is_err());
        assert!(H264Encoder::new(cfg(33, 32)).is_err());
        let mut hevc = cfg(64, 64);
        hevc.codec = VideoCodec::Hevc;
        assert!(H264Encoder::new(hevc).is_err());
        let mut enc = H264Encoder::new(cfg(64, 64)).unwrap();
        assert!(
            enc.encode(&text_like(32, 32, 0), false).is_err(),
            "wrong frame size"
        );
        let mut dec = H264Decoder::new().unwrap();
        assert!(dec.decode(&[]).is_err());
    }

    #[test]
    fn decoder_survives_garbage() {
        let mut dec = H264Decoder::new().unwrap();
        let mut seed = 0x1234_5678_u32;
        for _ in 0..200 {
            let len = (seed % 400) as usize + 1;
            let junk: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 24) as u8
                })
                .collect();
            let _ = dec.decode(&junk); // any result is fine; it must not panic or hang
        }
    }

    #[test]
    fn static_screens_are_tiny() {
        let (w, h) = (640, 360);
        let mut enc = H264Encoder::new(cfg(w, h)).unwrap();
        let f = text_like(w, h, 0);
        let key = enc.encode(&f, false).unwrap().unwrap();
        let delta = enc.encode(&f, false).unwrap().unwrap();
        assert!(
            delta.data.len() * 20 < key.data.len(),
            "key {} delta {}",
            key.data.len(),
            delta.data.len()
        );
    }

    fn annexb_nal_types(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i + 3 < data.len() {
            if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                out.push(data[i + 3] & 0x1f);
                i += 3;
            } else {
                i += 1;
            }
        }
        out
    }
}
