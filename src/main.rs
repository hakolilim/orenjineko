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
use std::sync::atomic::{AtomicBool, Ordering};
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
            if let Err(error) = flush_app.store.lock().unwrap().flush_query() {
                eprintln!("Ghi thống kê định kỳ: {error}");
            }
        }
    });
    let result = gateway::run(Arc::clone(&app));
    SHUTDOWN.store(true, Ordering::Relaxed);
    drop(stop_flush);
    let _ = flusher.join();
    if let Err(error) = app.store.lock().unwrap().flush_query() {
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
