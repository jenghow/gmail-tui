//! Passphrase-encrypted credential file.
//!
//! Layout:
//!
//! ```text
//!   magic 6    kdf 12            salt 16   nonce 12   ciphertext ‖ tag 16
//!   "GTUI01"   m_kib‖passes‖lanes  random    random     ChaCha20-Poly1305
//! ```
//!
//! The 32-byte key is `Argon2id(passphrase, salt)` with the cost parameters
//! taken from the header. Carrying them in the file (rather than hard-coding
//! them) means the cost can be raised later without invalidating existing
//! files, and that a debug build reads what a release build wrote.
//!
//! Both the salt and the nonce are regenerated on every write, so the derived
//! key differs every time and nonce reuse is structurally impossible. The
//! header is authenticated as AEAD associated data, so changing the cost
//! parameters, salt or nonce is detected rather than silently deriving a
//! different key.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use zeroize::{Zeroize, Zeroizing};

use crate::fsx::atomic_write;

const MAGIC: &[u8; 6] = b"GTUI01";

/// `memory_kib ‖ passes ‖ lanes`, three big-endian `u32`s.
const KDF_LEN: usize = 12;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const KEY_LEN: usize = 32;

const HEADER_LEN: usize = MAGIC.len() + KDF_LEN + SALT_LEN + NONCE_LEN;

/// Argon2id cost parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Kdf {
    /// Memory, in kibibytes.
    memory_kib: u32,
    /// Iterations.
    passes: u32,
    /// Parallelism.
    lanes: u32,
}

impl Kdf {
    /// What a new credential file is written with.
    const CURRENT: Self = Self {
        memory_kib: 64 * 1024,
        passes: 3,
        lanes: 1,
    };

    /// Cheap enough to keep the test suite quick. Only used by tests.
    #[cfg(test)]
    const TESTING: Self = Self {
        memory_kib: 16,
        passes: 1,
        lanes: 1,
    };

    /// Bounds applied when *reading*: the header is attacker-controlled until
    /// the AEAD verifies it, and an unbounded `memory_kib` would otherwise let
    /// a hostile file make us allocate gigabytes.
    const MAX_MEMORY_KIB: u32 = 1024 * 1024;
    const MAX_PASSES: u32 = 10;
    const MAX_LANES: u32 = 8;

    fn to_bytes(self) -> [u8; KDF_LEN] {
        let mut bytes = [0u8; KDF_LEN];
        bytes[0..4].copy_from_slice(&self.memory_kib.to_be_bytes());
        bytes[4..8].copy_from_slice(&self.passes.to_be_bytes());
        bytes[8..12].copy_from_slice(&self.lanes.to_be_bytes());
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let read = |range: std::ops::Range<usize>| {
            u32::from_be_bytes(bytes[range].try_into().expect("range is four bytes"))
        };
        let kdf = Self {
            memory_kib: read(0..4),
            passes: read(4..8),
            lanes: read(8..12),
        };

        if kdf.lanes == 0
            || kdf.passes == 0
            || kdf.memory_kib < 8 * kdf.lanes
            || kdf.memory_kib > Self::MAX_MEMORY_KIB
            || kdf.passes > Self::MAX_PASSES
            || kdf.lanes > Self::MAX_LANES
        {
            bail!("the credential file declares unusable key-derivation parameters");
        }
        Ok(kdf)
    }
}

/// The encrypted-file credential store.
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Decrypt the stored app password.
    pub fn load(&self, passphrase: &str) -> Result<String> {
        let blob = fs::read(&self.path)
            .with_context(|| format!("could not read {}", self.path.display()))?;

        open(passphrase, &blob).map_err(|err| {
            err.context(format!(
                "could not decrypt {} (wrong passphrase?)",
                self.path.display()
            ))
        })
    }

    /// Encrypt `secret` and replace the file.
    pub fn save(&self, passphrase: &str, secret: &str) -> Result<()> {
        let blob = seal(Kdf::CURRENT, passphrase, secret.as_bytes())?;
        atomic_write(&self.path, &blob, true)
    }

    /// Delete the file. Returns `false` when it was not there.
    pub fn remove(&self) -> Result<bool> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => {
                Err(err).with_context(|| format!("could not delete {}", self.path.display()))
            }
        }
    }
}

/// `magic ‖ kdf ‖ salt ‖ nonce`, used as the file prefix and as AEAD associated
/// data.
fn header(kdf: Kdf, salt: &[u8; SALT_LEN], nonce: &[u8; NONCE_LEN]) -> [u8; HEADER_LEN] {
    let mut header = [0u8; HEADER_LEN];

    let (magic, rest) = header.split_at_mut(MAGIC.len());
    magic.copy_from_slice(MAGIC);

    let (kdf_out, rest) = rest.split_at_mut(KDF_LEN);
    kdf_out.copy_from_slice(&kdf.to_bytes());

    let (salt_out, nonce_out) = rest.split_at_mut(SALT_LEN);
    salt_out.copy_from_slice(salt);
    nonce_out.copy_from_slice(nonce);

    header
}

/// Fill `bytes` from the OS random number generator.
///
/// `getrandom::Error` is not a `std::error::Error`, so it cannot go through
/// `anyhow::Context`.
fn random(bytes: &mut [u8]) -> Result<()> {
    getrandom::fill(bytes).map_err(|err| anyhow!("no source of randomness is available: {err}"))
}

fn derive_key(kdf: Kdf, passphrase: &str, salt: &[u8]) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    let params = Params::new(kdf.memory_kib, kdf.passes, kdf.lanes, Some(KEY_LEN))
        .map_err(|err| anyhow!("invalid key-derivation parameters: {err}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, &mut key[..])
        .map_err(|err| anyhow!("could not derive a key from the passphrase: {err}"))?;
    Ok(key)
}

fn seal(kdf: Kdf, passphrase: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    random(&mut salt)?;
    random(&mut nonce)?;

    let header = header(kdf, &salt, &nonce);
    let key = derive_key(kdf, passphrase, &salt)?;
    let cipher = ChaCha20Poly1305::new(&Key::from(*key));
    let nonce = Nonce::try_from(&nonce[..]).expect("the nonce length is fixed");

    let ciphertext = cipher
        .encrypt(&nonce, Payload { msg: plaintext, aad: &header })
        .map_err(|_| anyhow!("could not encrypt the credential"))?;

    let mut blob = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    blob.extend_from_slice(&header);
    blob.extend_from_slice(&ciphertext);
    Ok(blob)
}

fn open(passphrase: &str, blob: &[u8]) -> Result<String> {
    if blob.len() < HEADER_LEN + TAG_LEN {
        bail!("the credential file is truncated");
    }

    let (header, ciphertext) = blob.split_at(HEADER_LEN);
    if &header[..MAGIC.len()] != MAGIC {
        bail!("the credential file was not written by this version of gmail-tui");
    }

    let kdf = Kdf::from_bytes(&header[MAGIC.len()..MAGIC.len() + KDF_LEN])?;
    let salt = &header[MAGIC.len() + KDF_LEN..MAGIC.len() + KDF_LEN + SALT_LEN];
    let nonce = &header[MAGIC.len() + KDF_LEN + SALT_LEN..];

    let key = derive_key(kdf, passphrase, salt)?;
    let cipher = ChaCha20Poly1305::new(&Key::from(*key));
    let nonce = Nonce::try_from(nonce).expect("the nonce length is fixed");

    let plaintext = cipher
        .decrypt(&nonce, Payload { msg: ciphertext, aad: header })
        .map_err(|_| anyhow!("authentication failed"))?;

    match String::from_utf8(take_bytes(Zeroizing::new(plaintext))) {
        Ok(secret) => Ok(secret),
        Err(err) => {
            // Do not leave the recovered bytes in a discarded buffer.
            let mut bytes = err.into_bytes();
            bytes.zeroize();
            bail!("the decrypted credential was not valid UTF-8")
        }
    }
}

/// Move the allocation out of a [`Zeroizing`] wrapper.
///
/// `zeroize` 1.9 has no `into_inner`, so take the vector and leave an empty one
/// behind; ownership — and therefore the eventual wipe — passes to the caller.
fn take_bytes(mut bytes: Zeroizing<Vec<u8>>) -> Vec<u8> {
    std::mem::take(&mut *bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("gmail-tui-{name}-{}.enc", std::process::id()));
        path
    }

    /// Tests use the cheap parameters so the suite stays quick.
    fn seal_test(passphrase: &str, plaintext: &[u8]) -> Vec<u8> {
        seal(Kdf::TESTING, passphrase, plaintext).unwrap()
    }

    #[test]
    fn round_trips() {
        let blob = seal_test("correct horse battery staple", b"abcdefghijklmnop");
        assert_eq!(open("correct horse battery staple", &blob).unwrap(), "abcdefghijklmnop");
    }

    #[test]
    fn round_trips_unicode() {
        let blob = seal_test("pässphrase", "hänget".as_bytes());
        assert_eq!(open("pässphrase", &blob).unwrap(), "hänget");
    }

    #[test]
    fn rejects_the_wrong_passphrase() {
        let blob = seal_test("right", b"secret");
        assert!(open("wrong", &blob).is_err());
    }

    #[test]
    fn rejects_a_tampered_header() {
        // The salt is covered by the associated data, so flipping a bit must
        // fail authentication rather than deriving a different key.
        let mut blob = seal_test("pass", b"secret");
        blob[MAGIC.len() + KDF_LEN] ^= 0x01;
        assert!(open("pass", &blob).is_err());
    }

    #[test]
    fn rejects_tampered_kdf_parameters() {
        // Raising the cost in the header would otherwise be a memory-usage
        // attack even before the AEAD gets a chance to reject it.
        let mut blob = seal_test("pass", b"secret");
        blob[MAGIC.len()..MAGIC.len() + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(open("pass", &blob).is_err());
    }

    #[test]
    fn rejects_a_tampered_ciphertext() {
        // Flip a bit of the nonce (past magic, kdf and salt).
        let mut blob = seal_test("pass", b"secret");
        blob[MAGIC.len() + KDF_LEN + SALT_LEN] ^= 0x01;
        assert!(open("pass", &blob).is_err());

        let mut blob = seal_test("pass", b"secret");
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        assert!(open("pass", &blob).is_err());
    }

    #[test]
    fn rejects_truncated_and_foreign_files() {
        assert!(open("pass", b"").is_err());
        assert!(open("pass", b"this is not an encrypted credential").is_err());

        let blob = seal_test("pass", b"secret");
        assert!(open("pass", &blob[..blob.len() - 1]).is_err());
    }

    #[test]
    fn uses_a_fresh_salt_and_nonce_every_write() {
        // Same passphrase, same plaintext: key and ciphertext must both differ.
        let first = seal_test("pass", b"same plaintext");
        let second = seal_test("pass", b"same plaintext");
        assert_ne!(first, second);
        assert_ne!(&first[..HEADER_LEN], &second[..HEADER_LEN]);
    }

    #[test]
    fn current_parameters_survive_a_round_trip() {
        // Guards the real cost settings, not just the cheap test ones.
        let blob = seal(Kdf::CURRENT, "pass", b"secret").unwrap();
        assert_eq!(open("pass", &blob).unwrap(), "secret");
    }

    #[test]
    fn store_round_trips_through_a_file() {
        let path = temp_path("store");
        let store = Store::at(path.clone());
        assert!(!store.exists());

        store.save("passphrase", "my-app-password").unwrap();
        assert!(store.exists());

        // The secret must not be recoverable from the bytes on disk.
        let on_disk = fs::read(&path).unwrap();
        assert!(!on_disk.windows(15).any(|window| window == b"my-app-password"));

        assert_eq!(store.load("passphrase").unwrap(), "my-app-password");
        assert!(store.load("nope").is_err());

        assert!(store.remove().unwrap());
        assert!(!store.remove().unwrap());
        assert!(!store.exists());
    }
}
