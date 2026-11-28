use std::{collections::HashMap, env, fs, time::Duration};

const DEFAULT_SYNC_SECS: u64 = 600;
const MAX_SYNC_SECS: u64 = 86_400;
const BASE_PORT: u32 = 3000;

pub struct Config {
    pub token: String,
    pub supabase_url: String,
    pub supabase_key: String,
    pub report_channel: String,
    pub correct_emoji: String,
    pub wrong_emoji: String,
    /// Web port. Defaults to `3000 + shard_id` so shards on one host never collide.
    pub port: u16,
    /// This process serves the guilds whose Discord shard is `shard_id` (of `shard_count`).
    pub shard_id: u32,
    pub shard_count: u32,
    /// How often RAM is re-read from Supabase (`SYNC_INTERVAL_SECS`).
    pub sync_interval: Duration,
    pub ai: AiConfig,
}

pub struct AiConfig {
    pub enabled: bool,
    pub base: String,
    pub key: String,
    pub model: String,
    pub prompt: String,
    pub timeout: Duration,
}

impl Config {
    pub fn load() -> Result<Self, String> {
        let file = match fs::read_to_string(".env") {
            Ok(file) => parse_env(&file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => return Err(format!("Không đọc được .env: {error}")),
        };
        let get = |key: &str| {
            env::var(key)
                .ok()
                .or_else(|| file.get(key).cloned())
                .unwrap_or_default()
        };
        let required = |key| {
            let value = get(key);
            if value.trim().is_empty() {
                Err(format!("Thiếu biến môi trường {key}"))
            } else {
                Ok(value)
            }
        };
        let default = |key, fallback: &str| {
            let value = get(key);
            if value.is_empty() {
                fallback.to_owned()
            } else {
                value
            }
        };
        let (shard_id, shard_count) = shard(&get("SHARD_ID"), &get("SHARD_COUNT"))?;
        Ok(Self {
            token: required("BOT_TOKEN")?,
            supabase_url: required("SUPABASE_URL")?.trim_end_matches('/').to_owned(),
            supabase_key: required("SUPABASE_KEY")?,
            report_channel: get("REPORT_CHANNEL"),
            correct_emoji: default("CORRECT_EMOJI", "✅"),
            wrong_emoji: default("WRONG_EMOJI", "❌"),
            port: port(&get("PORT"), shard_id)?,
            shard_id,
            shard_count,
            sync_interval: sync_interval(&get("SYNC_INTERVAL_SECS")),
            ai: AiConfig {
                enabled: !get("AI_BASE_URL").trim().is_empty() || !get("AI_API_KEY").is_empty(),
                base: default("AI_BASE_URL", "https://api.openai.com/v1")
                    .trim_end_matches('/')
                    .to_owned(),
                key: get("AI_API_KEY"),
                model: default("AI_MODEL", "gpt-4o-mini"),
                prompt: default(
                    "AI_SYSTEM_PROMPT",
                    "Bạn là trợ lý tiếng Việt hữu ích, trả lời ngắn gọn, súc tích.",
                ),
                timeout: Duration::from_millis(
                    get("AI_TIMEOUT_MS")
                        .parse()
                        .ok()
                        .filter(|ms| *ms > 0)
                        .unwrap_or(30_000),
                ),
            },
        })
    }
}

/// Unlike the other settings, a bad shard setting must stop the bot instead of falling back
/// to a default: a guessed value would make two processes serve the same guilds, which means
/// duplicate replies and conflicting read-modify-write updates of rankings and games.
fn shard(id: &str, count: &str) -> Result<(u32, u32), String> {
    let number = |name: &str, value: &str, default: u32| {
        let value = value.trim();
        if value.is_empty() {
            return Ok(default);
        }
        value
            .parse::<u32>()
            .map_err(|_| format!("{name} phải là số nguyên không âm, nhận được \"{value}\""))
    };
    let count = number("SHARD_COUNT", count, 1)?;
    let id = number("SHARD_ID", id, 0)?;
    if count == 0 {
        return Err("SHARD_COUNT phải từ 1 trở lên".into());
    }
    if id >= count {
        return Err(format!(
            "SHARD_ID ({id}) phải nhỏ hơn SHARD_COUNT ({count}); shard được đánh số từ 0"
        ));
    }
    Ok((id, count))
}

/// An unusable `PORT` falls back to the default, which is offset by the shard number.
fn port(value: &str, shard_id: u32) -> Result<u16, String> {
    value
        .trim()
        .parse()
        .ok()
        .filter(|port| *port > 0)
        .map_or_else(
            || {
                u16::try_from(BASE_PORT + shard_id).map_err(|_| {
                    format!("Không suy ra được PORT cho shard {shard_id}; hãy đặt biến PORT")
                })
            },
            Ok,
        )
}

/// Invalid, zero or absurdly large values fall back to the default instead of disabling
/// the resync or making it effectively never run.
fn sync_interval(value: &str) -> Duration {
    let secs = value
        .trim()
        .parse()
        .ok()
        .filter(|secs| (1..=MAX_SYNC_SECS).contains(secs));
    Duration::from_secs(secs.unwrap_or(DEFAULT_SYNC_SECS))
}

fn parse_env(file: &str) -> HashMap<String, String> {
    let mut parsed = HashMap::new();
    let mut lines = file.lines();
    while let Some(line) = lines.next() {
        let line = line.trim().trim_start_matches('\u{feff}');
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            continue;
        }
        let raw = raw.trim();
        let value = match raw.chars().next() {
            Some(marker @ ('"' | '\'' | '\x60')) => {
                let mut quoted = raw[1..].to_owned();
                loop {
                    let end = quoted.match_indices(marker).find(|(offset, _)| {
                        quoted[..*offset]
                            .bytes()
                            .rev()
                            .take_while(|byte| *byte == b'\\')
                            .count()
                            % 2
                            == 0
                    });
                    if let Some((end, _)) = end {
                        quoted.truncate(end);
                        break;
                    }
                    let Some(next) = lines.next() else {
                        break;
                    };
                    quoted.push('\n');
                    quoted.push_str(next);
                }
                if marker == '"' {
                    quoted.replace("\\n", "\n").replace("\\r", "\r")
                } else {
                    quoted
                }
            }
            _ => raw
                .split('#')
                .next()
                .unwrap_or_default()
                .trim_end()
                .to_owned(),
        };
        parsed.insert(key.to_owned(), value);
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_interval_defaults_to_ten_minutes_and_rejects_unusable_values() {
        assert_eq!(sync_interval(""), Duration::from_secs(600));
        assert_eq!(sync_interval(" 60 "), Duration::from_secs(60));
        for invalid in ["0", "-5", "abc", "1.5", "86401"] {
            assert_eq!(
                sync_interval(invalid),
                Duration::from_secs(600),
                "{invalid}"
            );
        }
        assert_eq!(sync_interval("86400"), Duration::from_secs(86_400));
    }

    #[test]
    fn shard_settings_default_to_a_single_shard_and_reject_ambiguous_values() {
        assert_eq!(shard("", ""), Ok((0, 1)));
        assert_eq!(shard(" 2 ", "4"), Ok((2, 4)));
        assert_eq!(shard("", "3"), Ok((0, 3)));
        assert_eq!(shard("3", "4"), Ok((3, 4)));
        for (id, count) in [
            ("4", "4"),
            ("1", ""),
            ("0", "0"),
            ("-1", "2"),
            ("x", "2"),
            ("0", "1.5"),
        ] {
            assert!(shard(id, count).is_err(), "{id}/{count}");
        }
        let error = shard("5", "4").unwrap_err();
        assert!(
            error.contains("SHARD_ID (5)") && error.contains("SHARD_COUNT (4)"),
            "{error}"
        );
    }

    #[test]
    fn each_shard_gets_its_own_default_port() {
        assert_eq!(port("", 0), Ok(3000));
        assert_eq!(port("", 3), Ok(3003));
        assert_eq!(port("8080", 3), Ok(8080));
        assert_eq!(port("0", 1), Ok(3001));
        assert_eq!(port("junk", 2), Ok(3002));
        assert!(port("", 70_000).is_err());
    }
}
