//! The OS credential store, via the `keyring` crate.
//!
//! On Windows this is the Credential Manager (DPAPI-protected, always
//! available). On macOS it is the Keychain. On Linux it is the Secret Service
//! over D-Bus, which needs a running keyring daemon *and* a user session bus —
//! so it works on a desktop and fails over SSH, in a container, or under WSL.

use anyhow::{Context, Result};
use keyring::{Entry, Error as KeyringError};

/// The "service" name the platform's credential manager shows.
pub const SERVICE: &str = "gmail-tui";

/// The outcome of reading the store.
///
/// The distinction between [`Lookup::Missing`] and [`Lookup::Unavailable`] is
/// what makes the fallback work: on Linux the Secret Service is *present* but
/// non-functional when headless, and only a real call can tell us which one we
/// are looking at.
#[derive(Debug)]
pub enum Lookup {
    /// The store works and holds a credential for this account.
    Found(String),
    /// The store works, but nothing is stored for this account yet.
    Missing,
    /// The store cannot be used at all.
    Unavailable(String),
}

/// Read the stored app password.
pub fn lookup(user: &str) -> Result<Lookup> {
    let entry = match Entry::new(SERVICE, user) {
        Ok(entry) => entry,
        Err(err) => return Ok(Lookup::Unavailable(describe(&err))),
    };

    match entry.get_password() {
        Ok(secret) => Ok(Lookup::Found(secret)),
        Err(KeyringError::NoEntry) => Ok(Lookup::Missing),
        Err(err) => Ok(Lookup::Unavailable(describe(&err))),
    }
}

/// Write the app password.
pub fn store(user: &str, secret: &str) -> Result<()> {
    Entry::new(SERVICE, user)
        .context("could not open the OS credential store")?
        .set_password(secret)
        .context("could not write to the OS credential store")
}

/// Delete the app password. Returns `false` when there was nothing to delete.
pub fn remove(user: &str) -> Result<bool> {
    let entry = Entry::new(SERVICE, user).context("could not open the OS credential store")?;
    match entry.delete_credential() {
        Ok(()) => Ok(true),
        Err(KeyringError::NoEntry) => Ok(false),
        Err(err) => Err(err).context("could not delete from the OS credential store"),
    }
}

/// A one-line reason, for the status message shown when we fall back.
fn describe(err: &KeyringError) -> String {
    match err {
        KeyringError::NoStorageAccess(inner) => format!("no storage access: {inner}"),
        KeyringError::PlatformFailure(inner) => format!("platform failure: {inner}"),
        KeyringError::NoDefaultStore => "no default credential store is configured".to_string(),
        KeyringError::NotSupportedByStore(reason) => format!("unsupported by this store: {reason}"),
        other => other.to_string(),
    }
}
