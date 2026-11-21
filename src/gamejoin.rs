// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

//! Parsing of Roblox launch URIs (`roblox-player:...`, `roblox://...`) and their
//! translation for Sober, which only understands `roblox://experiences/start?...`.

#[derive(Debug, Default, Clone, PartialEq)]
pub enum JoinType {
    #[default]
    Unknown,
    RequestGame,
    RequestGameJob,
    RequestPrivateGame,
    RequestFollowUser,
    RequestPlayTogetherGame,
}

#[derive(Debug, Default, Clone)]
pub struct JoinData {
    pub join_type: JoinType,
    pub place_id: Option<u64>,
    pub job_id: Option<String>,
    pub access_code: Option<String>,
    pub join_origin: Option<String>,
    pub place_launcher_url: Option<String>,
}

/// Query keys that can appear as `key:value` segments inside a `roblox-player:` URI.
const PLAYER_URI_KEYS: [&str; 6] = [
    "roblox-player:",
    "launchmode:",
    "gameinfo:",
    "placelauncherurl:",
    "channel:",
    "time:",
];

#[derive(Debug, Default)]
pub struct LaunchCommand {
    pub mode: LaunchModeKind,
    pub placelauncher_url: Option<String>,
    pub channel: Option<String>,
}

#[derive(Debug, Default, PartialEq)]
pub enum LaunchModeKind {
    #[default]
    Unknown,
    Play,
    GameJob,
    Edit,
}


/// Parses `roblox-player:` URIs. Values are extracted between known key markers
/// rather than split on `+`, because values may contain literal plus signs.
pub fn parse_player_uri(uri: &str) -> Option<LaunchCommand> {
    let lower = uri.to_ascii_lowercase();
    if !lower.starts_with("roblox-player:") {
        return None;
    }

    let mut cmd = LaunchCommand::default();

    // Mark every known key occurrence so segment boundaries can be found.
    let mut marks: Vec<(usize, &str)> = Vec::new();
    for key in PLAYER_URI_KEYS {
        let mut from = 0;
        while let Some(found) = lower[from..].find(key) {
            let abs = from + found;
            marks.push((abs, key));
            from = abs + key.len();
        }
    }
    marks.sort_by_key(|(pos, _)| *pos);

    for (idx, (pos, key)) in marks.iter().enumerate() {
        let value_start = pos + key.len();
        let value_end = marks
            .get(idx + 1)
            .map(|(next_pos, _)| {
                // keys are '+'-separated; strip the separator but keep any raw '+'
                // that belongs to the value itself (base64 gameinfo payloads)
                if *next_pos > 0 && uri.as_bytes().get(next_pos - 1) == Some(&b'+') {
                    next_pos - 1
                } else {
                    *next_pos
                }
            })
            .unwrap_or(uri.len());
        let value = &uri[value_start..value_end.min(uri.len())];

        match *key {
            "launchmode:" => match value.to_ascii_lowercase().as_str() {
                "play" => cmd.mode = LaunchModeKind::Play,
                "gamejob" => cmd.mode = LaunchModeKind::GameJob,
                "edit" => cmd.mode = LaunchModeKind::Edit,
                _ => {}
            },
            "placelauncherurl:" => {
                cmd.placelauncher_url = Some(percent_decode(value));
            }
            "channel:" if !value.is_empty() => {
                cmd.channel = Some(value.to_ascii_lowercase());
            }
            _ => {}
        }
    }

    Some(cmd)
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k.eq_ignore_ascii_case(key) {
            Some(v.to_string())
        } else {
            None
        }
    })
}

/// Extracts join information; the place launcher URL is looked at because the
/// stock URI format wraps all join details inside it.
pub fn get_join_data(launch_command: &str) -> JoinData {
    let mut data = JoinData::default();

    let command = launch_command.trim();
    if command.is_empty() {
        return data;
    }

    if command.starts_with("roblox://") {
        let query = command.split_once('?').map(|(_, q)| q).unwrap_or("");
        if let Some(place_id) = query_value(query, "placeId").and_then(|v| v.parse().ok()) {
            data.join_type = JoinType::RequestGame;
            data.place_id = Some(place_id);
            data.place_launcher_url = Some(command.to_string());
        }
        data.job_id = query_value(query, "gameInstanceId").filter(|v| !v.is_empty());
        data.access_code = query_value(query, "accessCode").filter(|v| !v.is_empty());
        data.join_origin = query_value(query, "joinAttemptOrigin").filter(|v| !v.is_empty());
        return data;
    }

    let Some(cmd) = parse_player_uri(command) else {
        return data;
    };
    let Some(url) = cmd.placelauncher_url else {
        return data;
    };
    data.place_launcher_url = Some(url.clone());

    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    match query_value(query, "request").as_deref() {
        Some("RequestGame") => {
            data.join_type = JoinType::RequestGame;
            data.place_id = query_value(query, "placeId").and_then(|v| v.parse().ok());
            data.join_origin = query_value(query, "joinAttemptOrigin").filter(|v| !v.is_empty());
        }
        Some("RequestGameJob") => {
            data.join_type = JoinType::RequestGameJob;
            data.place_id = query_value(query, "placeId").and_then(|v| v.parse().ok());
            data.job_id = query_value(query, "gameId").filter(|v| !v.is_empty());
            data.join_origin = query_value(query, "joinAttemptOrigin").filter(|v| !v.is_empty());
        }
        Some("RequestPrivateGame") => {
            data.join_type = JoinType::RequestPrivateGame;
            data.place_id = query_value(query, "placeId").and_then(|v| v.parse().ok());
            data.access_code = query_value(query, "accessCode").filter(|v| !v.is_empty());
        }
        Some("RequestFollowUser") => {
            data.join_type = JoinType::RequestFollowUser;
            data.place_id = None;
        }
        Some("RequestPlayTogetherGame") => {
            data.join_type = JoinType::RequestPlayTogetherGame;
            data.place_id = query_value(query, "placeId").and_then(|v| v.parse().ok());
            data.job_id = query_value(query, "conversationId").filter(|v| !v.is_empty());
        }
        _ => {}
    }
    data
}

/// Rebuilds parsed launch data into a `roblox://experiences/start` deeplink, the
/// only launch format Sober understands. Returns None when there is nothing to join.
pub fn build_sober_uri(data: &JoinData) -> Option<String> {
    let place_id = data.place_id.filter(|id| *id > 0)?;
    let is_play_together = data.join_type == JoinType::RequestPlayTogetherGame;

    let mut uri = format!("roblox://experiences/start?placeId={place_id}");
    if let Some(access_code) = &data.access_code {
        uri.push_str(&format!("&accessCode={access_code}"));
    } else if !is_play_together {
        if let Some(job_id) = &data.job_id {
            uri.push_str(&format!("&gameInstanceId={job_id}"));
        }
    }
    Some(uri)
}

/// Converts shortcut data `placeId[;jobId[;accessCode]]` into a `roblox-player:` deeplink.
pub fn game_shortcut_to_deeplink(data: &str) -> Option<String> {
    let mut parts = data.split(';');
    let place_id = parts.next()?.trim();
    if place_id.is_empty() {
        return None;
    }
    let mut deeplink = format!("roblox://experiences/start?placeId={}", percent_encode(place_id));
    let job_id = parts.next().map(str::trim).filter(|v| !v.is_empty());
    let access_code = parts.next().map(str::trim).filter(|v| !v.is_empty());
    if let Some(access_code) = access_code {
        deeplink.push_str(&format!("&accessCode={}", percent_encode(access_code)));
    } else if let Some(job_id) = job_id {
        deeplink.push_str(&format!("&gameInstanceId={}", percent_encode(job_id)));
    }
    Some(deeplink)
}

pub fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(
                    std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"),
                    16,
                ) {
                    out.push(byte);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TYPICAL: &str = "roblox-player:1+launchmode:play+gameinfo:eyJwbGFjZUlkIjoxMjM0NTZ9+placelauncherurl:https%3a%2f%2fwww.roblox.com%2fGame%2fPlaceLauncher.ashx%3frequest%3dRequestGame%26placeId%3d123456%26joinAttemptOrigin%3dfriendServerListJoin+time:1700000000";

    #[test]
    fn parses_typical_player_uri() {
        let cmd = parse_player_uri(TYPICAL).unwrap();
        assert_eq!(cmd.mode, LaunchModeKind::Play);
        assert!(cmd.channel.is_none());
        let url = cmd.placelauncher_url.unwrap();
        assert!(url.starts_with("https://www.roblox.com/Game/PlaceLauncher.ashx?"));
        assert!(url.ends_with("joinAttemptOrigin=friendServerListJoin"));

        let data = get_join_data(TYPICAL);
        assert_eq!(data.join_type, JoinType::RequestGame);
        assert_eq!(data.place_id, Some(123456));
        assert_eq!(data.join_origin.as_deref(), Some("friendServerListJoin"));
    }

    #[test]
    fn parses_gameinfo_with_plus_signs() {
        // base64 payloads may legally contain '+'; values must not be truncated there
        let uri = "roblox-player:1+launchmode:play+gameinfo:eyJwbGFjZUlkIjoxMjM0NTZ9abc+def+placelauncherurl:https%3a%2f%2fwww.roblox.com%2fGame%2fPlaceLauncher.ashx%3frequest%3dRequestGame%26placeId%3d7+time:1";
        let cmd = parse_player_uri(uri).unwrap();
        assert_eq!(cmd.mode, LaunchModeKind::Play);
        assert_eq!(cmd.placelauncher_url.as_deref().map(|u| u.ends_with("placeId=7")), Some(true));
    }

    #[test]
    fn parses_channel() {
        let cmd = parse_player_uri("roblox-player:1+launchmode:play+channel:zcontrol").unwrap();
        assert_eq!(cmd.channel.as_deref(), Some("zcontrol"));
    }

    #[test]
    fn parses_roblox_scheme() {
        let data = get_join_data(
            "roblox://experiences/start?placeId=99&gameInstanceId=abc-123&accessCode=xyz",
        );
        assert_eq!(data.join_type, JoinType::RequestGame);
        assert_eq!(data.place_id, Some(99));
        assert_eq!(data.job_id.as_deref(), Some("abc-123"));
        assert_eq!(data.access_code.as_deref(), Some("xyz"));
    }

    #[test]
    fn parses_private_game_job() {
        let url = "https://www.roblox.com/Game/PlaceLauncher.ashx?request=RequestGameJob&placeId=42&gameId=job-77";
        let cmd = format!("roblox-player:1+launchmode:play+placelauncherurl:{}", percent_encode(url));
        let data = get_join_data(&cmd);
        assert_eq!(data.join_type, JoinType::RequestGameJob);
        assert_eq!(data.place_id, Some(42));
        assert_eq!(data.job_id.as_deref(), Some("job-77"));
    }

    #[test]
    fn sober_uri_translation() {
        let mut data = get_join_data(TYPICAL);
        assert_eq!(
            build_sober_uri(&data).unwrap(),
            "roblox://experiences/start?placeId=123456"
        );

        data.job_id = Some("srv-9".into());
        assert_eq!(
            build_sober_uri(&data).unwrap(),
            "roblox://experiences/start?placeId=123456&gameInstanceId=srv-9"
        );

        let none = build_sober_uri(&JoinData::default());
        assert_eq!(none, None);
    }

    #[test]
    fn shortcut_deeplink() {
        assert_eq!(
            game_shortcut_to_deeplink("12345").unwrap(),
            "roblox://experiences/start?placeId=12345"
        );
        assert_eq!(
            game_shortcut_to_deeplink("12345;jid").unwrap(),
            "roblox://experiences/start?placeId=12345&gameInstanceId=jid"
        );
        assert_eq!(
            game_shortcut_to_deeplink("12345;;ac").unwrap(),
            "roblox://experiences/start?placeId=12345&accessCode=ac"
        );
        assert_eq!(game_shortcut_to_deeplink(""), None);
    }

    #[test]
    fn percent_codecs_roundtrip() {
        let encoded = percent_encode("a b+c&d=e");
        assert_eq!(encoded, "a%20b%2Bc%26d%3De");
        assert_eq!(percent_decode(&encoded), "a b+c&d=e");
    }
}
