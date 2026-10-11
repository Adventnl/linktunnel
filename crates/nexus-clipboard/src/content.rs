//! What a clipboard holds, and how it travels.

use nexus_proto::v1::{ClipboardItem, ClipboardUpdate};

use crate::error::ClipError;

/// Largest value synced in one update (text and image together).
pub const MAX_CLIPBOARD_BYTES: usize = 32 << 20;
const MIME_TEXT: &str = "text/plain";
const MIME_PNG: &str = "image/png";
/// Images larger than this many pixels are not decoded (a PNG bomb guard).
const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// A clipboard value: text, an image, or both.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipContent {
    /// UTF-8 text.
    pub text: Option<String>,
    /// A PNG image.
    pub image_png: Option<Vec<u8>>,
}

impl ClipContent {
    /// Text only.
    #[must_use]
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            text: Some(s.into()),
            image_png: None,
        }
    }

    /// Nothing worth syncing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.as_deref().is_none_or(str::is_empty)
            && self.image_png.as_deref().is_none_or(<[u8]>::is_empty)
    }

    /// Payload size in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.text.as_ref().map_or(0, String::len) + self.image_png.as_ref().map_or(0, Vec::len)
    }

    /// Content hash, used to recognise echoes.
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(&[u8::from(self.text.is_some())]);
        if let Some(t) = &self.text {
            h.update(&(t.len() as u64).to_le_bytes());
            h.update(t.as_bytes());
        }
        h.update(&[u8::from(self.image_png.is_some())]);
        if let Some(i) = &self.image_png {
            h.update(&(i.len() as u64).to_le_bytes());
            h.update(i);
        }
        *h.finalize().as_bytes()
    }

    /// The wire form.
    #[must_use]
    pub fn to_update(&self, seq: u64) -> ClipboardUpdate {
        let mut items = Vec::new();
        if let Some(t) = self.text.as_deref().filter(|t| !t.is_empty()) {
            items.push(ClipboardItem {
                mime: MIME_TEXT.into(),
                data: t.as_bytes().to_vec().into(),
            });
        }
        if let Some(i) = self.image_png.as_deref().filter(|i| !i.is_empty()) {
            items.push(ClipboardItem {
                mime: MIME_PNG.into(),
                data: i.to_vec().into(),
            });
        }
        ClipboardUpdate { seq, items }
    }

    /// Validates and converts a received update. Unknown types are ignored; oversized,
    /// non-UTF-8 or non-PNG data rejects the whole update.
    pub fn from_update(u: &ClipboardUpdate) -> Result<Self, ClipError> {
        let total: usize = u.items.iter().map(|i| i.data.len()).sum();
        if total > MAX_CLIPBOARD_BYTES {
            return Err(ClipError::Rejected(format!("{total} bytes is too large")));
        }
        let mut out = Self::default();
        for item in &u.items {
            match item.mime.as_str() {
                MIME_TEXT => {
                    let s = std::str::from_utf8(&item.data)
                        .map_err(|_| ClipError::Rejected("text is not UTF-8".into()))?;
                    out.text = Some(s.to_owned());
                }
                MIME_PNG => {
                    // Parse the header only: enough to refuse non-PNG and absurd sizes.
                    let info = png::Decoder::new(std::io::Cursor::new(&item.data[..]))
                        .read_info()
                        .map_err(|e| ClipError::Rejected(format!("not a PNG: {e}")))?;
                    let i = info.info();
                    if u64::from(i.width) * u64::from(i.height) > MAX_PIXELS {
                        return Err(ClipError::Rejected("image is too large".into()));
                    }
                    out.image_png = Some(item.data.to_vec());
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

/// Encodes straight-alpha RGBA pixels as PNG.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, ClipError> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    enc.write_header()
        .and_then(|mut w| w.write_image_data(rgba))
        .map_err(|e| ClipError::Backend(format!("png encode: {e}")))?;
    Ok(out)
}

/// Decodes a PNG to RGBA, refusing images over [`MAX_PIXELS`] pixels.
pub fn decode_png(data: &[u8]) -> Result<(u32, u32, Vec<u8>), ClipError> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(data));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
    let mut reader = dec
        .read_info()
        .map_err(|e| ClipError::Rejected(format!("not a PNG: {e}")))?;
    let (w, h) = (reader.info().width, reader.info().height);
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(ClipError::Rejected("image is too large".into()));
    }
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    let frame = reader
        .next_frame(&mut buf)
        .map_err(|e| ClipError::Rejected(format!("png decode: {e}")))?;
    buf.truncate(frame.buffer_size());
    let rgba = match frame.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Indexed => {
            return Err(ClipError::Rejected("unexpanded palette".into()));
        }
    };
    Ok((w, h, rgba))
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
mod tests {
    use super::*;

    #[test]
    fn digest_distinguishes_and_roundtrips() {
        let a = ClipContent::text("hello");
        let b = ClipContent::text("hellp");
        assert_ne!(a.digest(), b.digest());
        assert_eq!(a.digest(), ClipContent::text("hello").digest());
        let back = ClipContent::from_update(&a.to_update(1)).unwrap();
        assert_eq!(back, a);
        assert!(ClipContent::default().is_empty());
        assert!(ClipContent::text("").is_empty());
    }

    #[test]
    fn png_roundtrip() {
        let rgba: Vec<u8> = (0..4 * 8 * 4).map(|i| (i * 7) as u8).collect();
        let png = encode_png(8, 4, &rgba).unwrap();
        let (w, h, back) = decode_png(&png).unwrap();
        assert_eq!((w, h), (8, 4));
        assert_eq!(back, rgba);
        let c = ClipContent {
            text: None,
            image_png: Some(png),
        };
        assert_eq!(ClipContent::from_update(&c.to_update(2)).unwrap(), c);
    }

    #[test]
    fn hostile_updates_are_rejected() {
        let bad_text = ClipboardUpdate {
            seq: 1,
            items: vec![ClipboardItem {
                mime: "text/plain".into(),
                data: vec![0xff, 0xfe].into(),
            }],
        };
        assert!(ClipContent::from_update(&bad_text).is_err());
        let not_png = ClipboardUpdate {
            seq: 1,
            items: vec![ClipboardItem {
                mime: "image/png".into(),
                data: b"GIF89a".to_vec().into(),
            }],
        };
        assert!(ClipContent::from_update(&not_png).is_err());
        let huge = ClipboardUpdate {
            seq: 1,
            items: vec![ClipboardItem {
                mime: "text/plain".into(),
                data: vec![b'a'; MAX_CLIPBOARD_BYTES + 1].into(),
            }],
        };
        assert!(ClipContent::from_update(&huge).is_err());
        // Unknown types are ignored, not fatal.
        let unknown = ClipboardUpdate {
            seq: 1,
            items: vec![ClipboardItem {
                mime: "application/x-evil".into(),
                data: vec![1].into(),
            }],
        };
        assert!(ClipContent::from_update(&unknown).unwrap().is_empty());
    }
}
