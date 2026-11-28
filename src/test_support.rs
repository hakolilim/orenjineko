use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};

use crate::App;
use crate::config::{AiConfig, Config};
use crate::discord::Discord;
use crate::http::Request;
use crate::json::Json;
use crate::store::Store;

pub struct Reply {
    pub method: &'static str,
    pub path: String,
    pub status: u16,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(method: &'static str, path: impl Into<String>, status: u16, body: Json) -> Self {
        Self {
            method,
            path: path.into(),
            status,
            body: if status == 204 {
                Vec::new()
            } else {
                body.encode().into_bytes()
            },
        }
    }
    pub fn empty(method: &'static str, path: impl Into<String>) -> Self {
        Self::json(method, path, 204, Json::Null)
    }
}

pub struct Received {
    pub path: String,
    pub body: Json,
    pub headers: Vec<String>,
}

#[derive(Default)]
struct Transport {
    replies: VecDeque<Reply>,
    received: Vec<Received>,
}

thread_local! {
    static TRANSPORT: RefCell<Option<Transport>> = const { RefCell::new(None) };
}

pub fn mock<R>(replies: Vec<Reply>, test: impl FnOnce() -> R) -> (R, Vec<Received>) {
    TRANSPORT.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Transport {
            replies: replies.into(),
            received: Vec::new(),
        });
    });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(test));
    let transport = TRANSPORT.with(|slot| slot.borrow_mut().take().unwrap());
    let result = result.unwrap_or_else(|error| std::panic::resume_unwind(error));
    assert!(
        transport.replies.is_empty(),
        "Còn {} request chưa được gọi",
        transport.replies.len()
    );
    (result, transport.received)
}

pub fn try_request(
    request: &Request,
    write: &mut dyn FnMut(&[u8]) -> bool,
) -> Option<Result<u16, String>> {
    TRANSPORT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let transport = slot.as_mut()?;
        let (_, target) = request.url.split_once("://").unwrap();
        let path = &target[target.find('/').unwrap()..];
        let reply = transport
            .replies
            .pop_front()
            .unwrap_or_else(|| panic!("Request ngoài dự kiến: {} {path}", request.method));
        assert_eq!(request.method, reply.method);
        assert_eq!(path, reply.path);
        transport.received.push(Received {
            path: reply.path,
            body: request
                .body
                .map(|body| Json::parse(body).unwrap())
                .unwrap_or_default(),
            headers: request.headers.to_vec(),
        });
        if !reply.body.is_empty() && !write(&reply.body) {
            return Some(Err("HTTP 23: callback kết thúc trước khi đọc hết".into()));
        }
        Some(Ok(reply.status))
    })
}

/// Every table in the order `Snapshot::fetch` reads it, with its primary-key columns.
pub const TABLES: [(&str, &[&str]); 8] = [
    ("words", &["word"]),
    ("report_words", &["word"]),
    ("guild_config", &["channel_id"]),
    ("counting_channels", &["channel_id"]),
    ("game_state", &["channel_id"]),
    ("rankings", &["guild_id", "user_id"]),
    ("global_stats", &["key"]),
    ("premium_guilds", &["guild_id"]),
];

/// Request path of one keyset page of `table`; an empty `cursor` is the first page.
pub fn page(table: &str, cursor: &str) -> String {
    let keys = TABLES.iter().find(|(name, _)| *name == table).unwrap().1;
    format!(
        "/rest/v1/{table}?select=*&order={}&limit=1000{cursor}",
        keys.join(",")
    )
}

/// Replies for one complete read of the given tables, as `Snapshot::fetch` requests them.
pub fn database(fixtures: &[(&str, Vec<Json>)]) -> Vec<Reply> {
    let mut replies = Vec::new();
    for (table, keys) in TABLES {
        let records = fixtures
            .iter()
            .find(|(name, _)| *name == table)
            .map(|(_, rows)| rows.as_slice())
            .unwrap_or_default();
        let mut cursor = String::new();
        for chunk in records.chunks(1000) {
            replies.push(Reply::json(
                "GET",
                page(table, &cursor),
                200,
                Json::Array(chunk.to_vec()),
            ));
            cursor = crate::store::after(keys, chunk.last().unwrap());
        }
        replies.push(Reply::json("GET", page(table, &cursor), 200, json!([])));
    }
    replies
}

pub fn dictionary() -> Vec<Json> {
    ["học sinh", "sinh nhật", "nhật học"]
        .into_iter()
        .map(|word| json!({"word": word}))
        .collect()
}

pub fn store() -> Store {
    Store::load("http://localhost", "supabase-test-key").unwrap()
}

pub fn app(store: Store) -> App {
    App {
        config: Config {
            token: "discord-test-token".into(),
            supabase_url: "http://localhost".into(),
            supabase_key: "supabase-test-key".into(),
            report_channel: "admin".into(),
            correct_emoji: "✅".into(),
            wrong_emoji: "❌".into(),
            port: 3000,
            shard_id: 0,
            shard_count: 1,
            sync_interval: Duration::from_secs(600),
            ai: AiConfig {
                enabled: false,
                base: "http://localhost".into(),
                key: String::new(),
                model: "test".into(),
                prompt: "Test".into(),
                timeout: Duration::from_secs(2),
            },
        },
        discord: Discord::test_client(),
        store: Mutex::new(store),
        activity: Default::default(),
        moderation: Mutex::new(()),
        started: Instant::now(),
        synced: AtomicBool::new(false),
        guild_total: AtomicUsize::new(0),
    }
}
