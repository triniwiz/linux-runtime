//! Build-time switches and the bundle key, in a sentinel-marked slot of the runtime binary
//! that the CLI patches in the staged executable (like Electron fuses).
//!
//! Slot (128 bytes): sentinel[32], layout version u8 (1), fuse count u8, fuses[8] ('0' off,
//! '1' on, 'r' reserved), flags u8 (1 key, 2 integrity), reserved[5], key ^ pepper[32],
//! bundle integrity SHA-256[32], reserved[16].

use super::crypto;
use std::sync::OnceLock;

pub const SENTINEL: [u8; 32] = *b"NSFUSE1:7c2b9e41d05f83a6e1b4c97d";
pub const SLOT_LEN: usize = 128;
const VERSION: u8 = 1;
const FLAG_KEY: u8 = 1;
const FLAG_INTEGRITY: u8 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fuse {
    /// app.nsbundle's header/TOC must match the hash in the slot; entries are hash checked
    BundleIntegrity = 0,
    /// no plaintext app folder, nothing inside the app root that isn't in the bundle
    OnlyLoadAppFromBundle = 1,
    /// `--inspect` (DevTools)
    Inspector = 2,
    /// runtime flags, env overrides and dev hooks (`--metadata`, `NS_METADATA`, `-e`, scripts,
    /// SIGUSR1 LiveSync, module lookup above the app folder)
    EnvOverrides = 3,
}

pub const FUSES: [(Fuse, &str); 4] = [
    (Fuse::BundleIntegrity, "bundleIntegrity"),
    (Fuse::OnlyLoadAppFromBundle, "onlyLoadAppFromBundle"),
    (Fuse::Inspector, "inspector"),
    (Fuse::EnvOverrides, "envOverrides"),
];

const fn initial() -> [u8; SLOT_LEN] {
    let mut s = [0u8; SLOT_LEN];
    let mut i = 0;
    while i < 32 {
        s[i] = SENTINEL[i];
        i += 1;
    }
    s[32] = VERSION;
    s[33] = FUSES.len() as u8;
    // defaults of an unpatched (development) runtime
    let defaults = *b"0011rrrr";
    let mut i = 0;
    while i < 8 {
        s[34 + i] = defaults[i];
        i += 1;
    }
    s
}

#[used]
static SLOT: [u8; SLOT_LEN] = initial();

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fuses {
    pub on: [bool; 4],
    pub key: Option<[u8; 32]>,
    pub integrity: Option<[u8; 32]>,
}

impl Fuses {
    pub fn is_on(&self, fuse: Fuse) -> bool {
        self.on[fuse as usize]
    }

    pub fn set(&mut self, fuse: Fuse, on: bool) {
        self.on[fuse as usize] = on;
    }

    /// Everything locked, for a damaged slot.
    fn locked() -> Self {
        Fuses {
            on: [true, true, false, false],
            key: None,
            integrity: None,
        }
    }

    pub fn parse(slot: &[u8]) -> Option<Self> {
        if slot.len() < SLOT_LEN || slot[..32] != SENTINEL || slot[32] != VERSION {
            return None;
        }
        let mut on = [false; 4];
        for (i, v) in on.iter_mut().enumerate() {
            *v = match slot[34 + i] {
                b'0' => false,
                b'1' => true,
                _ => return None,
            };
        }
        let flags = slot[42];
        let pepper = crypto::default_key();
        let key =
            (flags & FLAG_KEY != 0).then(|| std::array::from_fn(|i| slot[48 + i] ^ pepper[i]));
        let integrity = (flags & FLAG_INTEGRITY != 0).then(|| slot[80..112].try_into().unwrap());
        Some(Fuses { on, key, integrity })
    }

    fn write(&self, slot: &mut [u8]) {
        for (i, on) in self.on.iter().enumerate() {
            slot[34 + i] = if *on { b'1' } else { b'0' };
        }
        let pepper = crypto::default_key();
        let key = self.key.unwrap_or([0; 32]);
        for i in 0..32 {
            slot[48 + i] = if self.key.is_some() {
                key[i] ^ pepper[i]
            } else {
                0
            };
        }
        slot[80..112].copy_from_slice(&self.integrity.unwrap_or([0; 32]));
        slot[42] = if self.key.is_some() { FLAG_KEY } else { 0 }
            | if self.integrity.is_some() {
                FLAG_INTEGRITY
            } else {
                0
            };
    }
}

/// This binary's fuses.
pub fn get() -> &'static Fuses {
    static FUSES_: OnceLock<Fuses> = OnceLock::new();
    FUSES_.get_or_init(|| {
        // volatile: the patched value isn't the one the compiler saw
        let slot = unsafe { std::ptr::read_volatile(&SLOT) };
        Fuses::parse(&slot).unwrap_or_else(|| {
            eprintln!("NativeScript: damaged fuse slot, running locked down");
            Fuses::locked()
        })
    })
}

/// Offset of the single fuse slot in a runtime binary.
pub fn find(binary: &[u8]) -> Result<usize, String> {
    let mut found = binary
        .windows(SENTINEL.len())
        .enumerate()
        .filter(|(at, w)| {
            *w == SENTINEL && binary.get(at + 32) == Some(&VERSION) && at + SLOT_LEN <= binary.len()
        })
        .map(|(at, _)| at);
    match (found.next(), found.next()) {
        (Some(at), None) => Ok(at),
        (None, _) => Err("no fuse slot (runtime too old?)".into()),
        _ => Err("more than one fuse slot".into()),
    }
}

pub fn read(binary: &[u8]) -> Result<Fuses, String> {
    let at = find(binary)?;
    Fuses::parse(&binary[at..at + SLOT_LEN]).ok_or_else(|| "damaged fuse slot".into())
}

pub fn write(binary: &mut [u8], fuses: &Fuses) -> Result<(), String> {
    let at = find(binary)?;
    fuses.write(&mut binary[at..at + SLOT_LEN]);
    Ok(())
}
