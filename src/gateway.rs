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

struct Workers {
    messages: Vec<SyncSender<Json>>,
    interactions: Vec<SyncSender<Json>>,
    ai: Vec<SyncSender<Json>>,
    autocomplete: Vec<SyncSender<Json>>,
    threads: Vec<JoinHandle<()>>,
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
        }
    }

    fn message(&self, data: Json) {
        if data["author"]["bot"].bool() || data["guild_id"].str().is_empty() {
            return;
        }
        let id = data["guild_id"].str().parse::<u64>().unwrap_or_default();
        let channel = &self.messages[(id as usize >> 22) % self.messages.len()];
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
    let gateway_url = app
        .discord
        .rest("GET", "/gateway/bot", None)
        .map(|data| data["url"].opt_str().unwrap_or(DEFAULT_GATEWAY).to_owned());
    let result = gateway_url.and_then(|gateway_url| connect_loop(&app, &workers, &gateway_url));
    SHUTDOWN.store(true, Ordering::Relaxed);
    workers.join();
    result
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
                return Err(format!(
                    "Discord Gateway từ chối phiên (mã {code}). Kiểm tra token và Message Content Intent."
                ));
            }
            Ok(Disconnect::Identify) => session = Session::default(),
            Ok(Disconnect::Retry) => {}
            Err(error) => eprintln!("Discord Gateway mất kết nối: {error}"),
        }
        if started.elapsed() > Duration::from_secs(60) {
            backoff = 1;
        }
        for _ in 0..backoff * 10 {
            if SHUTDOWN.load(Ordering::Relaxed) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
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
                            json!({"op": 2, "d": {
                                "token": token, "intents": INTENTS, "properties": {
                                    "os": (std::env::consts::OS), "browser": "orenjineko", "device": "orenjineko"
                                }, "presence": {
                                    "status": "idle", "since": null, "afk": false,
                                    "activities": [{"name": "nối từ | /rank", "type": 0}]
                                }
                            }})
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
            eprintln!("Đã kết nối Discord với {}", data["user"]["username"].str());
            if !app.synced.swap(true, Ordering::Relaxed) {
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
