use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use crate::http::{Request, encode_component};
use crate::json::Json;

const PAGE: usize = 1000;
const DICTIONARY_URL: &str =
    "https://raw.githubusercontent.com/undertheseanlp/dictionary/master/dictionary/words.txt";
const CONTRIBUTIONS_URL: &str =
    "https://raw.githubusercontent.com/lvdat/phobo-contribute-words/main/accepted-words.txt";

struct Database {
    base: String,
    headers: Vec<String>,
}

impl Database {
    fn new(base: &str, key: &str) -> Self {
        Self {
            base: format!("{base}/rest/v1"),
            headers: vec![
                format!("apikey: {key}"),
                format!("Authorization: Bearer {key}"),
            ],
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
            headers.push("Prefer: resolution=merge-duplicates,return=minimal".into());
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
            let message = response.json().unwrap_or_default();
            return Err(format!(
                "Supabase {method} {table}: HTTP {} {}",
                response.code,
                message["message"].str()
            ));
        }
        response.json()
    }

    fn all(&self, table: &str, order: &str) -> Result<Vec<Json>, String> {
        let mut rows = Vec::new();
        loop {
            let page = self.request(
                "GET",
                table,
                &format!("?select=*&order={order}&limit={PAGE}&offset={}", rows.len()),
                None,
            )?;
            let Json::Array(page) = page else {
                return Err(format!("Supabase {table}: phản hồi không phải danh sách"));
            };
            if page.is_empty() {
                return Ok(rows);
            }
            rows.extend(page);
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

#[derive(Clone)]
pub struct Mode {
    pub guild: String,
    pub bot: bool,
}

#[derive(Clone, Default)]
pub struct Counting {
    pub guild: String,
    pub count: String,
    pub last: Option<String>,
}

#[derive(Clone, Default)]
pub struct Game {
    pub running: bool,
    pub player: Option<String>,
    pub name: Option<String>,
    pub words: Vec<String>,
}

#[derive(Clone, Default)]
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
    dirty_query: bool,
}

fn id(row: &Json, key: &str) -> String {
    row[key].str().to_owned()
}
fn optional(row: &Json, key: &str) -> Option<String> {
    row[key].opt_str().map(str::to_owned)
}

impl Store {
    pub fn load(url: &str, key: &str) -> Result<Self, String> {
        let db = Database::new(url, key);
        let mut words: BTreeSet<String> = db
            .all("words", "word")?
            .iter()
            .map(|row| id(row, "word"))
            .collect();
        if words.is_empty() {
            words = seed(&db)?;
        }
        let blacklist = db
            .all("report_words", "word")?
            .iter()
            .map(|row| id(row, "word"))
            .collect();
        let mut dictionary = Dictionary {
            words,
            blacklist,
            ..Default::default()
        };
        dictionary.index();

        let configs = db
            .all("guild_config", "channel_id")?
            .iter()
            .map(|row| {
                (
                    id(row, "channel_id"),
                    Mode {
                        guild: id(row, "guild_id"),
                        bot: row["bot_mode"].bool(),
                    },
                )
            })
            .collect();
        let counting = db
            .all("counting_channels", "channel_id")?
            .iter()
            .map(|row| {
                (
                    id(row, "channel_id"),
                    Counting {
                        guild: id(row, "guild_id"),
                        count: id(row, "current_count"),
                        last: optional(row, "last_player_id"),
                    },
                )
            })
            .collect();
        let games = db
            .all("game_state", "channel_id")?
            .iter()
            .map(|row| {
                (
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
                )
            })
            .collect();
        let mut ranks: HashMap<String, HashMap<String, Player>> = HashMap::new();
        for row in db.all("rankings", "guild_id,user_id")? {
            ranks.entry(id(&row, "guild_id")).or_default().insert(
                id(&row, "user_id"),
                Player {
                    name: optional(&row, "name"),
                    avatar: optional(&row, "avatar"),
                    wins: row["win"].int(),
                    total: row["total"].int(),
                    correct: row["true_count"].int(),
                },
            );
        }
        let stats: HashMap<_, _> = db
            .all("global_stats", "key")?
            .iter()
            .map(|row| {
                (
                    id(row, "key"),
                    u64::try_from(row["value"].int()).unwrap_or_default(),
                )
            })
            .collect();
        let premium = db
            .all("premium_guilds", "guild_id")?
            .iter()
            .map(|row| id(row, "guild_id"))
            .collect();
        Ok(Self {
            db,
            dictionary,
            configs,
            counting,
            games,
            ranks,
            premium,
            query: *stats.get("query").unwrap_or(&0),
            dirty_query: false,
            words: *stats.get("word_played").unwrap_or(&0),
            rounds: *stats.get("round_played").unwrap_or(&0),
        })
    }

    pub fn add_word(&mut self, word: &str, blacklist: bool) -> Result<(), String> {
        let table = if blacklist { "report_words" } else { "words" };
        self.db.upsert(table, json!({"word": word}))?;
        if blacklist {
            self.dictionary.blacklist.insert(word.into());
        } else {
            self.dictionary.words.insert(word.into());
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
        }
        Ok(())
    }

    fn remove_word_channel(&mut self, channel: &str) -> Result<(), String> {
        if self.games.contains_key(channel) {
            self.db.delete("game_state", "channel_id", channel)?;
            self.games.remove(channel);
        }
        if self.configs.contains_key(channel) {
            self.db.delete("guild_config", "channel_id", channel)?;
            self.configs.remove(channel);
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
        Ok(())
    }

    pub fn player(&self, guild: &str, user: &str) -> Option<&Player> {
        self.ranks.get(guild)?.get(user)
    }

    pub fn players(&self) -> usize {
        self.ranks.values().map(HashMap::len).sum()
    }

    pub fn add_query(&mut self, amount: u64) {
        self.query += amount;
        self.dirty_query |= amount > 0;
    }

    fn save_stat(&self, key: &str, value: u64) -> Result<(), String> {
        self.db
            .upsert("global_stats", json!({"key": key, "value": value}))
    }

    pub fn flush_query(&mut self) -> Result<(), String> {
        if self.dirty_query {
            self.save_stat("query", self.query)?;
            self.dirty_query = false;
        }
        Ok(())
    }

    pub fn add_words(&mut self, count: u64) -> Result<(), String> {
        self.save_stat("word_played", self.words + count)?;
        self.words += count;
        Ok(())
    }

    pub fn add_round(&mut self) -> Result<(), String> {
        self.save_stat("round_played", self.rounds + 1)?;
        self.rounds += 1;
        Ok(())
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
        self.db.delete("counting_channels", "guild_id", guild)?;
        self.counting.retain(|_, value| value.guild != guild);
        self.db.delete("rankings", "guild_id", guild)?;
        self.ranks.remove(guild);
        self.db.delete("premium_guilds", "guild_id", guild)?;
        self.premium.remove(guild);
        self.db.delete("guild_config", "guild_id", guild)?;
        self.configs.retain(|_, value| value.guild != guild);
        Ok(())
    }
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
