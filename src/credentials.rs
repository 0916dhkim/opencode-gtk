use std::fmt;

use anyhow::{bail, Context, Result};
use keyring::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize};
use url::Url;

const KEYRING_SERVICE: &str = "ai.opencode.Gtk.cloudflare-access";
const STORED_VERSION: u8 = 1;

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
pub struct CloudflareAccessCredentials {
    pub client_id: String,
    pub client_secret: String,
}

impl CloudflareAccessCredentials {
    pub fn new(client_id: String, client_secret: String) -> Result<Self> {
        let client_id = client_id.trim().to_owned();
        let client_secret = client_secret.trim().to_owned();
        if client_id.is_empty() || client_secret.is_empty() {
            bail!("Cloudflare Access client ID and secret are both required");
        }
        Ok(Self {
            client_id,
            client_secret,
        })
    }
}

impl fmt::Debug for CloudflareAccessCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CloudflareAccessCredentials")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

#[derive(Deserialize, Serialize)]
struct StoredCredentials {
    version: u8,
    credentials: CloudflareAccessCredentials,
}

pub fn load(server: &str) -> Result<Option<CloudflareAccessCredentials>> {
    let entry = entry(server)?;
    let stored = match entry.get_password() {
        Ok(stored) => stored,
        Err(KeyringError::NoEntry) => return Ok(None),
        Err(error) => return Err(error).context("failed to read Cloudflare Access credentials"),
    };
    decode(&stored).map(Some)
}

pub fn save(server: &str, credentials: &CloudflareAccessCredentials) -> Result<()> {
    let stored = serde_json::to_string(&StoredCredentials {
        version: STORED_VERSION,
        credentials: credentials.clone(),
    })
    .context("failed to encode Cloudflare Access credentials")?;
    entry(server)?
        .set_password(&stored)
        .context("failed to save Cloudflare Access credentials")
}

pub fn remove(server: &str) -> Result<()> {
    match entry(server)?.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(error) => Err(error).context("failed to remove Cloudflare Access credentials"),
    }
}

fn entry(server: &str) -> Result<Entry> {
    Entry::new(KEYRING_SERVICE, &server_account(server)?)
        .context("failed to open the system keyring")
}

fn server_account(server: &str) -> Result<String> {
    let mut url = Url::parse(server.trim()).context("invalid OpenCode server URL")?;
    url.set_fragment(None);
    url.set_query(None);
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn decode(stored: &str) -> Result<CloudflareAccessCredentials> {
    let stored: StoredCredentials =
        serde_json::from_str(stored).context("Cloudflare Access keyring entry is invalid")?;
    if stored.version != STORED_VERSION {
        bail!("Cloudflare Access keyring entry uses an unsupported version");
    }
    CloudflareAccessCredentials::new(
        stored.credentials.client_id,
        stored.credentials.client_secret,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_credentials_round_trip_without_debugging_the_secret() {
        let credentials =
            CloudflareAccessCredentials::new("client.access".into(), "super-secret".into())
                .unwrap();
        let stored = serde_json::to_string(&StoredCredentials {
            version: STORED_VERSION,
            credentials: credentials.clone(),
        })
        .unwrap();

        assert_eq!(decode(&stored).unwrap(), credentials);
        assert!(!format!("{credentials:?}").contains("super-secret"));
    }

    #[test]
    fn server_account_is_canonical_and_secret_free() {
        assert_eq!(
            server_account("https://OpenCode.Example.com/").unwrap(),
            "https://opencode.example.com"
        );
    }
}
