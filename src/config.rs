//! Configuration, CLI arguments and on-disk locations.
//!
//! Everything here runs *before* the terminal enters raw mode, because both the
//! config file and the credential prompt may need to talk to the user.

use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

use crate::fsx::atomic_write;

/// Directory created under the platform's config root.
const APP_DIR: &str = "gmail-tui";

/// Where a stored credential should live.
///
/// `Auto` probes the OS keyring at runtime and falls back to the encrypted
/// file when the store is unusable. Headless Linux, containers and WSL all
/// report a *present* but non-functional keyring, so only a real round-trip can
/// tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Store {
    #[default]
    Auto,
    Os,
    File,
    Prompt,
}

impl Store {
    fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "os" => Ok(Self::Os),
            "file" => Ok(Self::File),
            "prompt" => Ok(Self::Prompt),
            other => {
                bail!("unknown credential store `{other}`; expected auto, os, file or prompt")
            }
        }
    }
}

/// What `--forget` should erase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forget {
    Os,
    File,
    All,
}

impl Forget {
    fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "os" => Ok(Self::Os),
            "file" => Ok(Self::File),
            "all" => Ok(Self::All),
            other => bail!("unknown --forget target `{other}`; expected os, file or all"),
        }
    }
}

pub const HELP: &str = "\
gmail-tui — a minimal Gmail client for the terminal

USAGE:
    gmail-tui [OPTIONS]

OPTIONS:
    --user <EMAIL>        Gmail address to sign in as (remembered in config.toml)
    --store <BACKEND>     Where to keep the app password (remembered):
                            auto   (default) OS keyring, else encrypted file
                            os     OS keyring only (Windows Credential Manager,
                                   macOS Keychain, Linux Secret Service)
                            file   passphrase-encrypted file only
                            prompt ask every launch, store nothing
    --forget[=<TARGET>]   Delete the stored credential and exit
                            os | file | all (default: all)
    -h, --help            Show this message

ENVIRONMENT:
    GMAIL_USER            Gmail address, when config.toml has none
    GMAIL_PASS            App password for this run only (never written to disk)

FILES:
    config.toml           Gmail address and backend preference
    credentials.enc       App password, encrypted with a passphrase
";

/// Command-line arguments.
///
/// Four flags do not justify a parser dependency, so this is parsed by hand.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub user: Option<String>,
    pub store: Option<Store>,
    pub forget: Option<Forget>,
    pub help: bool,
}

impl Args {
    pub fn parse<I>(argv: I) -> Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let mut args = Self::default();
        let mut argv = argv.into_iter().skip(1); // program name

        while let Some(arg) = argv.next() {
            let (flag, inline) = match arg.split_once('=') {
                Some((flag, value)) => (flag, Some(value.to_string())),
                None => (arg.as_str(), None),
            };

            match flag {
                "-h" | "--help" => args.help = true,
                "--user" => args.user = Some(value_of(&mut argv, inline, flag)?),
                "--store" => {
                    args.store = Some(Store::parse(&value_of(&mut argv, inline, flag)?)?);
                }
                "--forget" => {
                    args.forget = Some(match inline {
                        Some(target) => Forget::parse(&target)?,
                        None => Forget::All,
                    });
                }
                other => bail!("unrecognised argument `{other}` (try --help)"),
            }
        }

        Ok(args)
    }
}

/// Accepts `--flag=value` or `--flag value`.
fn value_of(
    argv: &mut impl Iterator<Item = String>,
    inline: Option<String>,
    flag: &str,
) -> Result<String> {
    if let Some(value) = inline.filter(|value| !value.is_empty()) {
        return Ok(value);
    }
    argv.next()
        .with_context(|| format!("{flag} requires a value"))
}

/// The non-secret half of the configuration, as persisted to `config.toml`.
#[derive(Debug, Serialize, Deserialize)]
struct FileConfig {
    user: String,
    #[serde(default)]
    credential_store: Store,
}

/// Everything the rest of the program needs to know about where it runs.
#[derive(Debug)]
pub struct Config {
    /// Gmail address to authenticate as.
    pub user: String,
    /// Which credential backend to use.
    pub store: Store,
    /// `config.toml`.
    pub path: PathBuf,
    /// `credentials.enc`, the encrypted-file fallback.
    pub secret_path: PathBuf,
}

impl Config {
    /// Locate the paths, read `config.toml` and work out who we are logging in
    /// as, prompting for an address on first run.
    pub fn load(args: &Args) -> Result<Self> {
        let base = BaseDirs::new().context("could not locate the OS config directory")?;
        let config_dir = base.config_dir().join(APP_DIR);

        // Roaming profiles are synchronised between machines; a credential blob
        // has no business travelling with them, so prefer the local root where
        // the platform distinguishes the two.
        #[cfg(windows)]
        let secret_dir = base.data_local_dir().join(APP_DIR);
        #[cfg(not(windows))]
        let secret_dir = config_dir.clone();

        let path = config_dir.join("config.toml");
        let secret_path = secret_dir.join("credentials.enc");

        let stored = read_file(&path)?;

        let user = match (&args.user, stored.as_ref(), env_user()) {
            (Some(user), _, _) => user.trim().to_string(),
            (None, Some(stored), _) => stored.user.clone(),
            (None, None, Some(user)) => user,
            (None, None, None) => prompt_user()?,
        };
        if user.is_empty() {
            bail!("the Gmail address must not be empty");
        }

        let store = args
            .store
            .or_else(|| stored.as_ref().map(|stored| stored.credential_store))
            .unwrap_or_default();

        // Persist the address the first time we learn it, and make an explicit
        // `--store` sticky: otherwise a second run would default back to `auto`,
        // ignore an existing credential file, and ask for the password again.
        let changed = match &stored {
            Some(stored) => stored.user != user || stored.credential_store != store,
            None => true,
        };

        let config = Self {
            user,
            store,
            path,
            secret_path,
        };
        if changed {
            config.save()?;
        }

        Ok(config)
    }

    fn save(&self) -> Result<()> {
        let file = FileConfig {
            user: self.user.clone(),
            credential_store: self.store,
        };
        let text = toml::to_string_pretty(&file).context("could not serialise config.toml")?;
        atomic_write(&self.path, text.as_bytes(), false)
            .with_context(|| format!("could not write {}", self.path.display()))
    }
}

fn read_file(path: &Path) -> Result<Option<FileConfig>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("could not read {}", path.display()));
        }
    };

    let parsed = toml::from_str(&text)
        .with_context(|| format!("could not parse {}", path.display()))?;
    Ok(Some(parsed))
}

fn env_user() -> Option<String> {
    env::var("GMAIL_USER")
        .ok()
        .map(|user| user.trim().to_string())
        .filter(|user| !user.is_empty())
}

fn prompt_user() -> Result<String> {
    if !io::stdin().is_terminal() {
        bail!(
            "no Gmail address configured and stdin is not a terminal.\n\
             Pass one with `gmail-tui --user you@gmail.com` or set GMAIL_USER."
        );
    }

    print!("Gmail address: ");
    io::stdout().flush().ok();

    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("could not read the Gmail address")?;

    let user = line.trim().to_string();
    if user.is_empty() {
        bail!("no Gmail address given");
    }
    Ok(user)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(argv: &[&str]) -> Result<Args> {
        Args::parse(argv.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn parses_flags_in_both_forms() {
        let parsed = args(&["gmail-tui", "--user", "a@b.com", "--store=file"]).unwrap();
        assert_eq!(parsed.user.as_deref(), Some("a@b.com"));
        assert_eq!(parsed.store, Some(Store::File));
        assert_eq!(parsed.forget, None);
        assert!(!parsed.help);
    }

    #[test]
    fn equals_form_works_for_every_valued_flag() {
        let parsed = args(&["gmail-tui", "--user=a@b.com", "--store=os", "--forget=all"]).unwrap();
        assert_eq!(parsed.user.as_deref(), Some("a@b.com"));
        assert_eq!(parsed.store, Some(Store::Os));
        assert_eq!(parsed.forget, Some(Forget::All));
    }

    #[test]
    fn bare_forget_means_everything() {
        assert_eq!(args(&["gmail-tui", "--forget"]).unwrap().forget, Some(Forget::All));
    }

    #[test]
    fn help_is_recognised() {
        assert!(args(&["gmail-tui", "-h"]).unwrap().help);
        assert!(args(&["gmail-tui", "--help"]).unwrap().help);
    }

    #[test]
    fn rejects_unknown_flags_and_values() {
        assert!(args(&["gmail-tui", "--nope"]).is_err());
        assert!(args(&["gmail-tui", "--store=floppy"]).is_err());
        assert!(args(&["gmail-tui", "--user"]).is_err());
    }

    #[test]
    fn store_round_trips_through_toml() {
        let file = FileConfig {
            user: "a@b.com".into(),
            credential_store: Store::File,
        };
        let text = toml::to_string_pretty(&file).unwrap();
        let parsed: FileConfig = toml::from_str(&text).unwrap();
        assert_eq!(parsed.user, "a@b.com");
        assert_eq!(parsed.credential_store, Store::File);
    }

    #[test]
    fn missing_credential_store_defaults_to_auto() {
        let parsed: FileConfig = toml::from_str("user = \"a@b.com\"\n").unwrap();
        assert_eq!(parsed.credential_store, Store::Auto);
    }
}
