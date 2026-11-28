#[macro_use]
mod json;
mod ai;
mod commands;
mod config;
mod discord;
mod game;
mod gateway;
mod http;
mod latex;
mod store;
#[cfg(test)]
mod test_support;
mod web;
mod websocket;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use config::Config;
use discord::Discord;
use store::Store;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

struct App {
    config: Config,
    discord: Discord,
    store: Mutex<Store>,
    activity: Activity,
    moderation: Mutex<()>,
    started: Instant,
    synced: AtomicBool,
    /// Guild count across every shard, refreshed by the sync thread. Zero until the first
    /// refresh (and always zero with a single shard, where the local cache is complete).
    guild_total: AtomicUsize,
}

impl App {
    /// Guilds this bot is in, over all shards. The local cache only knows this shard's guilds,
    /// so it is a lower bound that is used while the shared count is unavailable.
    fn total_guilds(&self) -> usize {
        let local = self.discord.cache.read().unwrap().guilds.len();
        local.max(self.guild_total.load(Ordering::Relaxed))
    }

    /// Asks Discord for the application's guild count (all shards). Called from the
    /// background thread so a slow Discord never delays the status endpoints.
    fn refresh_guild_total(&self) -> Result<(), String> {
        if self.config.shard_count == 1 {
            return Ok(());
        }
        let application = self.discord.rest("GET", "/applications/@me", None)?;
        let count = application["approximate_guild_count"].int();
        if count > 0 {
            self.guild_total.store(count as usize, Ordering::Relaxed);
        }
        Ok(())
    }
}

/// The first sync waits longer for higher shards so that N processes started together do not
/// all read Supabase at the same moment, then every shard keeps the same period.
fn first_sync_delay(interval: Duration, shard_id: u32, shard_count: u32) -> Duration {
    interval + interval * shard_id / shard_count.max(1)
}

#[derive(Default)]
struct Activity {
    inner: Mutex<HashMap<String, GuildActivity>>,
    idle: Condvar,
}

#[derive(Default)]
struct GuildActivity {
    leaving: bool,
    left: bool,
    active: usize,
}

struct Operation<'a> {
    activity: &'a Activity,
    guild: String,
}

struct Leave<'a> {
    activity: &'a Activity,
    guild: String,
    completed: bool,
}

impl Activity {
    fn begin(&self, guild: &str) -> Option<Operation<'_>> {
        let mut guards = self.inner.lock().unwrap();
        let state = guards.entry(guild.into()).or_default();
        if state.leaving {
            return None;
        }
        state.active += 1;
        Some(Operation {
            activity: self,
            guild: guild.into(),
        })
    }

    fn leave(&self, guild: &str) -> Option<Leave<'_>> {
        let mut guards = self.inner.lock().unwrap();
        let state = guards.entry(guild.into()).or_default();
        if state.leaving {
            return None;
        }
        state.leaving = true;
        Some(Leave {
            activity: self,
            guild: guild.into(),
            completed: false,
        })
    }

    fn allow(&self, guild: &str) {
        let mut guards = self.inner.lock().unwrap();
        if guards.get(guild).is_some_and(|state| state.left) {
            guards.remove(guild);
        }
    }
}

impl Drop for Operation<'_> {
    fn drop(&mut self) {
        let mut guards = self.activity.inner.lock().unwrap();
        if let Some(state) = guards.get_mut(&self.guild) {
            state.active -= 1;
            if state.active == 0 {
                if !state.leaving {
                    guards.remove(&self.guild);
                }
                self.activity.idle.notify_all();
            }
        }
    }
}

impl Leave<'_> {
    fn drain(&self) {
        let guards = self.activity.inner.lock().unwrap();
        drop(
            self.activity
                .idle
                .wait_while(guards, |guards| {
                    guards
                        .get(&self.guild)
                        .is_some_and(|state| state.active != 0)
                })
                .unwrap(),
        );
    }

    fn complete(mut self) {
        if let Some(state) = self.activity.inner.lock().unwrap().get_mut(&self.guild) {
            state.left = true;
        }
        self.completed = true;
    }
}

impl Drop for Leave<'_> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let mut guards = self.activity.inner.lock().unwrap();
        if let Some(state) = guards.get_mut(&self.guild) {
            state.leaving = false;
            if state.active == 0 {
                guards.remove(&self.guild);
            }
        }
    }
}

unsafe extern "C" {
    fn signal(number: i32, handler: extern "C" fn(i32)) -> usize;
}

extern "C" fn shutdown(_: i32) {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

fn run() -> Result<(), String> {
    let started = Instant::now();
    let config = Config::load()?;
    // SIGINT and SIGTERM only set an atomic flag. Database work runs on ordinary threads.
    unsafe {
        signal(2, shutdown);
        signal(15, shutdown);
    }
    let sync_interval = config.sync_interval;
    let first_sync = first_sync_delay(sync_interval, config.shard_id, config.shard_count);
    eprintln!("Đang nạp dữ liệu Supabase...");
    let store = Store::load(&config.supabase_url, &config.supabase_key)?;
    eprintln!("Đã nạp {} từ khả dụng.", store.dictionary.count());
    let discord = Discord::new(&config.token);
    let app = Arc::new(App {
        config,
        discord,
        store: Mutex::new(store),
        activity: Activity::default(),
        moderation: Mutex::new(()),
        started,
        synced: AtomicBool::new(false),
        guild_total: AtomicUsize::new(0),
    });
    let server = web::start(Arc::clone(&app))?;
    game::start_deletions(Arc::clone(&app));
    let (stop_flush, flush_signal) = channel::<()>();
    let flush_app = Arc::clone(&app);
    let flusher = thread::spawn(move || {
        while matches!(
            flush_signal.recv_timeout(Duration::from_secs(60)),
            Err(RecvTimeoutError::Timeout)
        ) {
            if let Err(error) = flush_app.store.lock().unwrap().flush_stats() {
                eprintln!("Ghi thống kê định kỳ: {error}");
            }
        }
    });
    // Mirrors Supabase into RAM so changes made elsewhere (other shards, the SQL editor)
    // are picked up. The first run is one interval after the startup load.
    let (stop_resync, resync_signal) = channel::<()>();
    let resync_app = Arc::clone(&app);
    let resyncer = thread::spawn(move || {
        let mut wait = first_sync;
        if let Err(error) = resync_app.refresh_guild_total() {
            eprintln!("Cập nhật tổng số máy chủ: {error}");
        }
        while matches!(
            resync_signal.recv_timeout(wait),
            Err(RecvTimeoutError::Timeout)
        ) {
            wait = sync_interval;
            if let Err(error) = resync_app.refresh_guild_total() {
                eprintln!("Cập nhật tổng số máy chủ: {error}");
            }
            match store::resync(&resync_app.store, &|| SHUTDOWN.load(Ordering::Relaxed)) {
                Ok(report) if report.total() > 0 => eprintln!("Đồng bộ Supabase: {report}"),
                Ok(_) => {}
                Err(_) if SHUTDOWN.load(Ordering::Relaxed) => {}
                Err(error) => eprintln!("Đồng bộ Supabase thất bại, thử lại sau: {error}"),
            }
        }
    });
    let result = gateway::run(Arc::clone(&app));
    SHUTDOWN.store(true, Ordering::Relaxed);
    drop(stop_flush);
    drop(stop_resync);
    let _ = flusher.join();
    let _ = resyncer.join();
    if let Err(error) = app.store.lock().unwrap().flush_stats() {
        eprintln!("Ghi thống kê lúc thoát: {error}");
    }
    let _ = server.join();
    result
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Lỗi khởi động hoặc kết nối: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_shards_start_their_first_sync_later_then_share_the_period() {
        let interval = Duration::from_secs(600);
        assert_eq!(first_sync_delay(interval, 0, 1), interval);
        assert_eq!(first_sync_delay(interval, 0, 4), interval);
        assert_eq!(first_sync_delay(interval, 1, 4), Duration::from_secs(750));
        assert_eq!(first_sync_delay(interval, 3, 4), Duration::from_secs(1050));
        assert!(
            first_sync_delay(Duration::from_secs(86_400), 999, 1000) < Duration::from_secs(172_800)
        );
    }

    #[test]
    fn guild_total_uses_the_shared_count_only_when_sharded_and_never_drops_below_local() {
        use crate::discord::Guild;
        use crate::test_support::{Reply, app, database, dictionary, mock, store};
        let (mut app, _) = mock(database(&[("words", dictionary())]), || app(store()));
        {
            let mut cache = app.discord.cache.write().unwrap();
            for id in ["1", "2"] {
                cache
                    .guilds
                    .insert(id.into(), Guild::from_gateway(json!({"id": id}), "bot"));
            }
        }
        // One shard: the local cache is complete and Discord is not asked.
        assert_eq!(app.refresh_guild_total(), Ok(()));
        assert_eq!(app.total_guilds(), 2);

        app.config.shard_count = 3;
        let replies = vec![
            Reply::json(
                "GET",
                "/api/v10/applications/@me",
                200,
                json!({"approximate_guild_count": 40}),
            ),
            Reply::json("GET", "/api/v10/applications/@me", 200, json!({"id": "1"})),
            Reply::json(
                "GET",
                "/api/v10/applications/@me",
                500,
                json!({"message": "boom"}),
            ),
        ];
        let (results, _) = mock(replies, || {
            let first = app.refresh_guild_total();
            let total = app.total_guilds();
            // A response without the field, or a failure, keeps the last known value.
            let second = app.refresh_guild_total();
            let third = app.refresh_guild_total();
            (first, total, second, third, app.total_guilds())
        });
        assert_eq!((results.0, results.1), (Ok(()), 40));
        assert_eq!((results.2, results.4), (Ok(()), 40));
        assert!(results.3.is_err());
        app.guild_total.store(1, Ordering::Relaxed);
        assert_eq!(app.total_guilds(), 2);
    }

    #[test]
    fn leaving_blocks_new_operations_and_can_be_retried() {
        let activity = Activity::default();
        let op = activity.begin("1").unwrap();
        let leave = activity.leave("1").unwrap();
        assert!(activity.begin("1").is_none());
        assert!(activity.leave("1").is_none());
        assert!(activity.begin("2").is_some());
        drop(op);
        leave.drain();
        activity.allow("1");
        assert!(activity.begin("1").is_none());
        drop(leave);
        assert!(activity.begin("1").is_some());
        activity.leave("1").unwrap().complete();
        assert!(activity.begin("1").is_none());
        activity.allow("1");
        assert!(activity.begin("1").is_some());
    }
}
