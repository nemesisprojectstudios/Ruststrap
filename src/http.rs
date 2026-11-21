// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use crate::logger;
use md5::{Digest, Md5};

pub const USER_AGENT: &str = concat!("Ruststrap/", env!("CARGO_PKG_VERSION"));

pub struct Http {
    agent: ureq::Agent,
}

impl Default for Http {
    fn default() -> Self {
        Self::new()
    }
}

impl Http {
    pub fn new() -> Http {
        Http {
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(15))
                .user_agent(USER_AGENT)
                .build(),
        }
    }

    pub fn get_string(&self, url: &str) -> Result<String, String> {
        let resp = self.agent.get(url).call().map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        resp.into_reader()
            .read_to_end(&mut buf)
            .map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// HTTP errors and JSON parse errors are kept apart: the outer error carries
    /// the HTTP status (None for network failures) so callers can react to
    /// private/missing channels.
    pub fn get_json_checked<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<Result<T, String>, (Option<u16>, String)> {
        let resp = self
            .agent
            .get(url)
            .call()
            .map_err(|e| (Self::status_of(&e), e.to_string()))?;
        let mut buf = Vec::new();
        resp.into_reader()
            .read_to_end(&mut buf)
            .map_err(|e| (None, e.to_string()))?;
        let body = String::from_utf8_lossy(&buf).into_owned();
        serde_json::from_str(&body).map_err(|e| (None, format!("invalid JSON from {url}: {e}")))
    }

    fn status_of(err: &ureq::Error) -> Option<u16> {
        match err {
            ureq::Error::Status(code, _) => Some(*code),
            _ => None,
        }
    }

    /// Streams the URL to `dest`, verifying the MD5 `expected` in flight if given.
    /// Retries up to three times on any failure.
    pub fn download(&self, url: &str, dest: &Path, expected: Option<&str>) -> Result<(), String> {
        let mut last_err = String::new();
        for attempt in 1..=3 {
            match self.try_download(url, dest, expected) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = e;
                    logger::warn(&format!(
                        "Download attempt {attempt}/3 failed for {url}: {last_err}"
                    ));
                    std::thread::sleep(Duration::from_secs(attempt as u64));
                }
            }
        }
        Err(last_err)
    }

    fn try_download(&self, url: &str, dest: &Path, expected: Option<&str>) -> Result<(), String> {
        let response = self
            .agent
            .get(url)
            .call()
            .map_err(|e| format!("{e}"))?;

        let mut reader = response.into_reader();
        let mut hasher = Md5::new();
        let mut file = std::fs::File::create(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
        let mut buf = [0u8; 128 * 1024];
        let mut total = 0usize;
        loop {
            let n = reader.read(&mut buf).map_err(|e| format!("{e}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            total += n;
        }
        file.flush().ok();
        let digest = to_hex(&hasher.finalize());

        if let Some(expected) = expected {
            if !digest.eq_ignore_ascii_case(expected) {
                let _ = std::fs::remove_file(dest);
                return Err(format!(
                    "checksum mismatch for {url} (expected {expected}, got {digest})"
                ));
            }
        }
        logger::info(&format!("Downloaded {url} ({total} bytes, md5 {digest})"));
        Ok(())
    }

    pub fn head_ok(&self, url: &str) -> Result<(), String> {
        let _ = self.agent.head(url).call().map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// The "FastHash" used by Roblox package manifests and the stock bootstrapper: uppercase MD5.
pub fn fast_hash_file(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Md5::new();
    let mut buf = [0u8; 128 * 1024];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(to_hex(&hasher.finalize()))
}

pub fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02X}", b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_uppercase() {
        assert_eq!(to_hex(&[0xAB, 0x01]), "AB01");
    }

    #[test]
    fn md5_reference_vector() {
        let mut hasher = Md5::new();
        hasher.update(b"abc");
        assert_eq!(to_hex(&hasher.finalize()), "900150983CD24FB0D6963F7D28E17F72");
    }
}
