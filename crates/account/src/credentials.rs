//! API keys from the macOS Keychain.

use std::fmt;
use std::process::Command;

use anyhow::{Context, Result, bail};
use venues::Venue;

/// What a key is allowed to do; each scope is a separate key on the venue.
#[derive(Clone, Copy, Debug)]
pub enum Scope {
    Read,
    /// Spot trading; never with withdrawal rights.
    Trade,
}

impl Scope {
    fn id(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Trade => "trade",
        }
    }
}

/// API key of one venue. Never printed: `Debug` hides every field.
pub struct Credentials {
    pub api_key: String,
    pub secret: String,
    /// OKX only.
    pub passphrase: Option<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials { .. }")
    }
}

impl Credentials {
    /// Reads the key from the macOS Keychain: service `federbot-<venue>-<scope>`,
    /// accounts `api_key`, `secret` and `passphrase` (OKX).
    pub fn load(venue: Venue, scope: Scope) -> Result<Self> {
        let service = format!("federbot-{}-{}", venue.id(), scope.id());
        let field = |account: &str| {
            keychain(&service, account)
                .with_context(|| format!("brak wpisu {service} / {account} w pęku kluczy"))
        };
        Ok(Credentials {
            api_key: field("api_key")?,
            secret: field("secret")?,
            passphrase: match venue {
                Venue::Okx => Some(field("passphrase")?),
                _ => None,
            },
        })
    }
}

/// Another secret from the Keychain, e.g. a notification token.
pub fn secret(service: &str, account: &str) -> Result<String> {
    keychain(service, account)
        .with_context(|| format!("brak wpisu {service} / {account} w pęku kluczy"))
}

/// Via `/usr/bin/security`, which created the items, so macOS doesn't ask again
/// after each rebuild.
fn keychain(service: &str, account: &str) -> Result<String> {
    let output = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .output()
        .context("uruchomienie /usr/bin/security")?;
    if !output.status.success() {
        bail!("security: kod {}", output.status);
    }
    let value = String::from_utf8(output.stdout).context("wpis nie jest tekstem")?;
    Ok(value.trim().to_owned())
}
