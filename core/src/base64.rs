//! Base64 for `btoa` / `atob` (WHATWG forgiving-base64).

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

const fn decode_table() -> [u8; 256] {
    let mut t = [0xFF; 256];
    let mut i = 0;
    while i < 64 {
        t[ALPHABET[i] as usize] = i as u8;
        i += 1;
    }
    t
}
static DECODE: [u8; 256] = decode_table();

pub fn encode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len().div_ceil(3) * 4);
    let mut chunks = input.chunks_exact(3);
    for c in &mut chunks {
        let n = (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32;
        out.extend_from_slice(&[
            ALPHABET[(n >> 18) as usize],
            ALPHABET[(n >> 12 & 63) as usize],
            ALPHABET[(n >> 6 & 63) as usize],
            ALPHABET[(n & 63) as usize],
        ]);
    }
    match *chunks.remainder() {
        [a] => out.extend_from_slice(&[
            ALPHABET[(a >> 2) as usize],
            ALPHABET[((a & 3) << 4) as usize],
            b'=',
            b'=',
        ]),
        [a, b] => out.extend_from_slice(&[
            ALPHABET[(a >> 2) as usize],
            ALPHABET[((a & 3) << 4 | b >> 4) as usize],
            ALPHABET[((b & 15) << 2) as usize],
            b'=',
        ]),
        _ => {}
    }
    out
}

/// `None` for input `atob` must reject (InvalidCharacterError).
pub fn decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut data: Vec<u8> = input
        .iter()
        .copied()
        .filter(|c| !matches!(c, b'\t' | b'\n' | b'\x0C' | b'\r' | b' '))
        .collect();
    if data.len() % 4 == 0 {
        let pad = data
            .iter()
            .rev()
            .take(2)
            .take_while(|c| **c == b'=')
            .count();
        data.truncate(data.len() - pad);
    }
    if data.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() / 4 * 3 + 2);
    let mut chunks = data.chunks_exact(4);
    for c in &mut chunks {
        let [a, b, c, d] = [
            DECODE[c[0] as usize],
            DECODE[c[1] as usize],
            DECODE[c[2] as usize],
            DECODE[c[3] as usize],
        ];
        if (a | b | c | d) & 0xC0 != 0 {
            return None;
        }
        let n = (a as u32) << 18 | (b as u32) << 12 | (c as u32) << 6 | d as u32;
        out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
    }
    let rest: Vec<u8> = chunks
        .remainder()
        .iter()
        .map(|c| DECODE[*c as usize])
        .collect();
    if rest.iter().any(|v| v & 0xC0 != 0) {
        return None;
    }
    match rest[..] {
        [a, b] => out.push(a << 2 | b >> 4),
        [a, b, c] => out.extend_from_slice(&[a << 2 | b >> 4, (b & 15) << 4 | c >> 2]),
        _ => {}
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for n in 0..64 {
            let data: Vec<u8> = (0..n).map(|i| (i * 37) as u8).collect();
            assert_eq!(decode(&encode(&data)).unwrap(), data);
        }
        assert_eq!(encode(b"hello"), b"aGVsbG8=");
    }

    #[test]
    fn forgiving() {
        assert_eq!(decode(b" aGVs\nbG8= ").unwrap(), b"hello");
        assert_eq!(decode(b"aGVsbG8").unwrap(), b"hello");
        assert!(decode(b"aGVsbG8==").is_none());
        assert!(decode(b"a").is_none());
        assert!(decode(b"aG!s").is_none());
    }
}
