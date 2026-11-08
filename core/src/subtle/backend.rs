//! ring primitives.

use super::Hash;
use ring::rand::SecureRandom;
use ring::{aead, digest, hkdf, hmac, pbkdf2};
use std::num::NonZeroU32;

pub fn random(out: &mut [u8]) -> Result<(), ()> {
    ring::rand::SystemRandom::new().fill(out).map_err(|_| ())
}

pub fn digest(hash: Hash, data: &[u8]) -> Result<Vec<u8>, ()> {
    let algorithm = match hash {
        Hash::Sha1 => &digest::SHA1_FOR_LEGACY_USE_ONLY,
        Hash::Sha256 => &digest::SHA256,
        Hash::Sha384 => &digest::SHA384,
        Hash::Sha512 => &digest::SHA512,
    };
    Ok(digest::digest(algorithm, data).as_ref().to_vec())
}

fn hmac_key(hash: Hash, secret: &[u8]) -> hmac::Key {
    let algorithm = match hash {
        Hash::Sha1 => hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
        Hash::Sha256 => hmac::HMAC_SHA256,
        Hash::Sha384 => hmac::HMAC_SHA384,
        Hash::Sha512 => hmac::HMAC_SHA512,
    };
    hmac::Key::new(algorithm, secret)
}

pub fn hmac_sign(hash: Hash, secret: &[u8], data: &[u8]) -> Result<Vec<u8>, ()> {
    Ok(hmac::sign(&hmac_key(hash, secret), data).as_ref().to_vec())
}

pub fn hmac_verify(hash: Hash, secret: &[u8], data: &[u8], signature: &[u8]) -> bool {
    hmac::verify(&hmac_key(hash, secret), data, signature).is_ok()
}

fn aes_key(secret: &[u8]) -> Result<aead::LessSafeKey, ()> {
    let cipher = match secret.len() {
        16 => &aead::AES_128_GCM,
        32 => &aead::AES_256_GCM,
        _ => return Err(()),
    };
    Ok(aead::LessSafeKey::new(aead::UnboundKey::new(cipher, secret).map_err(|_| ())?))
}

/// ciphertext followed by the 16-byte tag
pub fn aes_gcm_seal(secret: &[u8], iv: &[u8], aad: &[u8], data: &[u8]) -> Result<Vec<u8>, ()> {
    let nonce = aead::Nonce::try_assume_unique_for_key(iv).map_err(|_| ())?;
    let mut buffer = data.to_vec();
    aes_key(secret)?
        .seal_in_place_append_tag(nonce, aead::Aad::from(aad), &mut buffer)
        .map_err(|_| ())?;
    Ok(buffer)
}

pub fn aes_gcm_open(secret: &[u8], iv: &[u8], aad: &[u8], data: &[u8]) -> Result<Vec<u8>, ()> {
    let nonce = aead::Nonce::try_assume_unique_for_key(iv).map_err(|_| ())?;
    let mut buffer = data.to_vec();
    let len = aes_key(secret)?
        .open_in_place(nonce, aead::Aad::from(aad), &mut buffer)
        .map_err(|_| ())?
        .len();
    buffer.truncate(len);
    Ok(buffer)
}

pub fn pbkdf2(hash: Hash, iterations: u32, salt: &[u8], secret: &[u8], out: &mut [u8]) -> Result<(), ()> {
    let algorithm = match hash {
        Hash::Sha1 => pbkdf2::PBKDF2_HMAC_SHA1,
        Hash::Sha256 => pbkdf2::PBKDF2_HMAC_SHA256,
        Hash::Sha384 => pbkdf2::PBKDF2_HMAC_SHA384,
        Hash::Sha512 => pbkdf2::PBKDF2_HMAC_SHA512,
    };
    pbkdf2::derive(algorithm, NonZeroU32::new(iterations).ok_or(())?, salt, secret, out);
    Ok(())
}

struct OkmLength(usize);

impl hkdf::KeyType for OkmLength {
    fn len(&self) -> usize {
        self.0
    }
}

pub fn hkdf(hash: Hash, salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) -> Result<(), ()> {
    let algorithm = match hash {
        Hash::Sha1 => hkdf::HKDF_SHA1_FOR_LEGACY_USE_ONLY,
        Hash::Sha256 => hkdf::HKDF_SHA256,
        Hash::Sha384 => hkdf::HKDF_SHA384,
        Hash::Sha512 => hkdf::HKDF_SHA512,
    };
    let info = [info];
    hkdf::Salt::new(algorithm, salt)
        .extract(ikm)
        .expand(&info, OkmLength(out.len()))
        .and_then(|okm| okm.fill(out))
        .map_err(|_| ())
}
