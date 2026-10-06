//! Plain inbox records shared by native storage and the browser.
use crate::auth;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct Address {
    pub id: i64,
    pub identity_fingerprint: String,
    pub route_id: String,
    pub alias_id: String,
    /// Public address to display, share and copy; uses the saved custom domain for new aliases.
    pub address: String,
    /// This alias's canonical Ahrara token. The server never routes by the custom domain.
    pub routing_address: String,
    /// Configure the external catch-all to deliver to this enabled local Ahrara address.
    pub forwarding_address: String,
    pub label: Option<String>,
    pub created_at: String,
    pub enabled: bool,
}

#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct CustomDomain {
    pub domain: String,
    pub forwarding_address: String,
}

pub fn normalize_domain(value: &str) -> Result<String> {
    let domain = value.trim().to_ascii_lowercase();
    if domain == "ahrara.dev"
        || domain.len() > 253
        || !domain.contains('.')
        || domain.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    {
        bail!("use a custom DNS domain such as example.com (ASCII or punycode)")
    }

    Ok(domain)
}

// Keep the token and its case unchanged even for public custom-domain addresses.
pub(crate) fn address_token(value: &str) -> Result<([u8; 16], [u8; 8], String)> {
    let (token, domain) = value
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("invalid address"))?;
    if !domain.eq_ignore_ascii_case("ahrara.dev") {
        normalize_domain(domain)?;
    }

    auth::decode_address(&format!("{token}@ahrara.dev"))
}

#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct Email {
    pub id: i64,
    pub transfer_id: String,
    pub identity_fingerprint: String,
    pub recipient: String,
    pub envelope_sender: String,
    pub received_at: String,
    #[serde(rename = "from")]
    pub from_header: String,
    #[serde(rename = "to")]
    pub to_header: String,
    pub subject: String,
    pub message_id: Option<String>,
    pub size_bytes: i64,
}

#[derive(Clone, Default, Deserialize, Serialize, schemars::JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Filters {
    pub address: Option<String>,
    pub from_contains: Option<String>,
    pub to_contains: Option<String>,
    pub subject_contains: Option<String>,
    pub after_id: Option<i64>,
    pub limit: Option<u32>,
}

impl Filters {
    pub fn validate(&self) -> Result<()> {
        if self.after_id.is_some_and(|n| n < 0) || self.limit.is_some_and(|n| n == 0 || n > 100) {
            bail!("invalid_argument: invalid pagination: limit 1..100, after_id >= 0")
        }
        if let Some(address) = &self.address {
            address_token(address).map_err(|error| anyhow::anyhow!("invalid_argument: {error}"))?;
        }

        Ok(())
    }
}
