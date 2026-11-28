use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::discord::Guild;
use crate::json::Json;
use crate::websocket::{Event, WebSocket};
use crate::{App, SHUTDOWN, commands, game};

const INTENTS: u32 = (1 << 0) | (1 << 9) | (1 << 15);
const DEFAULT_GATEWAY: &str = "wss://gateway.discord.gg";
/// Shards whose `shard_id % max_concurrency` match may only IDENTIFY once per this period.
const IDENTIFY_SPACING: Duration = Duration::from_secs(5);

struct Workers {
    messages: Vec<SyncSender<Json>>,
    interactions: Vec<SyncSender<Json>>,
    ai: Vec<SyncSender<Json>>,
    autocomplete: Vec<SyncSender<Json>>,
    threads: Vec<JoinHandle<()>>,
    shard_count: u64,
}

/// Discord gives a guild to shard `(id >> 22) % shard_count`, so on one shard every guild
/// has the same remainder modulo `shard_count`. Taking `% workers` of that directly would send
/// all guilds to a single worker whenever `shard_count` is a multiple of the pool size.
/// Dividing the remainder out first spreads them over the whole pool, and one guild still always
/// maps to the same worker so its messages are handled in order.
fn worker_index(guild: u64, shard_count: u64, workers: usize) -> usize {
    ((guild >> 22) / shard_count.max(1) % workers as u64) as usize
}

impl Workers {
    fn new(app: &Arc<App>) -> Self {
        let mut threads = Vec::new();
        let messages = workers(app, 8, 128, game::message, &mut threads);
        let interactions = workers(app, 8, 16, commands::interaction, &mut threads);
        let ai = workers(app, 4, 4, commands::interaction, &mut threads);
        let autocomplete = workers(app, 2, 8, commands::interaction, &mut threads);
        Self {
            messages,
            interactions,
            ai,
            autocomplete,
            threads,
            shard_count: u64::from(app.config.shard_count),
        }
    }

    fn message(&self, data: Json) {
        if data["author"]["bot"].bool() || data["guild_id"].str().is_empty() {
            return;
        }
        let id = data["guild_id"].str().parse::<u64>().unwrap_or_default();
        let channel = &self.messages[worker_index(id, self.shard_count, self.messages.len())];
        if channel.try_send(data).is_err() {
            eprintln!("Hàng đợi tin nhắn Discord đã đầy");
        }
    }

    fn interaction(&self, mut data: Json) {
        let pool = match (data["type"].int(), data["data"]["name"].str()) {
            (4, _) => &self.autocomplete,
            (2, "ai") => &self.ai,
            _ => &self.interactions,
        };
        let start = crate::store::random_index(pool.len());
        for channel in pool.iter().cycle().skip(start).take(pool.len()) {
            match channel.try_send(data) {
                Ok(()) => return,
                Err(TrySendError::Full(value) | TrySendError::Disconnected(value)) => data = value,
            }
        }
        eprintln!("Hàng đợi tương tác Discord đã đầy");
    }

    fn join(self) {
        drop((self.messages, self.interactions, self.ai, self.autocomplete));
        for worker in self.threads {
            let _ = worker.join();
        }
    }
}

fn workers(
    app: &Arc<App>,
    size: usize,
    capacity: usize,
    handle: fn(&Arc<App>, Json),
    threads: &mut Vec<JoinHandle<()>>,
) -> Vec<SyncSender<Json>> {
    (0..size)
        .map(|_| {
            let app = Arc::clone(app);
            let (send, receive) = sync_channel(capacity);
            threads.push(thread::spawn(move || {
                while let Ok(data) = receive.recv() {
                    if SHUTDOWN.load(Ordering::Relaxed) {
                        break;
                    }
                    handle(&app, data);
                }
            }));
            send
        })
        .collect()
}

#[derive(Default)]
struct Session {
    sequence: Option<i64>,
    id: String,
    resume_url: String,
}

impl Session {
    fn resumable(&self) -> bool {
        !self.id.is_empty() && self.sequence.is_some()
    }
}

enum Disconnect {
    Retry,
    Identify,
    Fatal(u16),
}

pub fn run(app: Arc<App>) -> Result<(), String> {
    let workers = Workers::new(&app);
    let result = app
        .discord
        .rest("GET", "/gateway/bot", None)
        .and_then(|info| {
            let (shard, count) = (app.config.shard_id, app.config.shard_count);
            let limits = Limits::from(&info);
            eprintln!("Shard {shard}/{count}. {}", limits.describe(count));
            pause(identify_delay(shard, limits.max_concurrency));
            connect_loop(&app, &workers, &limits.url)
        });
    SHUTDOWN.store(true, Ordering::Relaxed);
    workers.join();
    result
}

/// What `/gateway/bot` says about connecting.
struct Limits {
    url: String,
    /// Shard count Discord recommends for the current number of guilds; 0 if unknown.
    recommended: u32,
    max_concurrency: u32,
    remaining: Option<i64>,
}

impl Limits {
    fn from(info: &Json) -> Self {
        let start = &info["session_start_limit"];
        Self {
            url: info["url"].opt_str().unwrap_or(DEFAULT_GATEWAY).to_owned(),
            recommended: info["shards"].int().clamp(0, u32::MAX.into()) as u32,
            max_concurrency: start["max_concurrency"].int().clamp(1, u32::MAX.into()) as u32,
            remaining: start["remaining"].decimal().map(|value| value as i64),
        }
    }

    fn describe(&self, shard_count: u32) -> String {
        let mut text = format!("Discord khuyến nghị {} shard", self.recommended);
        if let Some(remaining) = self.remaining {
            text.push_str(&format!(", còn {remaining} lượt IDENTIFY"));
        }
        if self.recommended > shard_count {
            text.push_str(&format!(
                ". CẢNH BÁO: SHARD_COUNT={shard_count} thấp hơn khuyến nghị, Discord có thể từ chối kết nối (mã 4011)"
            ));
        }
        text
    }
}

/// Shards that share an IDENTIFY slot (`shard_id % max_concurrency`) wait for their turn,
/// so processes started together do not trip Discord's IDENTIFY rate limit.
fn identify_delay(shard_id: u32, max_concurrency: u32) -> Duration {
    IDENTIFY_SPACING * (shard_id / max_concurrency.max(1))
}

/// Sleeps, but returns early when the bot is shutting down.
fn pause(duration: Duration) {
    let end = Instant::now() + duration;
    while !SHUTDOWN.load(Ordering::Relaxed) {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        thread::sleep(left.min(Duration::from_millis(100)));
    }
}

fn fatal_message(code: u16, shard_count: u32) -> String {
    let hint = match code {
        4004 => "Token bot không hợp lệ.".to_owned(),
        4010 => "Discord từ chối thông tin shard. Kiểm tra SHARD_ID và SHARD_COUNT.".to_owned(),
        4011 => format!(
            "Discord yêu cầu thêm shard: tăng SHARD_COUNT (hiện là {shard_count}) rồi khởi động lại mọi shard."
        ),
        4012 => "Phiên bản Gateway không được hỗ trợ.".to_owned(),
        4013 => "Intent gửi lên không hợp lệ.".to_owned(),
        _ => "Kiểm tra token và Message Content Intent.".to_owned(),
    };
    format!("Discord Gateway từ chối phiên (mã {code}). {hint}")
}

fn connect_loop(app: &Arc<App>, workers: &Workers, gateway_url: &str) -> Result<(), String> {
    let mut session = Session::default();
    let mut backoff = 1;
    while !SHUTDOWN.load(Ordering::Relaxed) {
        let url = if session.resume_url.is_empty() {
            gateway_url
        } else {
            &session.resume_url
        };
        let started = Instant::now();
        let result = WebSocket::connect(url)
            .and_then(|mut ws| connection(app, workers, &mut ws, &mut session));
        app.discord.cache.write().unwrap().ready = false;
        if SHUTDOWN.load(Ordering::Relaxed) {
            break;
        }
        match result {
            Ok(Disconnect::Fatal(code)) => {
                return Err(fatal_message(code, app.config.shard_count));
            }
            Ok(Disconnect::Identify) => session = Session::default(),
            Ok(Disconnect::Retry) => {}
            Err(error) => eprintln!("Discord Gateway mất kết nối: {error}"),
        }
        if started.elapsed() > Duration::from_secs(60) {
            backoff = 1;
        }
        pause(Duration::from_secs(backoff));
        backoff = (backoff * 2).min(30);
    }
    Ok(())
}

fn connection(
    app: &Arc<App>,
    workers: &Workers,
    ws: &mut WebSocket,
    session: &mut Session,
) -> Result<Disconnect, String> {
    let mut interval = Duration::from_secs(45);
    let mut next_beat = Instant::now() + interval;
    let mut waiting_for_ack = None;
    let mut received_hello = false;
    let hello_deadline = Instant::now() + Duration::from_secs(15);

    while !SHUTDOWN.load(Ordering::Relaxed) {
        let now = Instant::now();
        if !received_hello && now >= hello_deadline {
            return Err("Discord Gateway không gửi HELLO".into());
        }
        if now >= next_beat {
            if waiting_for_ack.is_some() {
                return Err("Discord Gateway không xác nhận heartbeat".into());
            }
            heartbeat(ws, session)?;
            waiting_for_ack = Some(now);
            next_beat = now + interval;
        }
        match ws.receive()? {
            Event::Pending => thread::sleep(Duration::from_millis(15)),
            Event::Close(code) => {
                return Ok(match code {
                    4004 | 4010..=4014 => Disconnect::Fatal(code),
                    4007 | 4009 => Disconnect::Identify,
                    _ => Disconnect::Retry,
                });
            }
            Event::Text(payload) => {
                let data = Json::parse(&payload)?;
                if !data["s"].is_null() {
                    session.sequence = Some(data["s"].int());
                }
                match data["op"].int() {
                    0 => dispatch(app, workers, session, data["t"].str(), data["d"].clone()),
                    1 => {
                        heartbeat(ws, session)?;
                        waiting_for_ack = Some(Instant::now());
                    }
                    7 => return Ok(Disconnect::Retry),
                    9 => {
                        return Ok(if data["d"].bool() {
                            Disconnect::Retry
                        } else {
                            Disconnect::Identify
                        });
                    }
                    10 => {
                        received_hello = true;
                        let ms =
                            data["d"]["heartbeat_interval"].int().clamp(1000, 300_000) as usize;
                        interval = Duration::from_millis(ms as u64);
                        next_beat = Instant::now()
                            + Duration::from_millis(crate::store::random_index(ms) as u64);
                        let token = &app.discord.token;
                        let identify = if session.resumable() {
                            json!({"op": 6, "d": {"token": token, "session_id": (&session.id), "seq": (session.sequence)}})
                        } else {
                            identify(token, app.config.shard_id, app.config.shard_count)
                        };
                        ws.text(&identify.encode())?;
                    }
                    11 => {
                        if let Some(sent) = waiting_for_ack.take() {
                            app.discord.cache.write().unwrap().ping =
                                Some(sent.elapsed().as_millis());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(Disconnect::Retry)
}

fn identify(token: &str, shard_id: u32, shard_count: u32) -> Json {
    json!({"op": 2, "d": {
        "token": token, "intents": INTENTS, "shard": [shard_id, shard_count], "properties": {
            "os": (std::env::consts::OS), "browser": "orenjineko", "device": "orenjineko"
        }, "presence": {
            "status": "idle", "since": null, "afk": false,
            "activities": [{"name": "nối từ | /rank", "type": 0}]
        }
    }})
}

fn heartbeat(ws: &mut WebSocket, session: &Session) -> Result<(), String> {
    ws.text(&json!({"op": 1, "d": (session.sequence)}).encode())
}

fn dispatch(app: &Arc<App>, workers: &Workers, session: &mut Session, event: &str, data: Json) {
    match event {
        "READY" => {
            session.id = data["session_id"].str().to_owned();
            session.resume_url = data["resume_gateway_url"].str().to_owned();
            let application = data["application"]["id"].str().to_owned();
            let mut cache = app.discord.cache.write().unwrap();
            cache.user = data["user"].clone();
            cache.application = application.clone();
            cache.guilds.clear();
            for guild in data["guilds"].array() {
                cache.guilds.insert(
                    guild["id"].str().to_owned(),
                    Guild::from_gateway(guild.clone(), cache_user(&data)),
                );
            }
            cache.ready = true;
            drop(cache);
            eprintln!(
                "Đã kết nối Discord với {} (shard {}/{}, {} máy chủ)",
                data["user"]["username"].str(),
                app.config.shard_id,
                app.config.shard_count,
                data["guilds"].array().len()
            );
            // The command list is global, so one shard registers it. Every shard doing a
            // bulk overwrite at once only burns Discord's command-update rate limit.
            if app.config.shard_id == 0 && !app.synced.swap(true, Ordering::Relaxed) {
                let app = Arc::clone(app);
                thread::spawn(move || {
                    if let Err(error) = app.discord.rest(
                        "PUT",
                        &format!("/applications/{application}/commands"),
                        Some(commands::definitions()),
                    ) {
                        eprintln!("Đồng bộ slash command: {error}");
                        app.synced.store(false, Ordering::Relaxed);
                    }
                });
            }
        }
        "RESUMED" => app.discord.cache.write().unwrap().ready = true,
        "GUILD_CREATE" => {
            let guild = data["id"].str().to_owned();
            app.activity.allow(&guild);
            let mut cache = app.discord.cache.write().unwrap();
            let state = Guild::from_gateway(data, cache.user["id"].str());
            cache.guilds.insert(guild, state);
        }
        "GUILD_UPDATE" => {
            let mut cache = app.discord.cache.write().unwrap();
            if let Some(guild) = cache.guilds.get_mut(data["id"].str())
                && let Json::Object(fields) = data
            {
                for (key, value) in fields {
                    guild.data[key.as_str()] = value;
                }
            }
        }
        "GUILD_DELETE" => {
            let mut cache = app.discord.cache.write().unwrap();
            if data["unavailable"].bool() {
                if let Some(guild) = cache.guilds.get_mut(data["id"].str()) {
                    guild.data["unavailable"] = true.into();
                }
            } else {
                cache.guilds.remove(data["id"].str());
            }
        }
        "CHANNEL_CREATE" | "CHANNEL_UPDATE" | "THREAD_CREATE" | "THREAD_UPDATE" => {
            if let Some(guild) = app
                .discord
                .cache
                .write()
                .unwrap()
                .guilds
                .get_mut(data["guild_id"].str())
            {
                guild.channels.insert(data["id"].str().to_owned(), data);
            }
        }
        "CHANNEL_DELETE" | "THREAD_DELETE" => {
            if let Some(guild) = app
                .discord
                .cache
                .write()
                .unwrap()
                .guilds
                .get_mut(data["guild_id"].str())
            {
                guild.channels.remove(data["id"].str());
            }
        }
        "GUILD_ROLE_CREATE" | "GUILD_ROLE_UPDATE" => {
            if let Some(guild) = app
                .discord
                .cache
                .write()
                .unwrap()
                .guilds
                .get_mut(data["guild_id"].str())
            {
                guild
                    .roles
                    .insert(data["role"]["id"].str().to_owned(), data["role"].clone());
            }
        }
        "GUILD_ROLE_DELETE" => {
            if let Some(guild) = app
                .discord
                .cache
                .write()
                .unwrap()
                .guilds
                .get_mut(data["guild_id"].str())
            {
                guild.roles.remove(data["role_id"].str());
            }
        }
        "GUILD_MEMBER_UPDATE" => {
            let mut cache = app.discord.cache.write().unwrap();
            if data["user"]["id"].str() == cache.user["id"].str()
                && let Some(guild) = cache.guilds.get_mut(data["guild_id"].str())
            {
                let joined = guild.bot_member["joined_at"].clone();
                guild.bot_member = data;
                if guild.bot_member["joined_at"].is_null() {
                    guild.bot_member["joined_at"] = joined;
                }
            }
        }
        "USER_UPDATE" => app.discord.cache.write().unwrap().user = data,
        "MESSAGE_CREATE" => workers.message(data),
        "INTERACTION_CREATE" => workers.interaction(data),
        _ => {}
    }
}

fn cache_user(data: &Json) -> &str {
    data["user"]["id"].str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_names_the_shard_this_process_serves() {
        let payload = identify("secret", 2, 4);
        assert_eq!(payload["op"].int(), 2);
        let shard = payload["d"]["shard"].array();
        assert_eq!((shard[0].int(), shard[1].int()), (2, 4));
        assert_eq!(payload["d"]["token"].str(), "secret");
        assert_eq!(payload["d"]["intents"].int(), i64::from(INTENTS));
        let encoded = payload.encode();
        assert!(encoded.contains(r#""shard":[2,4]"#), "{encoded}");
    }

    #[test]
    fn identify_waits_only_for_shards_that_share_a_slot() {
        let secs = |shard, concurrency| identify_delay(shard, concurrency).as_secs();
        assert_eq!((secs(0, 1), secs(1, 1), secs(3, 1)), (0, 5, 15));
        // With max_concurrency 16, shards 0-15 identify together, 16-31 five seconds later.
        assert_eq!(
            (secs(0, 16), secs(15, 16), secs(16, 16), secs(33, 16)),
            (0, 0, 5, 10)
        );
        assert_eq!(secs(7, 0), 35, "a bogus 0 is treated as 1");
    }

    #[test]
    fn gateway_limits_are_read_with_safe_defaults() {
        let info = json!({
            "url": "wss://gateway.example", "shards": 6,
            "session_start_limit": {"remaining": 990, "max_concurrency": 4}
        });
        let limits = Limits::from(&info);
        assert_eq!(limits.url, "wss://gateway.example");
        assert_eq!(
            (limits.recommended, limits.max_concurrency, limits.remaining),
            (6, 4, Some(990))
        );
        let described = limits.describe(2);
        assert!(
            described.contains("khuyến nghị 6")
                && described.contains("còn 990")
                && described.contains("CẢNH BÁO")
        );
        assert!(!limits.describe(6).contains("CẢNH BÁO"));

        let bare = Limits::from(&json!({}));
        assert_eq!(bare.url, DEFAULT_GATEWAY);
        assert_eq!(
            (bare.recommended, bare.max_concurrency, bare.remaining),
            (0, 1, None)
        );
        assert!(!bare.describe(1).contains("CẢNH BÁO"));
    }

    #[test]
    fn fatal_close_codes_explain_what_to_fix() {
        assert!(
            fatal_message(4010, 2).contains("SHARD_ID")
                && fatal_message(4010, 2).contains("SHARD_COUNT")
        );
        let more = fatal_message(4011, 2);
        assert!(
            more.contains("tăng SHARD_COUNT") && more.contains("(hiện là 2)"),
            "{more}"
        );
        assert!(fatal_message(4004, 1).contains("Token"));
        assert!(fatal_message(4014, 1).contains("Message Content Intent"));
    }

    #[test]
    fn a_shards_guilds_spread_over_every_message_worker_in_a_stable_order() {
        let snowflake = |timestamp: u64| timestamp << 22 | 0x2a;
        for (count, shard) in [(1u64, 0u64), (2, 1), (8, 3), (16, 5), (4, 0)] {
            let mut used = [0usize; 8];
            for round in 0..800u64 {
                let guild = snowflake(round * count + shard);
                assert_eq!((guild >> 22) % count, shard);
                let worker = worker_index(guild, count, 8);
                assert_eq!(
                    worker,
                    worker_index(guild, count, 8),
                    "same guild, same worker"
                );
                used[worker] += 1;
            }
            assert!(
                used.iter().all(|uses| *uses == 100),
                "{count}/{shard}: {used:?}"
            );
        }
        // One shard keeps the previous placement.
        assert_eq!(worker_index(snowflake(21), 1, 8), 21 % 8);
        // A degenerate count must not divide by zero.
        assert_eq!(worker_index(snowflake(21), 0, 8), 21 % 8);
    }
}
