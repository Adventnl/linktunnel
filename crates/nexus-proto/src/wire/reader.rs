use super::WireError;

/// Bounds-checked cursor over a byte slice. Every accessor either returns the value or a
/// [`WireError::Truncated`]; nothing here can panic or read out of range.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Starts reading at the beginning of `buf`.
    #[must_use]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bytes not yet consumed.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Bytes consumed so far.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// Takes the next `n` bytes.
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        if self.remaining() < n {
            return Err(WireError::Truncated {
                needed: n - self.remaining(),
            });
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.bytes(N)?);
        Ok(out)
    }

    /// Reads one byte.
    pub fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.array::<1>()?[0])
    }

    /// Reads a signed byte.
    pub fn i8(&mut self) -> Result<i8, WireError> {
        Ok(i8::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian `u16`.
    pub fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian `u32`.
    pub fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian `i32`.
    pub fn i32(&mut self) -> Result<i32, WireError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian `u64`.
    pub fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    /// Succeeds only if every byte was consumed.
    pub fn finish(self) -> Result<(), WireError> {
        match self.remaining() {
            0 => Ok(()),
            n => Err(WireError::Trailing(n)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian() {
        let mut r = Reader::new(&[
            1, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12, 0xff, 0xff, 0xff, 0xff,
        ]);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.u16().unwrap(), 0x1234);
        assert_eq!(r.u32().unwrap(), 0x1234_5678);
        assert_eq!(r.i32().unwrap(), -1);
        assert!(r.finish().is_ok());
    }

    #[test]
    fn truncation_reports_missing_bytes() {
        let mut r = Reader::new(&[1, 2, 3]);
        assert_eq!(r.u32().unwrap_err(), WireError::Truncated { needed: 1 });
        // A failed read consumes nothing.
        assert_eq!(r.remaining(), 3);
    }

    #[test]
    fn trailing_bytes_are_an_error() {
        let mut r = Reader::new(&[1, 2]);
        r.u8().unwrap();
        assert_eq!(r.finish().unwrap_err(), WireError::Trailing(1));
    }
}
