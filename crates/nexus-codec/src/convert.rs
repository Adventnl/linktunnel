//! Colour-space conversion between captured BGRA and codec I420 (BT.709, limited range).

use yuv::{
    YuvChromaSubsampling, YuvConversionMode, YuvPlanarImage, YuvPlanarImageMut, YuvRange,
    YuvStandardMatrix,
};

use crate::CodecError;
use crate::types::{Frame, I420Planes};

const RANGE: YuvRange = YuvRange::Limited;
const MATRIX: YuvStandardMatrix = YuvStandardMatrix::Bt709;

fn yuv_err(e: impl std::fmt::Display) -> CodecError {
    CodecError::Invalid(format!("colour conversion: {e}"))
}

/// Converts a BGRA frame to planar 4:2:0, rejecting inconsistent geometry instead of letting the
/// conversion library panic.
pub fn bgra_to_i420(frame: &Frame) -> Result<I420Planes, CodecError> {
    frame.validate()?;
    let stride =
        u32::try_from(frame.stride).map_err(|_| CodecError::Invalid("stride too large".into()))?;
    // Chroma planes need even dimensions; round up and let the converter replicate the edge.
    let mut planar =
        YuvPlanarImageMut::<u8>::alloc(frame.width, frame.height, YuvChromaSubsampling::Yuv420);
    yuv::bgra_to_yuv420(
        &mut planar,
        &frame.data,
        stride,
        RANGE,
        MATRIX,
        YuvConversionMode::Balanced,
    )
    .map_err(yuv_err)?;
    Ok(I420Planes {
        y: planar.y_plane.borrow().to_vec(),
        u: planar.u_plane.borrow().to_vec(),
        v: planar.v_plane.borrow().to_vec(),
        y_stride: planar.y_stride as usize,
        uv_stride: planar.u_stride as usize,
    })
}

/// Converts planar 4:2:0 to a tightly packed BGRA buffer (`width * 4` bytes per row, alpha 255).
pub fn i420_to_bgra(width: u32, height: u32, planes: &I420Planes) -> Result<Vec<u8>, CodecError> {
    if width == 0 || height == 0 {
        return Err(CodecError::Invalid("zero-sized picture".into()));
    }
    let image = YuvPlanarImage {
        y_plane: &planes.y,
        y_stride: u32::try_from(planes.y_stride)
            .map_err(|_| CodecError::Invalid("stride".into()))?,
        u_plane: &planes.u,
        u_stride: u32::try_from(planes.uv_stride)
            .map_err(|_| CodecError::Invalid("stride".into()))?,
        v_plane: &planes.v,
        v_stride: u32::try_from(planes.uv_stride)
            .map_err(|_| CodecError::Invalid("stride".into()))?,
        width,
        height,
    };
    image
        .check_constraints(YuvChromaSubsampling::Yuv420)
        .map_err(yuv_err)?;
    let mut out = vec![0u8; width as usize * height as usize * 4];
    yuv::yuv420_to_bgra(&image, &mut out, width * 4, RANGE, MATRIX).map_err(yuv_err)?;
    Ok(out)
}

/// Peak signal-to-noise ratio between two tightly packed BGRA images, in dB, over the colour
/// channels only. Identical images give `f64::INFINITY`; images of different sizes give `0.0`.
#[must_use]
pub fn psnr_bgra(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut sum = 0u64;
    let mut count = 0u64;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for c in 0..3 {
            let d = i64::from(pa[c]) - i64::from(pb[c]);
            sum += d.unsigned_abs().pow(2);
            count += 1;
        }
    }
    if sum == 0 {
        return f64::INFINITY;
    }
    let mse = sum as f64 / count as f64;
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use bytes::Bytes;

    use super::*;

    fn gradient(w: u32, h: u32) -> Frame {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&[
                    (x * 255 / w) as u8,
                    (y * 255 / h) as u8,
                    ((x + y) % 256) as u8,
                    255,
                ]);
            }
        }
        Frame {
            width: w,
            height: h,
            stride: (w * 4) as usize,
            format: crate::FrameFormat::Bgra8,
            data: Bytes::from(data),
            captured_us: 0,
            dirty: vec![],
        }
    }

    #[test]
    fn roundtrip_is_close() {
        let f = gradient(64, 48);
        let planes = bgra_to_i420(&f).unwrap();
        assert_eq!(planes.y.len(), 64 * 48);
        let back = i420_to_bgra(64, 48, &planes).unwrap();
        let psnr = psnr_bgra(&f.data, &back);
        assert!(psnr > 38.0, "colour conversion lost too much: {psnr:.1} dB");
    }

    #[test]
    fn white_and_black_map_to_the_limited_range_ends() {
        let mk = |v: u8| Frame {
            data: Bytes::from(vec![v; 16 * 16 * 4]),
            width: 16,
            height: 16,
            stride: 64,
            format: crate::FrameFormat::Bgra8,
            captured_us: 0,
            dirty: vec![],
        };
        let black = bgra_to_i420(&mk(0)).unwrap();
        let white = bgra_to_i420(&mk(255)).unwrap();
        assert!(
            black.y.iter().all(|&y| (14..=18).contains(&y)),
            "limited-range black is ~16"
        );
        assert!(
            white.y.iter().all(|&y| (233..=237).contains(&y)),
            "limited-range white is ~235"
        );
    }

    #[test]
    fn inconsistent_geometry_is_an_error_not_a_panic() {
        let mut f = gradient(32, 32);
        f.stride = 10;
        assert!(bgra_to_i420(&f).is_err());
        let mut f = gradient(32, 32);
        f.data = Bytes::from(vec![0u8; 100]);
        assert!(bgra_to_i420(&f).is_err());
        let mut f = gradient(32, 32);
        f.width = 0;
        assert!(bgra_to_i420(&f).is_err());
        let planes = bgra_to_i420(&gradient(32, 32)).unwrap();
        assert!(
            i420_to_bgra(64, 64, &planes).is_err(),
            "claimed size larger than the planes"
        );
    }

    #[test]
    fn odd_sizes_work() {
        let f = gradient(33, 17);
        let planes = bgra_to_i420(&f).unwrap();
        let back = i420_to_bgra(33, 17, &planes).unwrap();
        assert_eq!(back.len(), 33 * 17 * 4);
        assert!(psnr_bgra(&f.data, &back) > 34.0);
    }

    #[test]
    fn psnr_properties() {
        let a = vec![100u8; 64];
        assert!(psnr_bgra(&a, &a).is_infinite());
        let mut b = a.clone();
        b[0] = 110;
        assert!(psnr_bgra(&a, &b) > 40.0);
        let c = vec![200u8; 64];
        assert!(psnr_bgra(&a, &c) < 20.0);
    }
}
