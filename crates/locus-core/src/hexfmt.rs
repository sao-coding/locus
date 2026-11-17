//! The one hex encoder behind cell display, exports, the Redis dump and the
//! hex viewer (the `hex` crate does the byte work).

/// `0x` + upper-case hex of the first `max` bytes, `…` when cut.
pub fn prefixed(b: &[u8], max: usize) -> String {
    let n = b.len().min(max);
    let mut s = String::with_capacity(3 + n * 2);
    s.push_str("0x");
    s.push_str(&hex::encode_upper(&b[..n]));
    if b.len() > max {
        s.push('…');
    }
    s
}

/// Lower-case hex of all bytes, two digits each.
pub fn lower(b: &[u8]) -> String {
    hex::encode(b)
}

/// The `i`-th byte of a [`lower`] string.
pub fn pair(lower: &str, i: usize) -> Option<&str> {
    lower.get(i * 2..i * 2 + 2)
}

/// Printable ASCII of a dump row, `.` for everything else.
pub fn ascii(chunk: &[u8]) -> String {
    chunk
        .iter()
        .map(|&x| {
            if (0x20..0x7f).contains(&x) {
                x as char
            } else {
                '.'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(prefixed(&[0xab, 0x01], usize::MAX), "0xAB01");
        assert_eq!(prefixed(&[1, 2, 3], 2), "0x0102…");
        assert_eq!(prefixed(&[], 4), "0x");
        assert_eq!(lower(&[0xab, 1]), "ab01");
        assert_eq!(pair("ab01", 1), Some("01"));
        assert_eq!(pair("ab01", 2), None);
        assert_eq!(ascii(b"A\x00~\x7f"), "A.~.");
    }
}
