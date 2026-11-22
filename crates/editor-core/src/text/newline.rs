//! Newline search over bytes, eight at a time. The line index reads a block's lines back from
//! the text instead of storing them, so these run over tens of kilobytes per lookup.

const LOW: u64 = 0x0101_0101_0101_0101;
const HIGH: u64 = 0x8080_8080_8080_8080;
const SEVEN: u64 = 0x7f7f_7f7f_7f7f_7f7f;

/// 0x80 in every byte of `word` that is a newline, 0 elsewhere. Exact: no carries between bytes.
#[inline]
fn marks(word: u64) -> u64 {
    let x = word ^ (LOW * b'\n' as u64);
    !(((x & SEVEN) + SEVEN) | x | SEVEN) & HIGH
}

pub(super) fn count(bytes: &[u8]) -> usize {
    let mut chunks = bytes.chunks_exact(8);
    let mut total = 0;
    for chunk in &mut chunks {
        total += marks(u64::from_le_bytes(chunk.try_into().unwrap())).count_ones() as usize;
    }
    total
        + chunks
            .remainder()
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count()
}

pub(super) fn position(bytes: &[u8]) -> Option<usize> {
    let mut chunks = bytes.chunks_exact(8);
    let mut at = 0;
    for chunk in &mut chunks {
        let found = marks(u64::from_le_bytes(chunk.try_into().unwrap()));
        if found != 0 {
            return Some(at + (found.trailing_zeros() / 8) as usize);
        }
        at += 8;
    }
    chunks
        .remainder()
        .iter()
        .position(|&byte| byte == b'\n')
        .map(|offset| at + offset)
}

/// The index of the `n`th newline, counting from zero.
pub(super) fn nth(bytes: &[u8], n: usize) -> Option<usize> {
    let (mut at, mut left) = (0, n);
    loop {
        let found = position(&bytes[at..])?;
        if left == 0 {
            return Some(at + found);
        }
        left -= 1;
        at += found + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_a_byte_by_byte_search_at_every_alignment() {
        let mut seed = 3u64;
        let bytes: Vec<u8> = (0..4096)
            .map(|_| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                match (seed >> 40) % 7 {
                    0 => b'\n',
                    1 => 0x8a,
                    2 => 0x0b,
                    3 => 0xfa,
                    _ => (seed >> 33) as u8,
                }
            })
            .collect();
        for start in 0..17 {
            for end in [start, start + 1, start + 9, 2000, 4096] {
                let slice = &bytes[start..end];
                let expected: Vec<_> = (0..slice.len()).filter(|&i| slice[i] == b'\n').collect();
                assert_eq!(count(slice), expected.len());
                assert_eq!(position(slice), expected.first().copied());
                for (n, &found) in expected.iter().enumerate().take(5) {
                    assert_eq!(nth(slice, n), Some(found));
                }
                assert_eq!(nth(slice, expected.len()), None);
            }
        }
    }
}
