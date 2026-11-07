//! AES-256-GCM sealing, SHA-256 and the default pepper.

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};

pub const TAG_LEN: usize = 16;
pub const KEY_CHECK_AAD: &[u8] = b"nsbundle-keycheck";

const PEPPER_A: [u8; 32] = [
    0x4e, 0x53, 0x42, 0x31, 0x8a, 0x2f, 0xd1, 0x77, 0x03, 0x9c, 0x5e, 0x61, 0xb4, 0x22, 0xf0, 0x18,
    0x6d, 0xa9, 0x3b, 0xc7, 0x14, 0x5f, 0x88, 0xe2, 0x30, 0x0b, 0x97, 0x44, 0xd6, 0x1a, 0x59, 0xc3,
];
const PEPPER_B: [u8; 32] = [
    0x71, 0xe4, 0x0d, 0x9a, 0x56, 0x2c, 0xbf, 0x08, 0xa1, 0x33, 0x7e, 0xd0, 0x49, 0x8c, 0x15, 0xf6,
    0x3d, 0x60, 0xab, 0x24, 0x99, 0x0e, 0x52, 0xc8, 0x7b, 0x1f, 0xde, 0x45, 0x83, 0x6a, 0xf1, 0x27,
];
const PEPPER_C: [u8; 32] = [
    0x92, 0x1b, 0xc5, 0x4a, 0x0f, 0x87, 0x3e, 0xd9, 0x66, 0xaa, 0x21, 0x5c, 0xf3, 0x08, 0x7d, 0x94,
    0x11, 0x4e, 0xb0, 0x8f, 0x3a, 0xc6, 0x02, 0x59, 0xdd, 0x74, 0x1c, 0xa8, 0x60, 0xef, 0x35, 0x8b,
];

/// Shared fallback key, used when the runtime binary carries no per-build key.
pub fn default_key() -> [u8; 32] {
    std::array::from_fn(|i| PEPPER_A[i] ^ PEPPER_B[i] ^ PEPPER_C[i])
}

fn cipher(key: &[u8; 32]) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).expect("32 byte key"))
}

/// Encrypts `data` in place and appends the tag.
pub fn seal(key: &[u8; 32], nonce: [u8; 12], aad: &[u8], data: &mut Vec<u8>) {
    cipher(key)
        .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(aad), data)
        .expect("in-memory AES-GCM can't fail");
}

/// Decrypts ciphertext+tag; `None` if the key is wrong or the data was changed.
pub fn open(key: &[u8; 32], nonce: [u8; 12], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    let mut buf = sealed.to_vec();
    let len = cipher(key)
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad),
            &mut buf,
        )
        .ok()?
        .len();
    buf.truncate(len);
    Some(buf)
}

/// Tag of an empty message: proves a key matches without decrypting an entry.
pub fn key_check(key: &[u8; 32]) -> [u8; TAG_LEN] {
    let mut tag = Vec::new();
    seal(key, [0; 12], KEY_CHECK_AAD, &mut tag);
    tag.try_into().unwrap()
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    ring::digest::digest(&ring::digest::SHA256, data)
        .as_ref()
        .try_into()
        .unwrap()
}

pub fn random<const N: usize>() -> [u8; N] {
    use ring::rand::SecureRandom;
    let mut out = [0u8; N];
    ring::rand::SystemRandom::new()
        .fill(&mut out)
        .expect("the OS random generator is unavailable");
    out
}

pub fn parse_hex_key(hex: &str) -> Option<[u8; 32]> {
    let hex = hex.trim().as_bytes();
    if hex.len() != 64 {
        return None;
    }
    let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = [0u8; 32];
    for (i, pair) in hex.chunks(2).enumerate() {
        out[i] = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(out)
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
