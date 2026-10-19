//! Argon2id turns the passphrase into a key; XChaCha20-Poly1305 seals the
//! secrets with it. The tag makes a wrong passphrase and a changed file the
//! same, clean error.

use super::BackupError;
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kdf {
    pub m: u32,
    pub t: u32,
    pub p: u32,
}

impl Kdf {
    pub const DEFAULT: Kdf = Kdf {
        m: Params::DEFAULT_M_COST,
        t: Params::DEFAULT_T_COST,
        p: Params::DEFAULT_P_COST,
    };
    /// For tests only: far too weak for real use.
    pub const FAST: Kdf = Kdf { m: 8, t: 1, p: 1 };
}

/// The `[secrets]` table. The parameters travel with the data, so a file
/// made with other costs still opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sealed {
    pub m: u32,
    pub t: u32,
    pub p: u32,
    pub salt: String,
    pub nonce: String,
    pub data: String,
}

/// 1 GiB of memory, in KiB.
const MAX_M: u32 = 1 << 20;
const MAX_T: u32 = 64;
const MAX_P: u32 = 16;

fn key(passphrase: &str, salt: &[u8], kdf: Kdf) -> Result<[u8; 32], BackupError> {
    let params = Params::new(kdf.m, kdf.t, kdf.p, Some(32))
        .map_err(|e| BackupError::Parse(e.to_string()))?;
    let mut key = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| BackupError::Parse(e.to_string()))?;
    Ok(key)
}

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the OS random source is unavailable");
    bytes
}

pub fn seal(plain: &[u8], passphrase: &str, kdf: Kdf) -> Sealed {
    let salt: [u8; 16] = random();
    let nonce: [u8; 24] = random();
    let key = key(passphrase, &salt, kdf).expect("Kdf constants are valid Argon2 parameters");
    let data = XChaCha20Poly1305::new(&Key::from(key))
        .encrypt(&XNonce::from(nonce), plain)
        .expect("encrypting an in-memory buffer cannot fail");
    Sealed {
        m: kdf.m,
        t: kdf.t,
        p: kdf.p,
        salt: STANDARD.encode(salt),
        nonce: STANDARD.encode(nonce),
        data: STANDARD.encode(data),
    }
}

pub fn open(sealed: &Sealed, passphrase: &str) -> Result<Vec<u8>, BackupError> {
    let decode = |s: &str| {
        STANDARD
            .decode(s)
            .map_err(|e| BackupError::Parse(e.to_string()))
    };
    let salt = decode(&sealed.salt)?;
    let nonce: [u8; 24] = decode(&sealed.nonce)?
        .try_into()
        .map_err(|_| BackupError::Parse("the nonce is the wrong length".into()))?;
    let data = decode(&sealed.data)?;
    let kdf = Kdf {
        m: sealed.m,
        t: sealed.t,
        p: sealed.p,
    };
    // Far above what any Bombadil writes, so a changed file cannot make the
    // import allocate gigabytes or spin for hours before the tag check.
    if kdf.m > MAX_M || kdf.t > MAX_T || kdf.p > MAX_P {
        return Err(BackupError::Parse("the key costs are out of range".into()));
    }
    let key = key(passphrase, &salt, kdf)?;
    XChaCha20Poly1305::new(&Key::from(key))
        .decrypt(&XNonce::from(nonce), data.as_ref())
        .map_err(|_| BackupError::WrongPassphrase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_bytes_open_with_the_passphrase() {
        let sealed = seal(b"index/corp = \"s3cret\"", "correct horse", Kdf::FAST);
        assert_eq!(
            open(&sealed, "correct horse").unwrap(),
            b"index/corp = \"s3cret\""
        );
        assert!(!sealed.data.contains("s3cret"));
    }

    #[test]
    fn a_wrong_passphrase_is_refused() {
        let sealed = seal(b"x", "correct horse", Kdf::FAST);
        assert!(matches!(
            open(&sealed, "wrong horse"),
            Err(BackupError::WrongPassphrase)
        ));
    }

    #[test]
    fn a_flipped_byte_is_refused() {
        let mut sealed = seal(b"hello", "correct horse", Kdf::FAST);
        let mut raw = STANDARD.decode(&sealed.data).unwrap();
        raw[0] ^= 1;
        sealed.data = STANDARD.encode(raw);
        assert!(matches!(
            open(&sealed, "correct horse"),
            Err(BackupError::WrongPassphrase)
        ));
    }

    #[test]
    fn two_seals_differ() {
        let a = seal(b"x", "correct horse", Kdf::FAST);
        let b = seal(b"x", "correct horse", Kdf::FAST);
        assert_ne!((a.salt, a.nonce), (b.salt, b.nonce));
    }

    #[test]
    fn garbage_secrets_never_panic() {
        let good = seal(b"x", "correct horse", Kdf::FAST);
        let cases = [
            Sealed {
                data: "!!".into(),
                ..good.clone()
            },
            Sealed {
                nonce: "AAAA".into(),
                ..good.clone()
            },
            Sealed {
                salt: "".into(),
                ..good.clone()
            },
            Sealed {
                m: 0,
                ..good.clone()
            },
            Sealed {
                t: 0,
                ..good.clone()
            },
            // A changed file must not make Bombadil allocate or spin
            // without bound before the tag is even checked.
            Sealed {
                m: 1 << 27,
                ..good.clone()
            },
            Sealed {
                t: u32::MAX,
                ..good.clone()
            },
        ];
        for case in cases {
            assert!(open(&case, "correct horse").is_err());
        }
    }
}
