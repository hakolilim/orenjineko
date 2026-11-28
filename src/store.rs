use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use crate::http::{Request, encode_component};
use crate::json::Json;

const PAGE: usize = 1000;
const DICTIONARY_URL: &str =
    "https://raw.githubusercontent.com/undertheseanlp/dictionary/master/dictionary/words.txt";
const CONTRIBUTIONS_URL: &str =
    "https://raw.githubusercontent.com/lvdat/phobo-contribute-words/main/accepted-words.txt";

#[derive(Clone)]
struct Database {
    base: String,
    headers: Vec<String>,
}

impl Database {
    fn new(base: &str, key: &str) -> Self {
        let mut headers = vec![format!("apikey: {key}")];
        if !key.starts_with("sb_secret_") && !key.starts_with("sb_publishable_") {
            headers.push(format!("Authorization: Bearer {key}"));
        }
        Self {
            base: format!("{base}/rest/v1"),
            headers,
        }
    }

    fn request(
        &self,
        method: &str,
        table: &str,
        query: &str,
        payload: Option<Json>,
    ) -> Result<Json, String> {
        let mut headers = self.headers.clone();
        let body = payload.map(|payload| payload.encode());
        if body.is_some() {
            headers.push("Content-Type: application/json".into());
            // `return=minimal` would drop the scalar an RPC returns.
            if !table.starts_with("rpc/") {
                headers.push("Prefer: resolution=merge-duplicates,return=minimal".into());
            }
        }
        let response = Request {
            method,
            url: &format!("{}/{table}{query}", self.base),
            headers: &headers,
            body: body.as_deref().map(str::as_bytes),
            timeout: Duration::from_secs(12),
            redirects: false,
        }
        .send()
        .map_err(|error| format!("Supabase {method} {table}: {error}"))?;
        if !response.success() {
            let error = response.json().unwrap_or_default();
            let message = error["message"].str();
            let help = if error["code"].str() == "42501"
                || (response.code == 403 && message.starts_with("permission denied for table "))
            {
                let role = error["hint"]
                    .str()
                    .split_whitespace()
                    .last()
                    .map(|role| role.trim_matches([';', '"']));
                match role {
                    Some("anon" | "authenticated") => {
                        " Dùng Secret key (sb_secret_…) hoặc khóa service_role của cùng project cho SUPABASE_KEY."
                    }
                    Some("service_role") => {
                        " Chạy phần GRANT cuối supabase/schema.sql trong Supabase SQL Editor của project."
                    }
                    _ => {
                        " Kiểm tra SUPABASE_KEY là Secret key hoặc khóa service_role của cùng project, rồi kiểm tra phần GRANT cuối supabase/schema.sql."
                    }
                }
            } else if error["code"].str() == "PGRST202" {
                " Chạy khối increment_stat (tham số stat_key, delta) ở cuối supabase/schema.sql trong Supabase SQL Editor rồi thử lại sau vài giây."
            } else {
                ""
            };
            return Err(format!(
                "Supabase {method} {table}: HTTP {} {message}{help}",
                response.code
            ));
        }
        response.json()
    }

    /// Streams every row of `table` ordered by its primary key `keys`, one page at a time.
    /// Keyset pagination keeps rows that exist throughout the scan from being skipped when
    /// other rows are deleted or inserted meanwhile (offset pagination would shift).
    fn scan(
        &self,
        table: &str,
        keys: &[&str],
        stop: &dyn Fn() -> bool,
        each: &mut dyn FnMut(&Json),
    ) -> Result<(), String> {
        let order = keys.join(",");
        let mut cursor = String::new();
        loop {
            if stop() {
                return Err("Bot đang tắt".into());
            }
            let page = self.request(
                "GET",
                table,
                &format!("?select=*&order={order}&limit={PAGE}{cursor}"),
                None,
            )?;
            let Json::Array(page) = page else {
                return Err(format!("Supabase {table}: phản hồi không phải danh sách"));
            };
            let Some(last) = page.last() else {
                return Ok(());
            };
            let next = after(keys, last);
            if next == cursor {
                return Err(format!("Supabase {table}: con trỏ phân trang không tiến"));
            }
            page.iter().for_each(&mut *each);
            cursor = next;
        }
    }

    /// Atomically adds `delta` to a `global_stats` counter and returns the new database value.
    fn increment(&self, key: &str, delta: u64) -> Result<u64, String> {
        let reply = self.request(
            "POST",
            "rpc/increment_stat",
            "",
            Some(json!({"stat_key": key, "delta": delta})),
        )?;
        // Never fall back to `Json::int`: a missing body would read as 0 and reset the counter.
        match reply {
            Json::Num(value) => value
                .parse()
                .map_err(|_| format!("Supabase increment_stat: giá trị không hợp lệ {value}")),
            _ => Err("Supabase increment_stat: phản hồi không phải số".into()),
        }
    }

    fn upsert(&self, table: &str, row: Json) -> Result<(), String> {
        self.request("POST", table, "", Some(row)).map(|_| ())
    }

    fn delete(&self, table: &str, key: &str, value: &str) -> Result<(), String> {
        self.request(
            "DELETE",
            table,
            &format!("?{key}=eq.{}", encode_component(value)),
            None,
        )
        .map(|_| ())
    }

    fn delete_channels(&self, channels: &BTreeSet<String>) -> Result<(), String> {
        let channels = channels.iter().map(String::as_str).collect::<Vec<_>>();
        for batch in channels.chunks(100) {
            let list = batch
                .iter()
                .map(|id| encode_component(id))
                .collect::<Vec<_>>()
                .join(",");
            self.request(
                "DELETE",
                "game_state",
                &format!("?channel_id=in.({list})"),
                None,
            )?;
        }
        Ok(())
    }
}

/// Query-string cursor selecting the rows strictly after `row` in primary-key order.
pub(crate) fn after(keys: &[&str], row: &Json) -> String {
    match keys {
        [key] => format!("&{key}=gt.{}", encode_component(row[*key].str())),
        [first, second] => {
            let (a, b) = (
                filter_value(row[*first].str()),
                filter_value(row[*second].str()),
            );
            format!("&or=({first}.gt.{a},and({first}.eq.{a},{second}.gt.{b}))")
        }
        _ => unreachable!("khoá chính chỉ có một hoặc hai cột"),
    }
}

/// Value for use inside a PostgREST `or=(...)` list: plain identifiers stay as they are,
/// anything else is double-quoted so `,` `(` `)` `.` cannot change the filter structure.
fn filter_value(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if plain {
        value.to_owned()
    } else {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("%22{}%22", encode_component(&escaped))
    }
}

#[derive(Default)]
pub struct Dictionary {
    pub words: BTreeSet<String>,
    pub blacklist: HashSet<String>,
    active: BTreeMap<String, BTreeSet<String>>,
    active_count: usize,
    openings: Vec<String>,
}

impl Dictionary {
    fn index(&mut self) {
        self.active.clear();
        self.active_count = 0;
        for word in &self.words {
            if !self.blacklist.contains(word) {
                let first = word.split_whitespace().next().unwrap_or_default();
                self.active
                    .entry(first.to_owned())
                    .or_default()
                    .insert(word.clone());
                self.active_count += 1;
            }
        }
        self.openings = self
            .active
            .values()
            .flatten()
            .filter(|word| word.split_whitespace().count() == 2 && self.next(word).next().is_some())
            .cloned()
            .collect();
    }

    pub fn count(&self) -> usize {
        self.active_count
    }

    pub fn contains(&self, word: &str) -> bool {
        self.words.contains(word) && !self.blacklist.contains(word)
    }

    pub fn next(&self, word: &str) -> impl Iterator<Item = &str> {
        let last = word.split_whitespace().next_back().unwrap_or_default();
        self.active
            .get(last)
            .into_iter()
            .flatten()
            .map(String::as_str)
            .filter(move |candidate| *candidate != word && candidate.split_whitespace().count() > 1)
    }

    pub fn opening(&self) -> Option<(String, u64)> {
        if self.openings.is_empty() {
            return None;
        }
        let word = &self.openings[random_index(self.openings.len())];
        Some((word.clone(), self.count() as u64))
    }
}

pub fn random_index(length: usize) -> usize {
    use std::cell::Cell;
    use std::hash::{BuildHasher, RandomState};
    thread_local! {
        static SEED: Cell<u64> = Cell::new(RandomState::new().hash_one("orenjineko") | 1);
    }
    SEED.with(|seed| {
        let mut value = seed.get();
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        seed.set(value);
        (value % length as u64) as usize
    })
}

#[derive(Clone, PartialEq)]
pub struct Mode {
    pub guild: String,
    pub bot: bool,
}

#[derive(Clone, Default, PartialEq)]
pub struct Counting {
    pub guild: String,
    pub count: String,
    pub last: Option<String>,
}

#[derive(Clone, Default, PartialEq)]
pub struct Game {
    pub running: bool,
    pub player: Option<String>,
    pub name: Option<String>,
    pub words: Vec<String>,
}

#[derive(Clone, Default, PartialEq)]
pub struct Player {
    pub name: Option<String>,
    pub avatar: Option<String>,
    pub wins: i64,
    pub total: i64,
    pub correct: i64,
}

impl Player {
    pub fn accuracy(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.correct as f64 / self.total as f64
        }
    }
}

/// The three `global_stats` counters. `query` is batched in RAM and flushed periodically;
/// every counter is only ever written to Supabase as an atomic delta (`increment_stat`).
#[derive(Clone, Copy)]
enum Stat {
    Query,
    Words,
    Rounds,
}

impl Stat {
    const ALL: [Stat; 3] = [Stat::Query, Stat::Words, Stat::Rounds];

    fn key(self) -> &'static str {
        match self {
            Stat::Query => "query",
            Stat::Words => "word_played",
            Stat::Rounds => "round_played",
        }
    }
}

/// Everything read from Supabase, decoded but not yet applied to a `Store`.
#[derive(Default)]
struct Snapshot {
    words: BTreeSet<String>,
    blacklist: HashSet<String>,
    configs: HashMap<String, Mode>,
    counting: HashMap<String, Counting>,
    games: HashMap<String, Game>,
    ranks: HashMap<String, HashMap<String, Player>>,
    premium: HashSet<String>,
    stats: [u64; 3],
}

fn id(row: &Json, key: &str) -> String {
    row[key].str().to_owned()
}
fn optional(row: &Json, key: &str) -> Option<String> {
    row[key].opt_str().map(str::to_owned)
}

impl Snapshot {
    /// Reads every table. The table order is part of the contract with the test transport.
    fn fetch(db: &Database, stop: &dyn Fn() -> bool) -> Result<Self, String> {
        let mut snapshot = Snapshot::default();
        db.scan("words", &["word"], stop, &mut |row| {
            snapshot.words.insert(id(row, "word"));
        })?;
        db.scan("report_words", &["word"], stop, &mut |row| {
            snapshot.blacklist.insert(id(row, "word"));
        })?;
        db.scan("guild_config", &["channel_id"], stop, &mut |row| {
            snapshot.configs.insert(
                id(row, "channel_id"),
                Mode {
                    guild: id(row, "guild_id"),
                    bot: row["bot_mode"].bool(),
                },
            );
        })?;
        db.scan("counting_channels", &["channel_id"], stop, &mut |row| {
            snapshot.counting.insert(
                id(row, "channel_id"),
                Counting {
                    guild: id(row, "guild_id"),
                    count: id(row, "current_count"),
                    last: optional(row, "last_player_id"),
                },
            );
        })?;
        db.scan("game_state", &["channel_id"], stop, &mut |row| {
            snapshot.games.insert(
                id(row, "channel_id"),
                Game {
                    running: row["running"].bool(),
                    player: optional(row, "current_player_id"),
                    name: optional(row, "current_player_name"),
                    words: row["words"]
                        .array()
                        .iter()
                        .filter_map(Json::opt_str)
                        .map(str::to_owned)
                        .collect(),
                },
            );
        })?;
        db.scan("rankings", &["guild_id", "user_id"], stop, &mut |row| {
            snapshot
                .ranks
                .entry(id(row, "guild_id"))
                .or_default()
                .insert(
                    id(row, "user_id"),
                    Player {
                        name: optional(row, "name"),
                        avatar: optional(row, "avatar"),
                        wins: row["win"].int(),
                        total: row["total"].int(),
                        correct: row["true_count"].int(),
                    },
                );
        })?;
        db.scan("global_stats", &["key"], stop, &mut |row| {
            if let Some(stat) = Stat::ALL.iter().find(|stat| stat.key() == row["key"].str()) {
                snapshot.stats[*stat as usize] =
                    u64::try_from(row["value"].int()).unwrap_or_default();
            }
        })?;
        db.scan("premium_guilds", &["guild_id"], stop, &mut |row| {
            snapshot.premium.insert(id(row, "guild_id"));
        })?;
        Ok(snapshot)
    }
}

/// Keys written locally while a resync is reading Supabase. A snapshot may predate those
/// writes, so on apply the RAM value of every touched key wins over the snapshot.
///
/// Every `Store` method that changes RAM after a successful Supabase write must call
/// `Store::touch`; a mutator that forgets only gets its change reverted by a stale snapshot.
#[derive(Default)]
struct Touched {
    words: HashSet<String>,
    blacklist: HashSet<String>,
    configs: HashSet<String>,
    counting: HashSet<String>,
    games: HashSet<String>,
    ranks: HashSet<(String, String)>,
    // `remove_guild` deletes whole guilds table by table and can fail halfway, so each
    // table records its own removed guilds.
    config_guilds: HashSet<String>,
    counting_guilds: HashSet<String>,
    rank_guilds: HashSet<String>,
    premium_guilds: HashSet<String>,
    stats: [bool; 3],
}

/// How many RAM entries a resync added, removed or changed, per table.
#[derive(Debug, Default)]
pub struct ResyncReport {
    words: usize,
    blacklist: usize,
    configs: usize,
    counting: usize,
    games: usize,
    ranks: usize,
    premium: usize,
    stats: usize,
}

impl ResyncReport {
    fn parts(&self) -> [(&'static str, usize); 8] {
        [
            ("từ điển", self.words),
            ("danh sách đen", self.blacklist),
            ("kênh nối từ", self.configs),
            ("kênh đếm số", self.counting),
            ("ván nối từ", self.games),
            ("xếp hạng", self.ranks),
            ("premium", self.premium),
            ("bộ đếm", self.stats),
        ]
    }

    pub fn total(&self) -> usize {
        self.parts().iter().map(|(_, count)| count).sum()
    }
}

impl std::fmt::Display for ResyncReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts = self
            .parts()
            .iter()
            .filter(|(_, count)| *count > 0)
            .map(|(name, count)| format!("{name} {count}"))
            .collect::<Vec<_>>()
            .join(", ");
        write!(formatter, "{} thay đổi ({parts})", self.total())
    }
}

pub struct Store {
    db: Database,
    pub dictionary: Dictionary,
    pub configs: HashMap<String, Mode>,
    pub counting: HashMap<String, Counting>,
    pub games: HashMap<String, Game>,
    pub ranks: HashMap<String, HashMap<String, Player>>,
    pub premium: HashSet<String>,
    pub query: u64,
    pub words: u64,
    pub rounds: u64,
    /// Counter deltas already added to the RAM values above but not yet confirmed by Supabase.
    pending: [u64; 3],
    touched: Option<Touched>,
}

impl Store {
    pub fn load(url: &str, key: &str) -> Result<Self, String> {
        let db = Database::new(url, key);
        let mut snapshot = Snapshot::fetch(&db, &|| false)?;
        if snapshot.words.is_empty() {
            snapshot.words = seed(&db)?;
        }
        Ok(Self::from_snapshot(db, snapshot))
    }

    fn from_snapshot(db: Database, snapshot: Snapshot) -> Self {
        let mut dictionary = Dictionary {
            words: snapshot.words,
            blacklist: snapshot.blacklist,
            ..Default::default()
        };
        dictionary.index();
        Self {
            db,
            dictionary,
            configs: snapshot.configs,
            counting: snapshot.counting,
            games: snapshot.games,
            ranks: snapshot.ranks,
            premium: snapshot.premium,
            query: snapshot.stats[Stat::Query as usize],
            words: snapshot.stats[Stat::Words as usize],
            rounds: snapshot.stats[Stat::Rounds as usize],
            pending: [0; 3],
            touched: None,
        }
    }

    fn touch(&mut self, mark: impl FnOnce(&mut Touched)) {
        if let Some(touched) = self.touched.as_mut() {
            mark(touched);
        }
    }

    pub fn add_word(&mut self, word: &str, blacklist: bool) -> Result<(), String> {
        let table = if blacklist { "report_words" } else { "words" };
        self.db.upsert(table, json!({"word": word}))?;
        if blacklist {
            self.dictionary.blacklist.insert(word.into());
            self.touch(|touched| {
                touched.blacklist.insert(word.into());
            });
        } else {
            self.dictionary.words.insert(word.into());
            self.touch(|touched| {
                touched.words.insert(word.into());
            });
        }
        self.dictionary.index();
        Ok(())
    }

    pub fn unblacklist(&mut self, word: &str) -> Result<bool, String> {
        if !self.dictionary.blacklist.contains(word) {
            return Ok(false);
        }
        self.db.delete("report_words", "word", word)?;
        self.dictionary.blacklist.remove(word);
        self.touch(|touched| {
            touched.blacklist.insert(word.into());
        });
        self.dictionary.index();
        Ok(true)
    }

    pub fn set_word_channel(
        &mut self,
        guild: &str,
        channel: &str,
        bot: bool,
    ) -> Result<(), String> {
        self.db.upsert(
            "guild_config",
            json!({"channel_id": channel, "guild_id": guild, "bot_mode": bot}),
        )?;
        self.configs.insert(
            channel.into(),
            Mode {
                guild: guild.into(),
                bot,
            },
        );
        self.touch(|touched| {
            touched.configs.insert(channel.into());
        });
        self.remove_counting(channel)?;
        Ok(())
    }

    pub fn set_counting_channel(&mut self, guild: &str, channel: &str) -> Result<String, String> {
        if !self.counting.contains_key(channel) {
            self.save_counter(
                channel,
                Counting {
                    guild: guild.into(),
                    count: "0".into(),
                    last: None,
                },
            )?;
        }
        self.remove_word_channel(channel)?;
        Ok(self.counting[channel].count.clone())
    }

    fn remove_counting(&mut self, channel: &str) -> Result<(), String> {
        if self.counting.contains_key(channel) {
            self.db.delete("counting_channels", "channel_id", channel)?;
            self.counting.remove(channel);
            self.touch(|touched| {
                touched.counting.insert(channel.into());
            });
        }
        Ok(())
    }

    fn remove_word_channel(&mut self, channel: &str) -> Result<(), String> {
        if self.games.contains_key(channel) {
            self.db.delete("game_state", "channel_id", channel)?;
            self.games.remove(channel);
            self.touch(|touched| {
                touched.games.insert(channel.into());
            });
        }
        if self.configs.contains_key(channel) {
            self.db.delete("guild_config", "channel_id", channel)?;
            self.configs.remove(channel);
            self.touch(|touched| {
                touched.configs.insert(channel.into());
            });
        }
        Ok(())
    }

    pub fn unset(&mut self, channel: &str) -> Result<Option<&'static str>, String> {
        let mode = if self.counting.contains_key(channel) {
            "đếm số"
        } else if self.configs.contains_key(channel) {
            "nối từ"
        } else {
            return Ok(None);
        };
        self.remove_word_channel(channel)?;
        self.remove_counting(channel)?;
        Ok(Some(mode))
    }

    /// Every configured channel of every guild, not only this shard's: each shard mirrors all of
    /// Supabase, which is what lets `/broadcast` reach all guilds from whichever shard runs it
    /// (the ones owned by other shards are as fresh as the last resync).
    pub fn channel_configs(&self) -> BTreeMap<String, String> {
        self.configs
            .iter()
            .map(|(channel, config)| (channel.clone(), config.guild.clone()))
            .chain(
                self.counting
                    .iter()
                    .map(|(channel, config)| (channel.clone(), config.guild.clone())),
            )
            .collect()
    }

    fn save_counter(&mut self, channel: &str, counter: Counting) -> Result<(), String> {
        self.db.upsert("counting_channels", json!({
            "channel_id": channel, "guild_id": (counter.guild.as_str()),
            "current_count": (counter.count.as_str()), "last_player_id": (counter.last.as_deref())
        }))?;
        self.counting.insert(channel.into(), counter);
        self.touch(|touched| {
            touched.counting.insert(channel.into());
        });
        Ok(())
    }

    pub fn submit_count(
        &mut self,
        channel: &str,
        text: &str,
        user: &str,
    ) -> Result<Option<CountResult>, String> {
        let Some(previous) = self.counting.get(channel) else {
            return Ok(None);
        };
        let Some((counter, result)) = count_move(previous, text, user) else {
            return Ok(None);
        };
        self.save_counter(channel, counter)?;
        Ok(Some(result))
    }

    pub fn save_game(&mut self, channel: &str, game: Game) -> Result<(), String> {
        self.db.upsert("game_state", json!({
            "channel_id": channel, "running": (game.running), "current_player_id": (game.player.as_deref()),
            "current_player_name": (game.name.as_deref()), "words": (game.words.clone())
        }))?;
        self.games.insert(channel.into(), game);
        self.touch(|touched| {
            touched.games.insert(channel.into());
        });
        Ok(())
    }

    pub fn save_player(&mut self, guild: &str, user: &str, player: Player) -> Result<(), String> {
        self.db.upsert("rankings", json!({
            "guild_id": guild, "user_id": user, "win": (player.wins), "total": (player.total),
            "true_count": (player.correct), "name": (player.name.as_deref()), "avatar": (player.avatar.as_deref())
        }))?;
        self.ranks
            .entry(guild.into())
            .or_default()
            .insert(user.into(), player);
        self.touch(|touched| {
            touched.ranks.insert((guild.into(), user.into()));
        });
        Ok(())
    }

    pub fn player(&self, guild: &str, user: &str) -> Option<&Player> {
        self.ranks.get(guild)?.get(user)
    }

    pub fn players(&self) -> usize {
        self.ranks.values().map(HashMap::len).sum()
    }

    fn slot(&mut self, stat: Stat) -> &mut u64 {
        match stat {
            Stat::Query => &mut self.query,
            Stat::Words => &mut self.words,
            Stat::Rounds => &mut self.rounds,
        }
    }

    /// Adds to the RAM counter and remembers the delta until Supabase confirms it.
    fn bump(&mut self, stat: Stat, amount: u64) {
        *self.slot(stat) += amount;
        self.pending[stat as usize] += amount;
    }

    /// Sends the unconfirmed delta as an atomic increment and adopts the total Supabase
    /// returns, which also includes increments from other processes. A failed call keeps
    /// the delta for the next attempt.
    fn flush_stat(&mut self, stat: Stat) -> Result<(), String> {
        let pending = self.pending[stat as usize];
        if pending == 0 {
            return Ok(());
        }
        let total = self.db.increment(stat.key(), pending)?;
        self.pending[stat as usize] = 0;
        *self.slot(stat) = total;
        self.touch(|touched| touched.stats[stat as usize] = true);
        Ok(())
    }

    pub fn add_query(&mut self, amount: u64) {
        self.bump(Stat::Query, amount);
    }

    /// Stops at the first failure so an unreachable database blocks the store lock for one
    /// request timeout, not three. The remaining counters are retried on the next call.
    pub fn flush_stats(&mut self) -> Result<(), String> {
        Stat::ALL
            .into_iter()
            .try_for_each(|stat| self.flush_stat(stat))
    }

    pub fn add_words(&mut self, count: u64) -> Result<(), String> {
        self.bump(Stat::Words, count);
        self.flush_stat(Stat::Words)
    }

    pub fn add_round(&mut self) -> Result<(), String> {
        self.bump(Stat::Rounds, 1);
        self.flush_stat(Stat::Rounds)
    }

    pub fn remove_guild(
        &mut self,
        guild: &str,
        channels: impl IntoIterator<Item = String>,
    ) -> Result<(), String> {
        let channels = channels
            .into_iter()
            .chain(
                self.channel_configs()
                    .into_iter()
                    .filter(|(_, owner)| owner == guild)
                    .map(|(channel, _)| channel),
            )
            .collect::<BTreeSet<_>>();
        self.db.delete_channels(&channels)?;
        self.games.retain(|channel, _| !channels.contains(channel));
        self.touch(|touched| touched.games.extend(channels.iter().cloned()));
        self.db.delete("counting_channels", "guild_id", guild)?;
        self.counting.retain(|_, value| value.guild != guild);
        self.touch(|touched| {
            touched.counting_guilds.insert(guild.into());
        });
        self.db.delete("rankings", "guild_id", guild)?;
        self.ranks.remove(guild);
        self.touch(|touched| {
            touched.rank_guilds.insert(guild.into());
        });
        self.db.delete("premium_guilds", "guild_id", guild)?;
        self.premium.remove(guild);
        self.touch(|touched| {
            touched.premium_guilds.insert(guild.into());
        });
        self.db.delete("guild_config", "guild_id", guild)?;
        self.configs.retain(|_, value| value.guild != guild);
        self.touch(|touched| {
            touched.config_guilds.insert(guild.into());
        });
        Ok(())
    }

    fn begin_resync(&mut self) -> Database {
        self.touched = Some(Touched::default());
        self.db.clone()
    }

    /// Replaces RAM with the snapshot, except for keys written locally while it was being
    /// fetched (those keep their RAM value) and unconfirmed counter deltas (added on top).
    /// Rows missing from the snapshot are removed, so RAM mirrors Supabase exactly.
    fn apply(&mut self, snapshot: Result<Snapshot, String>) -> Result<ResyncReport, String> {
        // Always stop tracking, including on the error paths below.
        let touched = self.touched.take().unwrap_or_default();
        let mut snapshot = snapshot?;
        if snapshot.words.is_empty() {
            return Err("Supabase trả về từ điển rỗng, giữ nguyên dữ liệu trong RAM".into());
        }
        let mut report = ResyncReport::default();

        for word in &touched.words {
            if self.dictionary.words.contains(word) {
                snapshot.words.insert(word.clone());
            } else {
                snapshot.words.remove(word);
            }
        }
        for word in &touched.blacklist {
            if self.dictionary.blacklist.contains(word) {
                snapshot.blacklist.insert(word.clone());
            } else {
                snapshot.blacklist.remove(word);
            }
        }
        report.words = self
            .dictionary
            .words
            .symmetric_difference(&snapshot.words)
            .count();
        report.blacklist = self
            .dictionary
            .blacklist
            .symmetric_difference(&snapshot.blacklist)
            .count();
        if report.words + report.blacklist > 0 {
            self.dictionary.words = snapshot.words;
            self.dictionary.blacklist = snapshot.blacklist;
            self.dictionary.index();
        }

        report.configs = merge(
            &mut self.configs,
            snapshot.configs,
            &touched.configs,
            &touched.config_guilds,
            |mode| &mode.guild,
        );
        report.counting = merge(
            &mut self.counting,
            snapshot.counting,
            &touched.counting,
            &touched.counting_guilds,
            |counting| &counting.guild,
        );
        // `game_state` has no guild column: removed guilds are covered by their channels.
        report.games = merge(
            &mut self.games,
            snapshot.games,
            &touched.games,
            &HashSet::new(),
            |_| "",
        );
        report.ranks = merge_ranks(&mut self.ranks, snapshot.ranks, &touched);

        snapshot
            .premium
            .retain(|guild| !touched.premium_guilds.contains(guild));
        report.premium = self.premium.symmetric_difference(&snapshot.premium).count();
        self.premium = snapshot.premium;

        for stat in Stat::ALL {
            let index = stat as usize;
            if touched.stats[index] {
                continue;
            }
            let value = snapshot.stats[index] + self.pending[index];
            let slot = self.slot(stat);
            if *slot != value {
                *slot = value;
                report.stats += 1;
            }
        }
        Ok(report)
    }
}

/// Replaces `ram` with `snapshot`, keeping the RAM value of every `touched` key and dropping
/// snapshot rows whose guild was removed locally. Returns the number of entries that differ.
fn merge<V: PartialEq>(
    ram: &mut HashMap<String, V>,
    mut snapshot: HashMap<String, V>,
    touched: &HashSet<String>,
    removed_guilds: &HashSet<String>,
    guild_of: impl Fn(&V) -> &str,
) -> usize {
    let mut old = std::mem::take(ram);
    snapshot
        .retain(|key, value| !touched.contains(key) && !removed_guilds.contains(guild_of(value)));
    let mut changes = old
        .iter()
        .filter(|(key, value)| !touched.contains(*key) && snapshot.get(*key) != Some(*value))
        .count();
    changes += snapshot
        .keys()
        .filter(|key| !old.contains_key(*key))
        .count();
    for key in touched {
        if let Some(value) = old.remove(key) {
            snapshot.insert(key.clone(), value);
        }
    }
    *ram = snapshot;
    changes
}

fn merge_ranks(
    ram: &mut HashMap<String, HashMap<String, Player>>,
    mut snapshot: HashMap<String, HashMap<String, Player>>,
    touched: &Touched,
) -> usize {
    let is_touched = |guild: &str, user: &str| {
        !touched.ranks.is_empty() && touched.ranks.contains(&(guild.to_owned(), user.to_owned()))
    };
    let mut old = std::mem::take(ram);
    snapshot.retain(|guild, _| !touched.rank_guilds.contains(guild));
    if !touched.ranks.is_empty() {
        for (guild, users) in &mut snapshot {
            users.retain(|user, _| !is_touched(guild, user));
        }
    }
    let mut changes = 0;
    for (guild, users) in &old {
        for (user, player) in users {
            if !is_touched(guild, user)
                && snapshot.get(guild).and_then(|users| users.get(user)) != Some(player)
            {
                changes += 1;
            }
        }
    }
    for (guild, users) in &snapshot {
        changes += users
            .keys()
            .filter(|user| {
                !old.get(guild)
                    .is_some_and(|users| users.contains_key(*user))
            })
            .count();
    }
    for (guild, user) in &touched.ranks {
        if let Some(player) = old.get_mut(guild).and_then(|users| users.remove(user)) {
            snapshot
                .entry(guild.clone())
                .or_default()
                .insert(user.clone(), player);
        }
    }
    snapshot.retain(|_, users| !users.is_empty());
    *ram = snapshot;
    changes
}

/// Re-reads Supabase and mirrors it into RAM. The slow network reads run without the store
/// lock; only `begin_resync` and `apply` take it, briefly.
pub fn resync(store: &Mutex<Store>, stop: &dyn Fn() -> bool) -> Result<ResyncReport, String> {
    let db = store.lock().unwrap().begin_resync();
    let snapshot = Snapshot::fetch(&db, stop);
    store.lock().unwrap().apply(snapshot)
}

fn seed(db: &Database) -> Result<BTreeSet<String>, String> {
    let get = |url: &str| {
        let response = Request {
            method: "GET",
            url,
            headers: &[],
            body: None,
            timeout: Duration::from_secs(30),
            redirects: true,
        }
        .send()?;
        if !response.success() {
            return Err(format!("HTTP {}", response.code));
        }
        String::from_utf8(response.body).map_err(|_| "Từ điển không phải UTF-8".into())
    };
    let mut words = BTreeSet::new();
    for line in get(DICTIONARY_URL)?
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let data = Json::parse(line.as_bytes())?;
        let word = data["text"].str().to_lowercase();
        if word.split(' ').count() == 2 && !word.contains(['-', '(', ')']) {
            words.insert(word);
        }
    }
    match get(CONTRIBUTIONS_URL) {
        Ok(contributions) => words.extend(
            contributions
                .lines()
                .map(|word| word.trim().to_lowercase())
                .filter(|word| !word.is_empty()),
        ),
        Err(error) => eprintln!("Không tải được từ đóng góp: {error}"),
    }
    if words.is_empty() {
        return Err("Từ điển nguồn trống".into());
    }
    let rows = words
        .iter()
        .map(|word| json!({"word": word}))
        .collect::<Vec<_>>();
    for batch in rows.chunks(PAGE) {
        db.upsert("words", Json::Array(batch.to_vec()))?;
    }
    Ok(words)
}

pub fn normalize_word(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub fn increment(count: &str) -> String {
    let mut digits = count.as_bytes().to_vec();
    for digit in digits.iter_mut().rev() {
        if *digit < b'9' {
            *digit += 1;
            return String::from_utf8(digits).expect("Bộ đếm chỉ gồm chữ số ASCII");
        }
        *digit = b'0';
    }
    digits.insert(0, b'1');
    String::from_utf8(digits).expect("Bộ đếm chỉ gồm chữ số ASCII")
}

#[derive(Debug, PartialEq)]
pub enum CountResult {
    Correct,
    WrongNumber,
    SamePlayer,
}

fn count_move(previous: &Counting, content: &str, user: &str) -> Option<(Counting, CountResult)> {
    let text = content.trim();
    let number = text.strip_prefix(['+', '-']).unwrap_or(text);
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let is_number = match number.split_once(['.', ',']) {
        Some((whole, fractional)) => digits(whole) && digits(fractional),
        None => digits(number),
    };
    if !is_number {
        return None;
    }
    let expected = increment(&previous.count);
    let result = if previous.last.as_deref() == Some(user) {
        CountResult::SamePlayer
    } else if digits(text) && text.trim_start_matches('0') == expected {
        CountResult::Correct
    } else {
        CountResult::WrongNumber
    };
    let mut next = previous.clone();
    if result == CountResult::Correct {
        next.count = expected;
        next.last = Some(user.into());
    } else {
        next.count = "0".into();
        next.last = None;
    }
    Some((next, result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counting_preserves_unbounded_numbers_and_player_turns() {
        let count = "9".repeat(120);
        let state = Counting {
            count,
            last: Some("a".into()),
            guild: "guild".into(),
        };
        let expected = format!("1{}", "0".repeat(120));
        let (new, result) = count_move(&state, &format!("00{expected}"), "b").unwrap();
        assert_eq!(result, CountResult::Correct);
        assert_eq!(new.count, expected);
        for (input, user, result) in [
            ("12", "a", CountResult::SamePlayer),
            ("+12", "b", CountResult::WrongNumber),
            ("1,2", "b", CountResult::WrongNumber),
        ] {
            let (new, actual) = count_move(&state, input, user).unwrap();
            assert_eq!(result, actual);
            assert_eq!(new.count, "0");
            assert!(new.last.is_none());
        }
        assert!(count_move(&state, "12 apples", "a").is_none());
    }

    #[test]
    fn dictionary_filters_blacklist_and_works_with_no_opening() {
        let mut dictionary = Dictionary::default();
        dictionary
            .words
            .extend(["học sinh".into(), "sinh học".into(), "sinh vật".into()]);
        dictionary.blacklist.insert("sinh vật".into());
        dictionary.index();
        assert_eq!(
            dictionary.next("học sinh").collect::<Vec<_>>(),
            ["sinh học"]
        );
        assert!(dictionary.opening().is_some());
        dictionary.blacklist.insert("sinh học".into());
        dictionary.index();
        assert!(dictionary.opening().is_none());
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use crate::test_support::{Reply, database, dictionary, mock, store};

    #[test]
    fn sends_supabase_secret_keys_in_apikey_and_legacy_keys_in_both_headers() {
        let path = "/rest/v1/words";
        let (_, requests) = mock(
            vec![
                Reply::json("GET", path, 200, json!([])),
                Reply::json("GET", path, 200, json!([])),
            ],
            || {
                Database::new("http://localhost", "sb_secret_test")
                    .request("GET", "words", "", None)
                    .unwrap();
                Database::new("http://localhost", "legacy-jwt")
                    .request("GET", "words", "", None)
                    .unwrap();
            },
        );
        assert_eq!(requests[0].headers, ["apikey: sb_secret_test"]);
        assert_eq!(
            requests[1].headers,
            ["apikey: legacy-jwt", "Authorization: Bearer legacy-jwt"]
        );
    }

    #[test]
    fn permission_error_distinguishes_missing_grants_from_wrong_key() {
        for (role, expected) in [
            ("service_role", "phần GRANT cuối supabase/schema.sql"),
            ("anon", "cùng project cho SUPABASE_KEY"),
        ] {
            let reply = Reply::json(
                "GET",
                "/rest/v1/words",
                403,
                json!({
                    "message": "permission denied for table words",
                    "code": "42501",
                    "hint": (format!("Grant the required privileges to the current role with: GRANT SELECT ON public.words TO {role};"))
                }),
            );
            let (error, _) = mock(vec![reply], || {
                Database::new("http://localhost", "test-key")
                    .request("GET", "words", "", None)
                    .unwrap_err()
            });
            assert!(error.contains("HTTP 403 permission denied for table words"));
            assert!(error.contains(expected));
            assert!(!error.contains("test-key"));
        }
    }

    #[test]
    fn counting_write_failure_keeps_previous_turn_and_retry_updates_it() {
        let initial = "999999999999999999999999999999";
        let next = "1000000000000000000000000000000";
        let mut replies = database(&[
            ("words", dictionary()),
            (
                "counting_channels",
                vec![json!({
                    "channel_id": "count", "guild_id": "guild",
                    "current_count": initial, "last_player_id": "old"
                })],
            ),
        ]);
        replies.extend([
            Reply::json(
                "POST",
                "/rest/v1/counting_channels",
                503,
                json!({"message": "unavailable"}),
            ),
            Reply::empty("POST", "/rest/v1/counting_channels"),
        ]);
        let (_, requests) = mock(replies, || {
            let mut store = store();
            assert!(store.submit_count("count", next, "new").is_err());
            assert_eq!(store.counting["count"].count, initial);
            assert_eq!(store.counting["count"].last.as_deref(), Some("old"));
            assert_eq!(
                store.submit_count("count", next, "new").unwrap(),
                Some(CountResult::Correct)
            );
            assert_eq!(store.counting["count"].count, next);
        });
        assert_eq!(
            requests[0]
                .headers
                .iter()
                .find(|header| header.starts_with("apikey:"))
                .unwrap(),
            "apikey: supabase-test-key"
        );
        assert_eq!(requests.last().unwrap().body["current_count"].str(), next);
    }

    #[test]
    fn guild_cleanup_can_retry_with_configured_channels_and_preserves_global_data() {
        let mut replies = database(&[
            ("words", dictionary()),
            (
                "guild_config",
                vec![json!({"channel_id": "configured", "guild_id": "one", "bot_mode": false})],
            ),
            (
                "counting_channels",
                vec![
                    json!({"channel_id": "count", "guild_id": "one", "current_count": "1", "last_player_id": "user"}),
                ],
            ),
            (
                "game_state",
                vec![json!({"channel_id": "configured", "running": true, "words": ["học sinh"]})],
            ),
            (
                "rankings",
                vec![
                    json!({"guild_id": "one", "user_id": "user"}),
                    json!({"guild_id": "two", "user_id": "user"}),
                ],
            ),
            (
                "global_stats",
                vec![json!({"key": "word_played", "value": 42})],
            ),
            ("premium_guilds", vec![json!({"guild_id": "one"})]),
        ]);
        replies.extend([
            Reply::empty(
                "DELETE",
                "/rest/v1/game_state?channel_id=in.(configured,count,live)",
            ),
            Reply::empty("DELETE", "/rest/v1/counting_channels?guild_id=eq.one"),
            Reply::json(
                "DELETE",
                "/rest/v1/rankings?guild_id=eq.one",
                500,
                json!({"message": "failure"}),
            ),
            Reply::empty(
                "DELETE",
                "/rest/v1/game_state?channel_id=in.(configured,live)",
            ),
            Reply::empty("DELETE", "/rest/v1/counting_channels?guild_id=eq.one"),
            Reply::empty("DELETE", "/rest/v1/rankings?guild_id=eq.one"),
            Reply::empty("DELETE", "/rest/v1/premium_guilds?guild_id=eq.one"),
            Reply::empty("DELETE", "/rest/v1/guild_config?guild_id=eq.one"),
        ]);
        mock(replies, || {
            let mut store = store();
            let channels = ["live".to_owned()];
            assert!(store.remove_guild("one", channels.clone()).is_err());
            assert!(store.configs.contains_key("configured"));
            assert!(store.player("one", "user").is_some());
            store.remove_guild("one", channels).unwrap();
            assert!(!store.configs.contains_key("configured"));
            assert!(!store.premium.contains("one"));
            assert!(store.player("one", "user").is_none());
            assert!(store.player("two", "user").is_some());
            assert_eq!(store.words, 42);
            assert_eq!(store.dictionary.count(), 3);
        });
    }
}

#[cfg(test)]
mod resync_tests {
    use super::*;
    use crate::test_support::{Reply, database, dictionary, mock, page, store};
    use std::cell::Cell;

    type Fixtures = Vec<(&'static str, Vec<Json>)>;

    /// Supabase contents shared by most tests: one word channel with a game, one counting
    /// channel and some rankings in guild `g1`, a ranking in `g2`, and premium for `g1`.
    fn base() -> Fixtures {
        vec![
            ("words", dictionary()),
            (
                "guild_config",
                vec![json!({"channel_id": "c1", "guild_id": "g1", "bot_mode": false})],
            ),
            (
                "counting_channels",
                vec![
                    json!({"channel_id": "k1", "guild_id": "g1", "current_count": "1", "last_player_id": "u1"}),
                ],
            ),
            (
                "game_state",
                vec![
                    json!({"channel_id": "c1", "running": true, "words": ["học sinh"], "current_player_id": null}),
                ],
            ),
            (
                "rankings",
                vec![
                    json!({"guild_id": "g1", "user_id": "u1", "win": 1, "total": 2, "true_count": 1, "name": "Lan"}),
                    json!({"guild_id": "g2", "user_id": "u3", "win": 0, "total": 0, "true_count": 0}),
                ],
            ),
            (
                "global_stats",
                vec![
                    json!({"key": "query", "value": 10}),
                    json!({"key": "word_played", "value": 3}),
                    json!({"key": "round_played", "value": 1}),
                ],
            ),
            ("premium_guilds", vec![json!({"guild_id": "g1"})]),
        ]
    }

    fn with(mut fixtures: Fixtures, table: &'static str, rows: Vec<Json>) -> Fixtures {
        fixtures.retain(|(name, _)| *name != table);
        fixtures.push((table, rows));
        fixtures
    }

    /// Runs `act` as a local write that lands after the resync already scanned Supabase, so
    /// the snapshot applied afterwards is stale for the keys `act` changed.
    fn during_resync(
        fixtures: &Fixtures,
        writes: Vec<Reply>,
        act: impl FnOnce(&mut Store),
    ) -> Store {
        let mut replies = database(fixtures);
        replies.extend(database(fixtures));
        replies.extend(writes);
        mock(replies, || {
            let mut store = store();
            let db = store.begin_resync();
            let stale = Snapshot::fetch(&db, &|| false);
            act(&mut store);
            store.apply(stale).unwrap();
            assert!(store.touched.is_none());
            store
        })
        .0
    }

    fn write(method: &'static str, path: &str) -> Reply {
        Reply::empty(method, format!("/rest/v1/{path}"))
    }

    #[test]
    fn keyset_cursors_are_encoded_and_quoted_for_postgrest() {
        assert_eq!(
            after(&["word"], &json!({"word": "học sinh"})),
            "&word=gt.h%E1%BB%8Dc%20sinh"
        );
        assert_eq!(
            after(
                &["guild_id", "user_id"],
                &json!({"guild_id": "1", "user_id": "2"})
            ),
            "&or=(guild_id.gt.1,and(guild_id.eq.1,user_id.gt.2))"
        );
        assert_eq!(
            after(&["a", "b"], &json!({"a": "x,y", "b": "p\"q\\"})),
            "&or=(a.gt.%22x%2Cy%22,and(a.eq.%22x%2Cy%22,b.gt.%22p%5C%22q%5C%5C%22))"
        );
    }

    #[test]
    fn loading_follows_the_cursor_across_pages() {
        let words = (0..2500)
            .map(|index| json!({"word": (format!("w{index:04} x"))}))
            .collect();
        let (store, requests) = mock(database(&[("words", words)]), store);
        assert_eq!(store.dictionary.words.len(), 2500);
        assert_eq!(requests[0].path, page("words", ""));
        assert_eq!(requests[1].path, page("words", "&word=gt.w0999%20x"));
        assert_eq!(requests[3].path, page("words", "&word=gt.w2499%20x"));
    }

    #[test]
    fn scan_fails_instead_of_looping_when_the_cursor_does_not_advance() {
        let row = json!([{"word": "a"}]);
        let replies = vec![
            Reply::json("GET", page("words", ""), 200, row.clone()),
            Reply::json("GET", page("words", "&word=gt.a"), 200, row),
        ];
        let (error, _) = mock(replies, || {
            let db = Database::new("http://localhost", "key");
            db.scan("words", &["word"], &|| false, &mut |_| {})
                .unwrap_err()
        });
        assert!(error.contains("con trỏ phân trang không tiến"), "{error}");
    }

    #[test]
    fn scan_stops_when_the_bot_is_shutting_down() {
        let replies = vec![Reply::json(
            "GET",
            page("words", ""),
            200,
            json!([{"word": "a"}]),
        )];
        let calls = Cell::new(0);
        let (error, _) = mock(replies, || {
            let db = Database::new("http://localhost", "key");
            let stop = || {
                calls.set(calls.get() + 1);
                calls.get() > 1
            };
            db.scan("words", &["word"], &stop, &mut |_| {}).unwrap_err()
        });
        assert!(error.contains("đang tắt"), "{error}");
    }

    #[test]
    fn increment_stat_is_a_plain_rpc_that_adopts_the_returned_total() {
        let path = "/rest/v1/rpc/increment_stat";
        let replies = vec![
            Reply::json("POST", path, 200, json!(7)),
            Reply::empty("POST", path),
            Reply::json(
                "POST",
                path,
                404,
                json!({"code": "PGRST202", "message": "Could not find the function"}),
            ),
        ];
        let (results, requests) = mock(replies, || {
            let db = Database::new("http://localhost", "key");
            (
                db.increment("word_played", 2),
                db.increment("word_played", 2),
                db.increment("word_played", 2),
            )
        });
        assert_eq!(results.0, Ok(7));
        assert!(results.1.is_err(), "một phản hồi rỗng không được coi là 0");
        let error = results.2.unwrap_err();
        assert!(
            error.contains("HTTP 404") && error.contains("schema.sql"),
            "{error}"
        );
        assert_eq!(requests[0].body["stat_key"].str(), "word_played");
        assert_eq!(requests[0].body["delta"].int(), 2);
        assert!(
            requests[0]
                .headers
                .contains(&"Content-Type: application/json".to_owned())
        );
        assert!(
            requests[0]
                .headers
                .iter()
                .all(|header| !header.starts_with("Prefer:"))
        );
    }

    #[test]
    fn failed_flushes_keep_the_delta_and_the_next_one_adopts_the_database_total() {
        let path = "/rest/v1/rpc/increment_stat";
        let mut replies = database(&base());
        replies.extend([
            Reply::json("POST", path, 503, json!({"message": "unavailable"})),
            // Another shard added 5 more in the meantime.
            Reply::json("POST", path, 200, json!(20)),
            Reply::json("POST", path, 503, json!({"message": "unavailable"})),
            Reply::json("POST", path, 200, json!(8)),
        ]);
        let (_, requests) = mock(replies, || {
            let mut store = store();
            store.add_query(5);
            assert_eq!(store.query, 15);
            assert!(store.flush_stats().is_err());
            assert_eq!((store.query, store.pending[0]), (15, 5));
            store.flush_stats().unwrap();
            assert_eq!((store.query, store.pending[0]), (20, 0));
            store.flush_stats().unwrap();

            assert!(store.add_words(2).is_err());
            assert_eq!((store.words, store.pending[1]), (5, 2));
            store.flush_stats().unwrap();
            assert_eq!((store.words, store.pending[1]), (8, 0));
        });
        let bodies = requests
            .iter()
            .filter(|request| request.path.ends_with("/rpc/increment_stat"))
            .map(|request| (request.body["stat_key"].str(), request.body["delta"].int()))
            .collect::<Vec<_>>();
        assert_eq!(
            bodies,
            [
                ("query", 5),
                ("query", 5),
                ("word_played", 2),
                ("word_played", 2)
            ]
        );
    }

    #[test]
    fn resync_mirrors_changes_made_outside_this_process() {
        let changed = vec![
            (
                "words",
                [dictionary(), vec![json!({"word": "nhật ký"})]].concat(),
            ),
            ("report_words", vec![json!({"word": "sinh nhật"})]),
            (
                "guild_config",
                vec![json!({"channel_id": "c2", "guild_id": "g2", "bot_mode": true})],
            ),
            (
                "counting_channels",
                vec![
                    json!({"channel_id": "k1", "guild_id": "g1", "current_count": "5", "last_player_id": null}),
                ],
            ),
            ("game_state", vec![]),
            (
                "rankings",
                vec![
                    json!({"guild_id": "g1", "user_id": "u1", "win": 2, "total": 2, "true_count": 1, "name": "Lan"}),
                    json!({"guild_id": "g2", "user_id": "u3", "win": 0, "total": 0, "true_count": 0}),
                    json!({"guild_id": "g2", "user_id": "u4", "win": 0, "total": 0, "true_count": 0}),
                ],
            ),
            (
                "global_stats",
                vec![
                    json!({"key": "query", "value": 50}),
                    json!({"key": "word_played", "value": 9}),
                    json!({"key": "round_played", "value": 4}),
                ],
            ),
            ("premium_guilds", vec![json!({"guild_id": "g2"})]),
        ];
        let mut replies = database(&base());
        replies.extend(database(&changed));
        let (report, _) = mock(replies, || {
            let store = Mutex::new(store());
            let report = resync(&store, &|| false).unwrap();
            let store = store.lock().unwrap();
            assert!(store.touched.is_none());
            assert!(store.dictionary.contains("nhật ký"));
            assert!(!store.dictionary.contains("sinh nhật"));
            assert_eq!(store.dictionary.count(), 3);
            assert!(!store.configs.contains_key("c1") && store.configs["c2"].bot);
            assert_eq!(store.counting["k1"].count, "5");
            assert!(store.counting["k1"].last.is_none());
            assert!(store.games.is_empty());
            assert_eq!(store.player("g1", "u1").unwrap().wins, 2);
            assert!(store.player("g1", "u2").is_none());
            assert!(store.player("g2", "u4").is_some());
            assert_eq!(store.players(), 3);
            assert_eq!(
                store.premium,
                HashSet::from(["g2".to_owned()]),
                "premium chỉ do người vận hành sửa ngoài bot"
            );
            assert_eq!((store.query, store.words, store.rounds), (50, 9, 4));
            report
        });
        assert_eq!(
            (
                report.words,
                report.blacklist,
                report.configs,
                report.counting
            ),
            (1, 1, 2, 1)
        );
        assert_eq!(
            (report.games, report.ranks, report.premium, report.stats),
            (1, 2, 2, 3)
        );
        assert_eq!(report.total(), 13);
        assert_eq!(
            report.to_string(),
            "13 thay đổi (từ điển 1, danh sách đen 1, kênh nối từ 2, kênh đếm số 1, ván nối từ 1, xếp hạng 2, premium 2, bộ đếm 3)"
        );
    }

    #[test]
    fn resync_without_changes_reports_nothing_and_keeps_the_dictionary_index() {
        let mut replies = database(&base());
        replies.extend(database(&base()));
        mock(replies, || {
            let store = Mutex::new(store());
            assert_eq!(resync(&store, &|| false).unwrap().total(), 0);
            let store = store.lock().unwrap();
            assert_eq!(store.dictionary.count(), 3);
            assert!(store.dictionary.opening().is_some());
        });
    }

    #[test]
    fn local_writes_during_a_resync_survive_a_stale_snapshot() {
        let base = base();
        let store = during_resync(&base, vec![write("POST", "words")], |store| {
            store.add_word("nhật ký", false).unwrap()
        });
        assert!(store.dictionary.contains("nhật ký"));

        let store = during_resync(&base, vec![write("POST", "report_words")], |store| {
            store.add_word("sinh nhật", true).unwrap()
        });
        assert!(!store.dictionary.contains("sinh nhật"));

        let blacklisted = with(
            base.clone(),
            "report_words",
            vec![json!({"word": "sinh nhật"})],
        );
        let store = during_resync(
            &blacklisted,
            vec![write("DELETE", "report_words?word=eq.sinh%20nh%E1%BA%ADt")],
            |store| assert!(store.unblacklist("sinh nhật").unwrap()),
        );
        assert!(store.dictionary.contains("sinh nhật"));

        let store = during_resync(&base, vec![write("POST", "guild_config")], |store| {
            store.set_word_channel("g1", "c9", true).unwrap()
        });
        assert!(store.configs["c9"].bot);

        let store = during_resync(&base, vec![write("POST", "counting_channels")], |store| {
            assert_eq!(store.set_counting_channel("g1", "k9").unwrap(), "0")
        });
        assert!(store.counting.contains_key("k9"));

        let store = during_resync(
            &base,
            vec![
                write("DELETE", "game_state?channel_id=eq.c1"),
                write("DELETE", "guild_config?channel_id=eq.c1"),
            ],
            |store| assert_eq!(store.unset("c1").unwrap(), Some("nối từ")),
        );
        assert!(!store.configs.contains_key("c1") && !store.games.contains_key("c1"));

        let store = during_resync(&base, vec![write("POST", "counting_channels")], |store| {
            assert_eq!(
                store.submit_count("k1", "2", "u2").unwrap(),
                Some(CountResult::Correct)
            )
        });
        assert_eq!(store.counting["k1"].count, "2");

        let store = during_resync(&base, vec![write("POST", "game_state")], |store| {
            let game = Game {
                running: true,
                words: vec!["học sinh".into(), "sinh nhật".into()],
                ..Game::default()
            };
            store.save_game("c1", game).unwrap();
        });
        assert_eq!(store.games["c1"].words.len(), 2);

        let store = during_resync(&base, vec![write("POST", "rankings")], |store| {
            let player = Player {
                wins: 5,
                ..store.player("g1", "u1").unwrap().clone()
            };
            store.save_player("g1", "u1", player).unwrap();
        });
        assert_eq!(store.player("g1", "u1").unwrap().wins, 5);

        let store = during_resync(
            &base,
            vec![
                write("DELETE", "game_state?channel_id=in.(c1,k1,live)"),
                write("DELETE", "counting_channels?guild_id=eq.g1"),
                write("DELETE", "rankings?guild_id=eq.g1"),
                write("DELETE", "premium_guilds?guild_id=eq.g1"),
                write("DELETE", "guild_config?guild_id=eq.g1"),
            ],
            |store| store.remove_guild("g1", ["live".to_owned()]).unwrap(),
        );
        assert!(store.configs.is_empty() && store.counting.is_empty() && store.games.is_empty());
        assert!(store.player("g1", "u1").is_none() && store.player("g2", "u3").is_some());
        assert!(store.premium.is_empty());
    }

    #[test]
    fn a_counter_flushed_during_a_resync_keeps_the_newer_database_total() {
        let store = during_resync(
            &base(),
            vec![Reply::json(
                "POST",
                "/rest/v1/rpc/increment_stat",
                200,
                json!(50),
            )],
            |store| {
                store.add_query(3);
                store.flush_stats().unwrap();
            },
        );
        assert_eq!((store.query, store.pending[0]), (50, 0));
    }

    #[test]
    fn resync_adds_unconfirmed_counter_deltas_on_top_of_the_database_values() {
        let changed = with(
            with(
                base(),
                "global_stats",
                vec![
                    json!({"key": "query", "value": 50}),
                    json!({"key": "word_played", "value": 7}),
                    json!({"key": "round_played", "value": 2}),
                ],
            ),
            "premium_guilds",
            vec![json!({"guild_id": "g1"})],
        );
        let mut replies = database(&base());
        replies.extend(database(&changed));
        mock(replies, || {
            let store = Mutex::new(store());
            store.lock().unwrap().add_query(3);
            resync(&store, &|| false).unwrap();
            let store = store.lock().unwrap();
            assert_eq!((store.query, store.words, store.rounds), (53, 7, 2));
            assert_eq!(store.pending, [3, 0, 0]);
        });
    }

    #[test]
    fn failed_or_unsafe_resyncs_change_nothing() {
        // A read that fails after some tables were already fetched.
        let mut replies = database(&base());
        let mut partial = database(&base());
        let broken = partial
            .iter()
            .position(|reply| reply.path == page("game_state", ""))
            .unwrap();
        partial.truncate(broken);
        partial.push(Reply::json(
            "GET",
            page("game_state", ""),
            500,
            json!({"message": "boom"}),
        ));
        replies.extend(partial);
        // Supabase answering with empty tables (for example a key without access).
        replies.extend(database(&[]));
        // A shutdown arriving mid-fetch.
        replies.extend(database(&base()).into_iter().take(2));
        mock(replies, || {
            let store = Mutex::new(store());
            let unchanged = |store: &Mutex<Store>| {
                let store = store.lock().unwrap();
                assert!(store.touched.is_none());
                assert_eq!(store.dictionary.count(), 3);
                assert!(store.configs.contains_key("c1") && store.games.contains_key("c1"));
                assert_eq!(store.counting["k1"].count, "1");
                assert!(store.player("g1", "u1").is_some() && store.premium.contains("g1"));
                assert_eq!(store.query, 10);
            };
            let error = resync(&store, &|| false).unwrap_err();
            assert!(error.contains("HTTP 500"), "{error}");
            unchanged(&store);

            let error = resync(&store, &|| false).unwrap_err();
            assert!(error.contains("từ điển rỗng"), "{error}");
            unchanged(&store);

            let calls = Cell::new(0);
            let stop = || {
                calls.set(calls.get() + 1);
                calls.get() > 2
            };
            assert!(resync(&store, &stop).unwrap_err().contains("đang tắt"));
            unchanged(&store);
        });
    }
}
