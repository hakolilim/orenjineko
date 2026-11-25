use std::{collections::HashMap, env, fs, time::Duration};

pub struct Config {
    pub token: String,
    pub supabase_url: String,
    pub supabase_key: String,
    pub report_channel: String,
    pub correct_emoji: String,
    pub wrong_emoji: String,
    pub port: u16,
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
        Ok(Self {
            token: required("BOT_TOKEN")?,
            supabase_url: required("SUPABASE_URL")?.trim_end_matches('/').to_owned(),
            supabase_key: required("SUPABASE_KEY")?,
            report_channel: get("REPORT_CHANNEL"),
            correct_emoji: default("CORRECT_EMOJI", "✅"),
            wrong_emoji: default("WRONG_EMOJI", "❌"),
            port: get("PORT")
                .parse()
                .ok()
                .filter(|port| *port > 0)
                .unwrap_or(3000),
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
