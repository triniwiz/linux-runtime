/// WTF `StringHasher::computeHashAndMaskTop8Bits`, as used by the generator.
pub fn string_hash(bytes: &[u8]) -> u32 {
    #[inline]
    fn widen(c: u8) -> u32 {
        c as i8 as i32 as u32
    }

    let mut hash: u32 = 0x9E37_79B9;
    let mut chunks = bytes.chunks_exact(2);
    for pair in &mut chunks {
        let (a, b) = (widen(pair[0]), widen(pair[1]));
        hash = hash.wrapping_add(a);
        hash = (hash << 16) ^ ((b << 11) ^ hash);
        hash = hash.wrapping_add(hash >> 11);
    }

    // avalancheBits
    let mut result = hash;
    if let [c] = chunks.remainder() {
        result = result.wrapping_add(widen(*c));
        result ^= result << 11;
        result = result.wrapping_add(result >> 17);
    }
    result ^= result << 3;
    result = result.wrapping_add(result >> 5);
    result ^= result << 2;
    result = result.wrapping_add(result >> 15);
    result ^= result << 10;

    // hashWithTop8BitsMasked
    result &= (1u32 << (32 - 8)) - 1;
    if result == 0 {
        result = 0x8000_0000 >> 8;
    }
    result
}
