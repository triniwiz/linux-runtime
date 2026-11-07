//! `long double` <-> `f64` conversions. x86_64 uses the x87 80-bit extended
//! format (stored in 16 bytes), aarch64 uses IEEE binary128.

enum Parts {
    Zero(bool),
    Inf(bool),
    Nan,
    /// value = mant / 2^63 * 2^exp, bit 63 of mant set
    Finite {
        neg: bool,
        exp: i32,
        mant: u64,
    },
}

fn decompose(v: f64) -> Parts {
    let bits = v.to_bits();
    let neg = bits >> 63 == 1;
    let e = ((bits >> 52) & 0x7ff) as i32;
    let f = bits & ((1u64 << 52) - 1);
    match (e, f) {
        (0x7ff, 0) => Parts::Inf(neg),
        (0x7ff, _) => Parts::Nan,
        (0, 0) => Parts::Zero(neg),
        (0, f) => {
            let lz = f.leading_zeros();
            Parts::Finite {
                neg,
                exp: -1074 + (63 - lz as i32),
                mant: f << lz,
            }
        }
        (e, f) => Parts::Finite {
            neg,
            exp: e - 1023,
            mant: (1 << 63) | (f << 11),
        },
    }
}

fn scale(mant: u64, exp: i32, neg: bool) -> f64 {
    // mant / 2^63 * 2^exp, scaled in two steps to avoid intermediate under/overflow
    let k = exp - 63;
    let v = mant as f64 * 2f64.powi(k / 2) * 2f64.powi(k - k / 2);
    if neg {
        -v
    } else {
        v
    }
}

pub fn to_bytes(v: f64) -> [u8; 16] {
    let mut out = [0u8; 16];
    if cfg!(target_arch = "x86_64") || cfg!(target_arch = "x86") {
        let (mant, se): (u64, u16) = match decompose(v) {
            Parts::Zero(neg) => (0, (neg as u16) << 15),
            Parts::Inf(neg) => (1 << 63, ((neg as u16) << 15) | 0x7fff),
            Parts::Nan => (0xC000_0000_0000_0000, 0x7fff),
            Parts::Finite { neg, exp, mant } => (mant, ((neg as u16) << 15) | (exp + 16383) as u16),
        };
        out[..8].copy_from_slice(&mant.to_le_bytes());
        out[8..10].copy_from_slice(&se.to_le_bytes());
    } else {
        let bits: u128 = match decompose(v) {
            Parts::Zero(neg) => (neg as u128) << 127,
            Parts::Inf(neg) => ((neg as u128) << 127) | (0x7fffu128 << 112),
            Parts::Nan => (0x7fffu128 << 112) | (1u128 << 111),
            Parts::Finite { neg, exp, mant } => {
                let frac = ((mant & !(1u64 << 63)) as u128) << 49;
                ((neg as u128) << 127) | (((exp + 16383) as u128) << 112) | frac
            }
        };
        out.copy_from_slice(&bits.to_le_bytes());
    }
    out
}

pub fn from_bytes(b: &[u8; 16]) -> f64 {
    if cfg!(target_arch = "x86_64") || cfg!(target_arch = "x86") {
        let mant = u64::from_le_bytes(b[..8].try_into().unwrap());
        let se = u16::from_le_bytes([b[8], b[9]]);
        let neg = se >> 15 == 1;
        let e = (se & 0x7fff) as i32;
        if e == 0x7fff {
            return if mant << 1 == 0 {
                if neg {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                }
            } else {
                f64::NAN
            };
        }
        if mant == 0 {
            return if neg { -0.0 } else { 0.0 };
        }
        // denormals (e == 0) have exponent 1 - bias
        scale(mant, e.max(1) - 16383, neg)
    } else {
        let bits = u128::from_le_bytes(*b);
        let neg = bits >> 127 == 1;
        let e = ((bits >> 112) & 0x7fff) as i32;
        let frac = bits & ((1u128 << 112) - 1);
        if e == 0x7fff {
            return if frac == 0 {
                if neg {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                }
            } else {
                f64::NAN
            };
        }
        if e == 0 {
            return if neg { -0.0 } else { 0.0 };
        }
        scale((1u64 << 63) | (frac >> 49) as u64, e - 16383, neg)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn roundtrip() {
        for v in [
            0.0,
            -0.0,
            1.0,
            -2.5,
            3.141592653589793,
            1e300,
            -1e-300,
            5e-324,
            f64::MAX,
            f64::MIN_POSITIVE,
        ] {
            assert_eq!(
                super::from_bytes(&super::to_bytes(v)).to_bits(),
                v.to_bits(),
                "{v}"
            );
        }
        assert!(super::from_bytes(&super::to_bytes(f64::NAN)).is_nan());
        assert_eq!(
            super::from_bytes(&super::to_bytes(f64::INFINITY)),
            f64::INFINITY
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn matches_x87_encoding() {
        // 1.5L as x87: mantissa 0xC000000000000000, exponent 0x3FFF
        let b = super::to_bytes(1.5);
        assert_eq!(&b[..10], &[0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0x3F]);
    }
}
