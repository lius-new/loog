//! UTF-8 byte offset <-> UTF-16 code unit conversion (design §6.2).
//!
//! Extension ranges are UTF-8 byte offsets; JavaScript strings and LSP
//! positions count UTF-16 units. Offsets inside a character are rejected.

/// UTF-16 offset of `byte`, or `None` when it is out of range or splits a char.
pub fn byte_to_utf16(text: &str, byte: usize) -> Option<usize> {
    if byte > text.len() || !text.is_char_boundary(byte) {
        return None;
    }
    Some(text[..byte].encode_utf16().count())
}

/// Byte offset of the UTF-16 offset `unit`, or `None` when it is out of range
/// or falls between the halves of a surrogate pair.
pub fn utf16_to_byte(text: &str, unit: usize) -> Option<usize> {
    let mut seen = 0;
    for (byte, ch) in text.char_indices() {
        if seen == unit {
            return Some(byte);
        }
        seen += ch.len_utf16();
        if seen > unit {
            return None;
        }
    }
    (seen == unit).then_some(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    // a(1) 中(3) 😀(4) e(1) U+0301(2) \r\n(2) z(1)
    const SAMPLE: &str = "a中😀e\u{301}\r\nz";

    #[test]
    fn converts_cjk_surrogates_combining_and_crlf() {
        assert_eq!(byte_to_utf16(SAMPLE, 0), Some(0));
        assert_eq!(byte_to_utf16(SAMPLE, 1), Some(1));
        assert_eq!(byte_to_utf16(SAMPLE, 4), Some(2));
        assert_eq!(byte_to_utf16(SAMPLE, 8), Some(4));
        assert_eq!(
            byte_to_utf16(SAMPLE, SAMPLE.len()),
            Some(SAMPLE.encode_utf16().count())
        );
    }

    #[test]
    fn rejects_offsets_inside_characters() {
        assert_eq!(byte_to_utf16(SAMPLE, 2), None);
        assert_eq!(byte_to_utf16(SAMPLE, 5), None);
        assert_eq!(utf16_to_byte(SAMPLE, 3), None, "inside the surrogate pair");
        assert_eq!(utf16_to_byte(SAMPLE, 1000), None);
        assert_eq!(byte_to_utf16(SAMPLE, 1000), None);
    }

    #[test]
    fn round_trips_every_boundary() {
        for (byte, _) in SAMPLE
            .char_indices()
            .chain(std::iter::once((SAMPLE.len(), ' ')))
        {
            let unit = byte_to_utf16(SAMPLE, byte).unwrap();
            assert_eq!(utf16_to_byte(SAMPLE, unit), Some(byte));
        }
    }
}
