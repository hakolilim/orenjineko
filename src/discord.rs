use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::http::{Request, encode_component};
use crate::json::Json;

pub const ADMIN: u128 = 1 << 3;
pub const MANAGE_GUILD: u128 = 1 << 5;
pub const ADD_REACTIONS: u128 = 1 << 6;
pub const VIEW_CHANNEL: u128 = 1 << 10;
pub const SEND_MESSAGES: u128 = 1 << 11;
pub const EMBED_LINKS: u128 = 1 << 14;
pub const ATTACH_FILES: u128 = 1 << 15;
pub const READ_HISTORY: u128 = 1 << 16;
pub const EPHEMERAL: u32 = 64;
pub const SILENT: u32 = 4096;
pub const COLOR: u32 = 13250094;

pub struct Discord {
    base: String,
    pub token: String,
    global_delay: Mutex<Option<Instant>>,
    pub cache: RwLock<Cache>,
}

#[derive(Default)]
pub struct Cache {
    pub user: Json,
    pub application: String,
    pub ready: bool,
    pub ping: Option<u128>,
    pub guilds: HashMap<String, Guild>,
}

#[derive(Default)]
pub struct Guild {
    pub data: Json,
    pub roles: HashMap<String, Json>,
    pub channels: HashMap<String, Json>,
    pub bot_member: Json,
}

impl Guild {
    pub fn from_gateway(data: Json, bot: &str) -> Self {
        let bot_member = data["members"]
            .array()
            .iter()
            .find(|member| member["user"]["id"].str() == bot)
            .cloned()
            .unwrap_or_default();
        let roles = by_id(data["roles"].array());
        let mut channels = by_id(data["channels"].array());
        channels.extend(by_id(data["threads"].array()));
        Self {
            roles,
            channels,
            bot_member,
            data,
        }
    }

    pub fn permissions(&self, guild: &str, user: &str, member: &Json, channel: &Json) -> u128 {
        if user == self.data["owner_id"].str() {
            return u128::MAX;
        }
        let roles: HashSet<_> = member["roles"].array().iter().map(Json::str).collect();
        let base = self
            .roles
            .get(guild)
            .map(|role| bits(&role["permissions"]))
            .unwrap_or_default();
        let mut permissions = roles
            .iter()
            .filter_map(|role| self.roles.get(*role))
            .fold(base, |current, role| current | bits(&role["permissions"]));
        if permissions & ADMIN != 0 {
            return u128::MAX;
        }
        let mut overwrite_channel = channel;
        if matches!(channel["type"].int(), 10..=12)
            && let Some(parent) = self.channels.get(channel["parent_id"].str())
        {
            overwrite_channel = parent;
        }
        let overwrites = overwrite_channel["permission_overwrites"].array();
        let mut deny = 0;
        let mut allow = 0;
        for overwrite in overwrites {
            let id = overwrite["id"].str();
            if overwrite["type"].int() != 0 {
                continue;
            }
            if id == guild {
                permissions = apply(permissions, overwrite);
            } else if roles.contains(id) {
                deny |= bits(&overwrite["deny"]);
                allow |= bits(&overwrite["allow"]);
            }
        }
        permissions = (permissions & !deny) | allow;
        if let Some(overwrite) = overwrites
            .iter()
            .find(|item| item["type"].int() == 1 && item["id"].str() == user)
        {
            permissions = apply(permissions, overwrite);
        }
        permissions
    }
}

fn apply(current: u128, overwrite: &Json) -> u128 {
    (current & !bits(&overwrite["deny"])) | bits(&overwrite["allow"])
}

fn by_id(array: &[Json]) -> HashMap<String, Json> {
    array
        .iter()
        .map(|item| (item["id"].str().to_owned(), item.clone()))
        .collect()
}

pub fn bits(value: &Json) -> u128 {
    value.str().parse().unwrap_or_default()
}
pub fn has(permissions: u128, required: u128) -> bool {
    permissions & ADMIN != 0 || permissions & required == required
}

impl Discord {
    pub fn new(token: &str) -> Self {
        Self {
            base: "https://discord.com/api/v10".into(),
            token: token.into(),
            global_delay: Mutex::new(None),
            cache: RwLock::new(Cache::default()),
        }
    }

    #[cfg(test)]
    pub fn test_client() -> Self {
        let mut discord = Self::new("discord-test-token");
        discord.base = "http://localhost/api/v10".into();
        discord
    }

    pub fn rest(&self, method: &str, path: &str, data: Option<Json>) -> Result<Json, String> {
        let data = data.map(|json| json.encode());
        self.request(
            method,
            path,
            data.as_deref().map(str::as_bytes),
            "application/json",
        )
    }

    pub fn upload(
        &self,
        path: &str,
        data: Json,
        filename: &str,
        file: &[u8],
    ) -> Result<Json, String> {
        let boundary = format!(
            "orenjineko-{}-{}",
            now_ms(),
            crate::store::random_index(usize::MAX)
        );
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"payload_json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n\
            --{boundary}\r\nContent-Disposition: form-data; name=\"files[0]\"; filename=\"{filename}\"\r\nContent-Type: image/png\r\n\r\n",
            data.encode(),
        ).into_bytes();
        body.extend_from_slice(file);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        self.request(
            "POST",
            path,
            Some(&body),
            &format!("multipart/form-data; boundary={boundary}"),
        )
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        content_type: &str,
    ) -> Result<Json, String> {
        // Webhook/callback URLs contain their own credential.
        let auth = !path.starts_with("/interactions/") && !path.starts_with("/webhooks/");
        let mut headers = vec![
            concat!(
                "User-Agent: DiscordBot (https://github.com/hakolilim/orenjineko, ",
                env!("CARGO_PKG_VERSION"),
                ")"
            )
            .into(),
        ];
        if auth {
            headers.push(format!("Authorization: Bot {}", self.token));
        }
        if body.is_some() {
            headers.push(format!("Content-Type: {content_type}"));
        } else if method == "PUT" {
            headers.push("Content-Length: 0".into());
        }
        let url = format!("{}{path}", self.base);
        for attempt in 0..5 {
            if auth {
                let wait = self
                    .global_delay
                    .lock()
                    .unwrap()
                    .map(|deadline| deadline.saturating_duration_since(Instant::now()));
                if let Some(wait) = wait {
                    thread::sleep(wait);
                }
            }
            let response = Request {
                method,
                url: &url,
                headers: &headers,
                body,
                timeout: Duration::from_secs(12),
                redirects: false,
            }
            .send()?;
            if response.success() {
                return response.json();
            }
            let data = response.json().unwrap_or_default();
            if response.code == 429 && attempt < 4 {
                let duration = Duration::from_secs_f64(
                    data["retry_after"]
                        .decimal()
                        .unwrap_or(1.0)
                        .clamp(0.05, 60.0),
                );
                if auth && data["global"].bool() {
                    let mut delay = self.global_delay.lock().unwrap();
                    let until = Instant::now() + duration;
                    *delay = Some(delay.unwrap_or(until).max(until));
                }
                thread::sleep(duration);
                continue;
            }
            return Err(format!(
                "Discord {method}: HTTP {} {}",
                response.code,
                data["message"].str()
            ));
        }
        unreachable!()
    }

    pub fn send(&self, channel: &str, data: Json) -> Result<Json, String> {
        self.rest(
            "POST",
            &format!("/channels/{channel}/messages"),
            Some(safe_message(data)),
        )
    }

    pub fn react(&self, channel: &str, message: &str, emoji: &str) -> Result<(), String> {
        let emoji = emoji
            .trim()
            .trim_matches(['<', '>'])
            .trim_start_matches("a:")
            .trim_start_matches(':');
        let emoji = encode_component(emoji);
        self.rest(
            "PUT",
            &format!("/channels/{channel}/messages/{message}/reactions/{emoji}/@me"),
            None,
        )?;
        Ok(())
    }

    pub fn channel(&self, guild: &str, channel: &str) -> Option<Json> {
        self.cache
            .read()
            .unwrap()
            .guilds
            .get(guild)?
            .channels
            .get(channel)
            .cloned()
    }

    pub fn guild_name(&self, guild: &str) -> String {
        self.cache
            .read()
            .unwrap()
            .guilds
            .get(guild)
            .and_then(|guild| guild.data["name"].opt_str())
            .unwrap_or("Máy chủ")
            .to_owned()
    }

    pub fn bot_permissions(&self, guild_id: &str, channel: &Json) -> Result<u128, String> {
        let bot_id = self.cache.read().unwrap().user["id"].str().to_owned();
        let cached = self
            .cache
            .read()
            .unwrap()
            .guilds
            .get(guild_id)
            .map(|guild| guild.bot_member.clone());
        let member = match cached {
            Some(member) if !member.is_null() => member,
            _ => self.rest("GET", &format!("/guilds/{guild_id}/members/{bot_id}"), None)?,
        };
        let cache = self.cache.read().unwrap();
        let guild = cache
            .guilds
            .get(guild_id)
            .ok_or("Máy chủ chưa có trong bộ nhớ Discord")?;
        Ok(guild.permissions(guild_id, &bot_id, &member, channel))
    }
}

pub fn safe_message(mut data: Json) -> Json {
    data["allowed_mentions"] = json!({"parse": [], "replied_user": false});
    data
}

pub struct Interaction<'a> {
    pub discord: &'a Discord,
    pub data: Json,
    acknowledged: bool,
    private: bool,
}

impl<'a> Interaction<'a> {
    pub fn new(discord: &'a Discord, data: Json) -> Self {
        Self {
            discord,
            data,
            acknowledged: false,
            private: false,
        }
    }

    pub fn guild(&self) -> &str {
        self.data["guild_id"].str()
    }

    pub fn channel(&self) -> &str {
        self.data["channel_id"]
            .opt_str()
            .unwrap_or(self.data["channel"]["id"].str())
    }

    pub fn user(&self) -> &Json {
        if self.data["member"]["user"].is_null() {
            &self.data["user"]
        } else {
            &self.data["member"]["user"]
        }
    }

    pub fn option(&self, name: &str) -> &Json {
        self.data["data"]["options"]
            .array()
            .iter()
            .find(|option| option["name"].str() == name)
            .map(|option| &option["value"])
            .unwrap_or(&Json::Null)
    }

    pub fn can_manage(&self) -> bool {
        has(bits(&self.data["member"]["permissions"]), MANAGE_GUILD)
    }

    pub fn callback(&mut self, kind: u32, payload: Json) -> Result<(), String> {
        let id = self.data["id"].str();
        let token = self.data["token"].str();
        self.discord.rest(
            "POST",
            &format!("/interactions/{id}/{token}/callback"),
            Some(json!({"type": kind, "data": payload})),
        )?;
        self.acknowledged = true;
        Ok(())
    }

    pub fn reply(&mut self, data: Json) -> Result<(), String> {
        self.private = data["flags"].int() & i64::from(EPHEMERAL) != 0;
        self.callback(4, safe_message(data))
    }

    pub fn text(&mut self, text: impl Into<Json>, private: bool) -> Result<(), String> {
        self.reply(
            json!({"content": (text.into()), "flags": (if private { EPHEMERAL } else { 0 })}),
        )
    }

    pub fn defer(&mut self, private: bool) -> Result<(), String> {
        self.private = private;
        self.callback(5, json!({"flags": (if private { EPHEMERAL } else { 0 })}))
    }

    fn webhook(&self) -> String {
        format!(
            "/webhooks/{}/{}",
            self.data["application_id"].str(),
            self.data["token"].str()
        )
    }

    pub fn edit(&self, data: Json) -> Result<(), String> {
        self.discord.rest(
            "PATCH",
            &format!("{}/messages/@original", self.webhook()),
            Some(safe_message(data)),
        )?;
        Ok(())
    }

    pub fn edit_text(&self, text: impl Into<Json>) -> Result<(), String> {
        self.edit(json!({"content": (text.into())}))
    }

    pub fn follow(&self, data: Json) -> Result<(), String> {
        self.discord
            .rest("POST", &self.webhook(), Some(self.follow_data(data)))?;
        Ok(())
    }

    fn follow_data(&self, mut data: Json) -> Json {
        if self.private {
            data["flags"] = EPHEMERAL.into();
        }
        safe_message(data)
    }

    pub fn follow_file(&self, mut data: Json, bytes: &[u8]) -> Result<(), String> {
        data["attachments"] = json!([{"id": 0, "filename": "ai-latex.png"}]);
        self.discord.upload(
            &self.webhook(),
            self.follow_data(data),
            "ai-latex.png",
            bytes,
        )?;
        Ok(())
    }

    pub fn error(&mut self) {
        const ERROR: &str = "Đã xảy ra lỗi khi thực thi lệnh. Vui lòng thử lại.";
        let result = if self.acknowledged {
            self.follow(json!({"content": ERROR, "flags": EPHEMERAL}))
        } else {
            self.text(ERROR, true)
        };
        if let Err(error) = result {
            eprintln!("Gửi lỗi Discord: {error}");
        }
    }
}

pub fn field(name: &str, value: impl Into<Json>, inline: bool) -> Json {
    json!({"name": name, "value": (value.into()), "inline": inline})
}

pub fn embed(fields: Vec<Json>) -> Json {
    json!({"color": COLOR, "fields": fields})
}

pub fn display_name<'a>(user: &'a Json, member: &'a Json) -> &'a str {
    member["nick"]
        .opt_str()
        .or_else(|| user["global_name"].opt_str())
        .unwrap_or(user["username"].str())
}

pub fn avatar(user: &Json) -> String {
    let id = user["id"].str();
    if let Some(hash) = user["avatar"].opt_str() {
        return format!(
            "https://cdn.discordapp.com/avatars/{id}/{hash}.{}?size=128",
            if hash.starts_with("a_") { "gif" } else { "png" }
        );
    }
    let index = user["discriminator"]
        .str()
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .map(|value| value % 5)
        .unwrap_or_else(|| (id.parse::<u64>().unwrap_or_default() >> 22) % 6);
    format!("https://cdn.discordapp.com/embed/avatars/{index}.png")
}

pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub fn snowflake_ms(id: &str) -> u128 {
    (id.parse::<u128>().unwrap_or_default() >> 22) + 1_420_070_400_000
}

pub fn clip(text: &str, max: usize) -> &str {
    let mut length = 0;
    for (index, character) in text.char_indices() {
        length += character.len_utf16();
        if length > max {
            return &text[..index];
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_apply_everyone_combined_roles_then_member() {
        let mut guild = Guild::default();
        guild.roles.insert(
            "guild".into(),
            json!({"permissions": (VIEW_CHANNEL.to_string())}),
        );
        guild.roles.insert(
            "red".into(),
            json!({"permissions": (SEND_MESSAGES.to_string())}),
        );
        let channel = json!({"permission_overwrites": [
            {"id": "guild", "type": 0, "deny": (SEND_MESSAGES.to_string()), "allow": "0"},
            {"id": "red", "type": 0, "deny": "0", "allow": (SEND_MESSAGES.to_string())},
            {"id": "user", "type": 1, "deny": (VIEW_CHANNEL.to_string()), "allow": "0"}
        ]});
        assert_eq!(
            guild.permissions("guild", "user", &json!({"roles": ["red"]}), &channel),
            SEND_MESSAGES
        );
    }
}
