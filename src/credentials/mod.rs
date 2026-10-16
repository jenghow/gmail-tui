//! Credential resolution.
//!
//! The app password is obtained from, in order:
//!
//! 1. `GMAIL_PASS`, an explicit override that is never written to disk.
//! 2. The configured backend — OS keyring, encrypted file, or prompt-only.
//! 3. An interactive prompt, which can then be saved to a backend.
//!
//! All of this is synchronous and must run *before* `ratatui::init()`: the
//! keyring may talk to D-Bus, the file backend runs Argon2, and the prompt
//! needs a real terminal rather than the alternate screen.

pub mod file;
pub mod os;

use std::io::IsTerminal;

use anyhow::{Context, Result, bail};
use zeroize::Zeroizing;

use crate::config::{Args, Config, Forget, Store};

/// The shortest passphrase we will accept for the encrypted file.
const MIN_PASSPHRASE: usize = 8;

/// A Gmail app password.
///
/// Deliberately opaque: there is no `Debug` or `Display`, so it cannot be
/// logged by accident, and the bytes are wiped when it is dropped.
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

/// Which backend a run reads from and writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Os,
    File,
    Prompt,
}

/// Obtain the app password for `config.user`.
pub fn resolve(config: &Config, args: &Args) -> Result<Secret> {
    // An exported password overrides everything and is never persisted.
    if let Ok(value) = std::env::var("GMAIL_PASS") {
        let value: String = value.split_whitespace().collect();
        if !value.is_empty() {
            return Ok(Secret::new(value));
        }
    }

    // `--store` applies to this run; otherwise the persisted preference.
    match args.store.unwrap_or(config.store) {
        Store::Prompt => setup(config, Backend::Prompt),

        Store::File => encrypted(config),

        Store::Os => match os::lookup(&config.user)? {
            os::Lookup::Found(secret) => Ok(Secret::new(secret)),
            os::Lookup::Missing => setup(config, Backend::Os),
            os::Lookup::Unavailable(reason) => bail!(
                "--store=os was requested, but the OS credential store is unusable: {reason}"
            ),
        },

        Store::Auto => match os::lookup(&config.user)? {
            os::Lookup::Found(secret) => Ok(Secret::new(secret)),
            os::Lookup::Missing => setup(config, Backend::Os),
            os::Lookup::Unavailable(reason) => {
                eprintln!("gmail-tui: the OS credential store is unavailable ({reason})");
                eprintln!("gmail-tui: falling back to {}", config.secret_path.display());
                encrypted(config)
            }
        },
    }
}

/// Delete a stored credential.
pub fn forget(config: &Config, target: Forget) -> Result<()> {
    let mut removed = false;

    if matches!(target, Forget::Os | Forget::All) {
        match os::remove(&config.user) {
            Ok(true) => {
                println!("Deleted the app password from the OS credential store.");
                removed = true;
            }
            Ok(false) => {}
            // Keep going so that `--forget=all` still clears the file.
            Err(err) => eprintln!("gmail-tui: could not use the OS credential store ({err:#})"),
        }
    }

    if matches!(target, Forget::File | Forget::All) {
        let store = file::Store::at(config.secret_path.clone());
        if store.remove()? {
            println!("Deleted {}.", store.path().display());
            removed = true;
        }
    }

    if !removed {
        println!("Nothing was stored for {}.", config.user);
    }
    Ok(())
}

/// Read the encrypted file, prompting for its passphrase — or set it up when
/// there is no file yet.
fn encrypted(config: &Config) -> Result<Secret> {
    let store = file::Store::at(config.secret_path.clone());
    if !store.exists() {
        return setup(config, Backend::File);
    }

    let passphrase = prompt("Credential file passphrase: ")?;
    let secret = store
        .load(&passphrase)
        .context("could not unlock the credential file")?;
    Ok(Secret::new(secret))
}

/// Ask for the app password once, then persist it to `backend`.
fn setup(config: &Config, backend: Backend) -> Result<Secret> {
    let secret = Secret::new(ask_for_app_password(&config.user)?);

    match backend {
        Backend::Os => {
            os::store(&config.user, secret.expose())
                .context("could not save the app password to the OS credential store")?;
            eprintln!("gmail-tui: saved the app password to the OS credential store.");
        }
        Backend::File => {
            let passphrase = new_passphrase()?;
            file::Store::at(config.secret_path.clone())
                .save(&passphrase, secret.expose())
                .context("could not write the credential file")?;
            eprintln!("gmail-tui: saved the app password to {}", config.secret_path.display());
        }
        Backend::Prompt => {}
    }

    Ok(secret)
}

fn ask_for_app_password(user: &str) -> Result<String> {
    require_terminal("an app password is needed to sign in")?;

    eprintln!("No stored app password for {user}.");
    eprintln!("Create one at https://myaccount.google.com/apppasswords");

    // Google shows app passwords as four space-separated groups, and they are
    // routinely pasted with the spaces still in, which the server rejects.
    let secret: String = prompt("Gmail app password: ")?.split_whitespace().collect();
    if secret.is_empty() {
        bail!("no app password given");
    }
    Ok(secret)
}

fn new_passphrase() -> Result<String> {
    require_terminal("a passphrase is needed to protect the credential file")?;

    eprintln!("Choose a passphrase for the stored credential.");
    eprintln!("You will be asked for it once per launch.");

    let first = prompt("New passphrase: ")?;
    let second = prompt("Repeat passphrase: ")?;
    if first != second {
        bail!("the passphrases did not match");
    }
    if first.chars().count() < MIN_PASSPHRASE {
        bail!("the passphrase must be at least {MIN_PASSPHRASE} characters");
    }
    Ok(first)
}

fn prompt(label: &str) -> Result<String> {
    rpassword::prompt_password(label).with_context(|| format!("could not read {label}"))
}

fn require_terminal(what: &str) -> Result<()> {
    if std::io::stdin().is_terminal() {
        return Ok(());
    }
    bail!(
        "{what}, but stdin is not a terminal, so it cannot be asked for.\n\
         Set GMAIL_PASS for a scripted or headless run."
    );
}
