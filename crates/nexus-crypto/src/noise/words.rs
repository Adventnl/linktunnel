//! 11-bit word encoding over the BIP-39 English list (2048 words).
//!
//! The list is chosen because every word is distinguishable by its first four letters, which
//! makes typing and speaking codes forgiving. The English list is used in *all* UI languages so
//! the two humans comparing codes always see identical words.

use bip39::Language;

/// Bits carried by one word.
pub const WORD_BITS: usize = 11;

/// The word for `index` (taken modulo 2048, so this cannot fail).
#[must_use]
pub fn word(index: u16) -> &'static str {
    let list = Language::English.word_list();
    list[usize::from(index) % list.len()]
}

/// Position of `word` in the list, case-insensitively.
#[must_use]
pub fn index_of(word: &str) -> Option<u16> {
    Language::English.find_word(&word.trim().to_ascii_lowercase())
}

/// Reads `count` big-endian 11-bit groups from `bytes`; `None` if `bytes` is too short.
#[must_use]
pub fn indices_from_bytes(bytes: &[u8], count: usize) -> Option<Vec<u16>> {
    if bytes.len() * 8 < count * WORD_BITS {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut value: u32 = 0;
        for bit in 0..WORD_BITS {
            let pos = i * WORD_BITS + bit;
            let b = (bytes[pos / 8] >> (7 - pos % 8)) & 1;
            value = (value << 1) | u32::from(b);
        }
        // 11 bits always fit in u16.
        out.push(u16::try_from(value).unwrap_or(0));
    }
    Some(out)
}

/// Packs 11-bit indices into bytes (big-endian, zero padded at the end).
#[must_use]
pub fn pack_indices(indices: &[u16]) -> Vec<u8> {
    let mut out = vec![0u8; (indices.len() * WORD_BITS).div_ceil(8)];
    for (i, index) in indices.iter().enumerate() {
        for bit in 0..WORD_BITS {
            let pos = i * WORD_BITS + bit;
            let b = u8::from((index >> (WORD_BITS - 1 - bit)) & 1 == 1);
            out[pos / 8] |= b << (7 - pos % 8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn list_is_the_bip39_english_list() {
        assert_eq!(word(0), "abandon");
        assert_eq!(word(2047), "zoo");
        assert_eq!(index_of("abandon"), Some(0));
        assert_eq!(index_of("  ZOO "), Some(2047));
        assert_eq!(index_of("nexus"), None);
        assert_eq!(index_of(""), None);
    }

    #[test]
    fn word_index_wraps_instead_of_panicking() {
        assert_eq!(word(2048), word(0));
        assert_eq!(word(u16::MAX), word(u16::MAX % 2048));
    }

    #[test]
    fn extraction_known_values() {
        // 0xFF 0xE0 = 11111111111 00000 → first index is 2047.
        assert_eq!(indices_from_bytes(&[0xFF, 0xE0], 1), Some(vec![2047]));
        // 00000000000 00000000001 → bit 21 set → byte 2 = 0b0000_0100 → [0, 1]
        assert_eq!(indices_from_bytes(&[0x00, 0x00, 0x04], 2), Some(vec![0, 1]));
        assert_eq!(
            indices_from_bytes(&[0xFF], 1),
            None,
            "8 bits cannot yield an 11-bit word"
        );
    }

    proptest! {
        #[test]
        fn pack_then_extract_roundtrips(indices in proptest::collection::vec(0u16..2048, 0..16)) {
            let packed = pack_indices(&indices);
            prop_assert_eq!(indices_from_bytes(&packed, indices.len()).unwrap(), indices);
        }
    }
}
