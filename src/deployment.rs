// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use serde::Deserialize;

use crate::http::Http;
use crate::logger;
use crate::paths::DEFAULT_CHANNEL;
use crate::state::Settings;

/// Ordered by likely latency, first responsive mirror wins; last is Roblox's S3 origin.
pub const MIRRORS: [&str; 5] = [
    "https://setup.rbxcdn.com",
    "https://setup-ak.rbxcdn.com",
    "https://setup-aws.rbxcdn.com",
    "https://setup-cfly.rbxcdn.com",
    "https://s3.amazonaws.com/setup.roblox.com",
];

#[derive(Clone, Debug, Deserialize)]
pub struct ClientVersion {
    #[serde(rename = "version")]
    pub version: String,
    #[serde(rename = "clientVersionUpload")]
    pub version_guid: String,
}

#[derive(Debug)]
pub enum ChannelError {
    /// Channel exists but is private/internal or has no build for this binary type.
    Invalid(u16),
    /// Connectivity or server failure; retryable.
    Network(String),
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelError::Invalid(code) => write!(f, "channel is not usable (HTTP {code})"),
            ChannelError::Network(e) => write!(f, "{e}"),
        }
    }
}

pub fn is_default_channel(channel: &str) -> bool {
    channel.eq_ignore_ascii_case(DEFAULT_CHANNEL) || channel.eq_ignore_ascii_case("live")
}

/// Probes mirrors in order and returns the first that answers, doubling as the
/// connectivity check. Falls back to the primary mirror address on total failure.
pub fn resolve_base_url(http: &Http) -> String {
    for mirror in MIRRORS {
        match http.head_ok(&format!("{mirror}/versionStudio")) {
            Ok(()) => {
                logger::info(&format!("Using download mirror: {mirror}"));
                return mirror.to_string();
            }
            Err(e) => logger::warn(&format!("Mirror {mirror} unreachable: {e}")),
        }
    }
    logger::warn("No mirrors responded, falling back to default");
    MIRRORS[0].to_string()
}

pub fn location(base_url: &str, channel: &str, resource: &str) -> String {
    if is_default_channel(channel) {
        format!("{base_url}{resource}")
    } else {
        format!("{base_url}/channel/common{resource}")
    }
}

fn clientsettings_url(domain: &str, binary_type: &str, channel: &str, cdn: bool) -> String {
    let service = if cdn { "clientsettingscdn" } else { "clientsettings" };
    let path = if is_default_channel(channel) {
        format!("v2/client-version/{binary_type}")
    } else {
        format!("v2/client-version/{binary_type}/channel/{channel}")
    };
    format!("https://{service}.{domain}/{path}")
}

/// Fetches deployment info for a channel. `Err(Invalid)` means the caller should
/// revert to the default channel; other errors are connectivity failures.
pub fn get_client_version(
    http: &Http,
    settings: &Settings,
    channel: &str,
    binary_type: &str,
) -> Result<ClientVersion, ChannelError> {
    let is_default = is_default_channel(channel);
    logger::info(&format!("Getting deploy info for channel {channel} ({binary_type})"));

    for cdn in [true, false] {
        let url = clientsettings_url(&settings.roblox_domain, binary_type, channel, cdn);
        match http.get_json_checked::<ClientVersion>(&url) {
            Ok(Ok(info)) => return Ok(info),
            Ok(Err(parse_err)) => {
                if cdn {
                    logger::warn(&format!("Malformed response from clientsettingscdn ({parse_err}), falling back to clientsettings"));
                } else {
                    return Err(ChannelError::Network(parse_err));
                }
            }
            Err((status, e)) => {
                if let Some(code) = status {
                    if !is_default && matches!(code, 401 | 403 | 404) {
                        return Err(ChannelError::Invalid(code));
                    }
                }
                if cdn {
                    logger::warn(&format!("Failed to contact clientsettingscdn ({e}), falling back to clientsettings"));
                } else {
                    return Err(ChannelError::Network(e));
                }
            }
        }
    }
    unreachable!()
}

/// A `roblox-player:` URI can carry `channel:xxx`, which enrolls the install
/// into that channel (headless equivalent of Froststrap's automatic mode).
pub fn channel_from_launch_args(launch_args: &str) -> Option<String> {
    if launch_args.is_empty() {
        return None;
    }
    if let Some(pos) = launch_args.to_ascii_lowercase().find("channel:") {
        let rest = &launch_args[pos + "channel:".len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !name.is_empty() {
            return Some(name.to_ascii_lowercase());
        }
    }
    None
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_parsing() {
        assert_eq!(
            channel_from_launch_args("roblox-player:1+launchmode:play+channel:zcontrol"),
            Some("zcontrol".to_string())
        );
        assert_eq!(channel_from_launch_args("roblox-player:1+launchmode:play"), None);
        assert_eq!(channel_from_launch_args(""), None);
    }

    #[test]
    fn default_channels() {
        assert!(is_default_channel("production"));
        assert!(is_default_channel("LIVE"));
        assert!(!is_default_channel("zcanary"));
    }

    #[test]
    fn location_channel_prefix() {
        assert_eq!(
            location("https://setup.rbxcdn.com", "production", "/x-rbxPkgManifest.txt"),
            "https://setup.rbxcdn.com/x-rbxPkgManifest.txt"
        );
        assert_eq!(
            location("https://setup.rbxcdn.com", "zbeta", "/x-rbxPkgManifest.txt"),
            "https://setup.rbxcdn.com/channel/common/x-rbxPkgManifest.txt"
        );
    }
}
