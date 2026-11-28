use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::App;
use crate::discord::{MANAGE_GUILD, SILENT, avatar, display_name, has};
use crate::json::Json;
use crate::store::{CountResult, Game, Store, normalize_word, random_index};

pub fn message(app: &Arc<App>, data: Json) {
    let guild = data["guild_id"].str();
    let Some(_activity) = app.activity.begin(guild) else {
        return;
    };
    if let Err(error) = handle(app, &data) {
        eprintln!("Tin nhắn trò chơi: {error}");
    }
}

enum Response {
    Silent(String),
    Reply(String),
    AutoDelete(String),
    Reaction(bool),
    BotWord(String),
}

fn handle(app: &App, message: &Json) -> Result<(), String> {
    let guild = message["guild_id"].str();
    let channel = message["channel_id"].str();
    let content = message["content"].str();
    let user = &message["author"];
    let user_id = user["id"].str();

    let responses = {
        let command = if content.starts_with("?phobo") {
            content.split_whitespace().nth(1).unwrap_or_default()
        } else {
            ""
        };
        if matches!(command, "set" | "unset") {
            vec![Response::Reply(prefix(app, message, command)?)]
        } else {
            let (bot_id, bot_name) = {
                let cache = app.discord.cache.read().unwrap();
                (
                    cache.user["id"].str().to_owned(),
                    cache.user["username"].str().to_owned(),
                )
            };
            let mut store = app.store.lock().unwrap();
            if store.counting.contains_key(channel) {
                match store.submit_count(channel, content, user_id)? {
                    Some(CountResult::Correct) => vec![Response::Reaction(true)],
                    Some(result) => {
                        let reason = if result == CountResult::SamePlayer {
                            "Bạn không thể đếm hai lần liên tiếp!"
                        } else {
                            "Bạn đã đếm sai!"
                        };
                        vec![
                            Response::Reaction(false),
                            Response::Reply(format!(
                                "{reason} Số đếm đã được reset về **0**. Hãy bắt đầu lại từ **1**."
                            )),
                        ]
                    }
                    None => Vec::new(),
                }
            } else if let Some(config) = store
                .configs
                .get(channel)
                .filter(|config| config.guild == guild)
            {
                let bot_mode = config.bot;
                play(&mut store, message, bot_mode, &bot_id, &bot_name)?
            } else {
                store.add_query(1);
                Vec::new()
            }
        }
    };

    let id = message["id"].str();
    for response in responses {
        let result = match response {
            Response::Reaction(correct) => {
                let emoji = if correct { &app.config.correct_emoji } else { &app.config.wrong_emoji };
                app.discord.react(channel, id, emoji)
            }
            Response::Silent(text) => app.discord.send(channel, json!({"content": text, "flags": SILENT})).map(|_| ()),
            Response::Reply(text) => app.discord.send(channel, json!({
                "content": text, "flags": SILENT, "message_reference": {"message_id": id, "fail_if_not_exists": false}
            })).map(|_| ()),
            Response::AutoDelete(text) => app.discord.send(channel, json!({"content": text, "flags": SILENT})).map(|sent| {
                let path = format!("/channels/{channel}/messages/{}", sent["id"].str());
                schedule_delete(path);
            }),
            Response::BotWord(word) => app.discord.send(channel, json!({"content": (format!("**{word}**")), "flags": SILENT}))
                .and_then(|sent| app.discord.react(channel, sent["id"].str(), &app.config.correct_emoji)),
        };
        if let Err(error) = result {
            eprintln!("Phản hồi trò chơi: {error}");
        }
    }
    Ok(())
}

fn prefix(app: &App, message: &Json, command: &str) -> Result<String, String> {
    let guild = message["guild_id"].str();
    let channel = message["channel_id"].str();
    let user = message["author"]["id"].str();
    let (permissions, name, guild_name) = {
        let cache = app.discord.cache.read().unwrap();
        let state = cache.guilds.get(guild).ok_or("Máy chủ chưa sẵn sàng")?;
        let channel_state = state.channels.get(channel).unwrap_or(&Json::Null);
        let permissions = state.permissions(guild, user, &message["member"], channel_state);
        let name = channel_state["name"]
            .opt_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("<#{channel}>"));
        (permissions, name, state.data["name"].str().to_owned())
    };
    if !has(permissions, MANAGE_GUILD) {
        return Ok("Bạn cần có quyền `MANAGE_GUILD` để dùng lệnh này".into());
    }
    let mut store = app.store.lock().unwrap();
    if command == "unset" {
        let result = store.unset(channel)?;
        return Ok(match result {
            Some(mode) => {
                format!("Đã xoá cài đặt kênh {mode} **{name}** của máy chủ **{guild_name}**!")
            }
            None => format!("Kênh **{name}** chưa được cài đặt trò chơi!"),
        });
    }
    if store.counting.contains_key(channel) {
        return Ok(
            "Kênh này đang chơi đếm số. Hãy dùng `/unset-channel` trước khi bật nối từ.".into(),
        );
    }
    store.set_word_channel(guild, channel, false)?;
    Ok(format!(
        "Bạn đã chọn kênh **{name}** làm kênh nối từ của máy chủ **{guild_name}**. Dùng `!start` để bắt đầu trò chơi"
    ))
}

fn play(
    store: &mut Store,
    message: &Json,
    bot_mode: bool,
    bot_id: &str,
    bot_name: &str,
) -> Result<Vec<Response>, String> {
    let channel = message["channel_id"].str();
    let guild = message["guild_id"].str();
    let user = &message["author"];
    let user_id = user["id"].str();
    let name = display_name(user, &Json::Null);
    let content = message["content"].str();
    let mut game = match store.games.get(channel) {
        Some(game) => game.clone(),
        None => {
            let game = Game::default();
            store.save_game(channel, game.clone())?;
            game
        }
    };
    let mut responses = Vec::new();

    if content == "!start" || content == "!stop" {
        if content == "!start" && game.running {
            return Ok(vec![Response::Silent(
                "Trò chơi vẫn đang tiếp tục. Bạn có thể dùng `!stop`".into(),
            )]);
        }
        if content == "!stop" && !game.running {
            return Ok(vec![Response::Silent(
                "Trò chơi chưa bắt đầu. Bạn có thể dùng `!start`".into(),
            )]);
        }
        let stopping = content == "!stop";
        let opening = opening_game(
            store,
            &mut responses,
            if stopping {
                "Đã kết thúc lượt này! Lượt mới đã bắt đầu!"
            } else {
                "Trò chơi đã bắt đầu!"
            },
        );
        store.save_game(channel, opening)?;
        if stopping && let Err(error) = store.add_round() {
            eprintln!("Lưu thống kê ván: {error}");
        }
        return Ok(responses);
    }
    if !game.running {
        return Ok(responses);
    }

    let existing = store.player(guild, user_id).cloned();
    let mut player = existing.clone().unwrap_or_default();
    player.name = Some(name.into());
    player.avatar = user["avatar"].opt_str().map(|_| avatar(user));

    let word = normalize_word(content);
    let mut parts = word.split_whitespace();
    let first = parts.next().unwrap_or_default();
    let exact_two = parts.count() == 1;
    let mut wrong = None;
    if exact_two {
        if game.player.as_deref() == Some(user_id) {
            wrong = Some("Bạn đã trả lời lượt trước rồi, hãy đợi đối thủ!".into());
        } else if let Some(last) = game
            .words
            .last()
            .and_then(|word| word.split_whitespace().next_back())
            && first != last
        {
            wrong = Some(format!("Từ này không bắt đầu với tiếng \x60{last}\x60"));
        }
        if wrong.is_none() {
            let index = game.words.iter().position(|used| used == &word);
            store.add_query(index.map_or(game.words.len(), |index| index + 1) as u64);
            if index.is_some() {
                wrong = Some("Từ này đã được sử dụng!".into());
            }
        }
    }
    if !exact_two || wrong.is_some() {
        if changed(existing.as_ref(), &player) {
            store.save_player(guild, user_id, player)?;
        }
        if let Some(hint) = wrong {
            responses.push(Response::Reaction(false));
            responses.push(Response::AutoDelete(hint));
        }
        return Ok(responses);
    }
    player.total += 1;
    if !store.dictionary.contains(&word) {
        store.save_player(guild, user_id, player)?;
        return Ok(vec![Response::Reaction(false)]);
    }

    player.correct += 1;
    game.words.push(word.clone());
    game.player = Some(user_id.into());
    game.name = Some(name.into());

    let mut counted_words = 1;
    let mut round_finished = false;
    let next = answers(store, &game, &word);
    responses.push(Response::Reaction(true));
    if next.is_empty() {
        player.wins += 1;
        round_finished = true;
        let turns = game.words.len() - 1;
        let prefix = if bot_mode { "Bot đã bí từ! " } else { "" };
        responses.push(Response::Silent(format!(
            "{prefix}{name} đã chiến thắng sau {turns} lượt!"
        )));
    } else if bot_mode {
        let bot_word = next[random_index(next.len())].clone();
        game.words.push(bot_word.clone());
        game.player = Some(bot_id.into());
        game.name = Some(bot_name.into());
        counted_words += 1;
        responses.push(Response::BotWord(bot_word.clone()));
        if answers(store, &game, &bot_word).is_empty() {
            round_finished = true;
            let turns = game.words.len() - 1;
            responses.push(Response::Silent(format!(
                "Bot đã chiến thắng sau {turns} lượt!"
            )));
        }
    }
    if round_finished {
        game = opening_game(store, &mut responses, "Lượt mới đã bắt đầu!");
    }
    store.save_game(channel, game)?;
    // Each write updates its cache only after Supabase confirms it. Independent
    // statistics writes are still attempted if a different statistics table fails.
    for result in [
        store.save_player(guild, user_id, player),
        store.add_words(counted_words),
    ] {
        if let Err(error) = result {
            eprintln!("Lưu thống kê nối từ: {error}");
        }
    }
    if round_finished && let Err(error) = store.add_round() {
        eprintln!("Lưu thống kê ván: {error}");
    }
    Ok(responses)
}

fn changed(previous: Option<&crate::store::Player>, current: &crate::store::Player) -> bool {
    previous
        .is_none_or(|previous| previous.name != current.name || previous.avatar != current.avatar)
}

fn opening_game(store: &mut Store, responses: &mut Vec<Response>, header: &str) -> Game {
    let Some((word, query)) = store.dictionary.opening() else {
        responses.push(Response::Silent(
            "Từ điển hiện không có từ phù hợp để bắt đầu trò chơi.".into(),
        ));
        return Game::default();
    };
    store.add_query(query);
    responses.push(Response::Silent(header.into()));
    responses.push(Response::Silent(format!("Từ bắt đầu: **{word}**")));
    Game {
        running: true,
        words: vec![word],
        ..Game::default()
    }
}

fn answers(store: &mut Store, game: &Game, word: &str) -> Vec<String> {
    let mut query = store.dictionary.count() as u64;
    let next = store
        .dictionary
        .next(word)
        .filter_map(|answer| {
            let used = game.words.iter().position(|word| word == answer);
            query += used.map_or(game.words.len(), |index| index + 1) as u64;
            used.is_none().then(|| answer.to_owned())
        })
        .collect();
    store.add_query(query);
    next
}

use std::sync::OnceLock;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Instant;

static DELETIONS: OnceLock<SyncSender<(Instant, String)>> = OnceLock::new();

pub fn start_deletions(app: Arc<App>) {
    let (send, receive) = sync_channel::<(Instant, String)>(256);
    if DELETIONS.set(send).is_err() {
        return;
    }
    thread::spawn(move || {
        let mut scheduled: std::collections::BinaryHeap<std::cmp::Reverse<(Instant, String)>> =
            std::collections::BinaryHeap::new();
        while !crate::SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
            let wait = match scheduled.peek() {
                Some(std::cmp::Reverse((at, _))) => (*at)
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(1)),
                None => Duration::from_secs(1),
            };
            if let Ok(job) = receive.recv_timeout(wait) {
                scheduled.push(std::cmp::Reverse(job));
            }
            while scheduled
                .peek()
                .is_some_and(|std::cmp::Reverse((at, _))| *at <= Instant::now())
            {
                let std::cmp::Reverse((_, path)) = scheduled.pop().unwrap();
                if let Err(error) = app.discord.rest("DELETE", &path, None) {
                    eprintln!("Xoá tin nhắn trò chơi: {error}");
                }
            }
        }
    });
}

fn schedule_delete(path: String) {
    if let Some(queue) = DELETIONS.get() {
        let _ = queue.try_send((Instant::now() + Duration::from_secs(3), path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Reply, database, dictionary, mock, store};

    #[test]
    fn loads_a_round_then_continues_against_the_bot_and_saves_stats() {
        let mut replies = database(&[
            ("words", dictionary()),
            (
                "game_state",
                vec![json!({
                    "channel_id": "word", "running": true, "words": ["học sinh"], "current_player_id": null
                })],
            ),
        ]);
        replies.extend([
            Reply::empty("POST", "/rest/v1/game_state"),
            Reply::empty("POST", "/rest/v1/rankings"),
            Reply::json("POST", "/rest/v1/rpc/increment_stat", 200, json!(2)),
            Reply::json("POST", "/rest/v1/rpc/increment_stat", 200, json!(1)),
        ]);
        let (_, requests) = mock(replies, || {
            let mut store = store();
            let message = json!({"channel_id": "word", "guild_id": "guild", "content": " SINH  NHẬT ",
                "author": {"id": "1", "username": "Lan"}});
            let responses = play(&mut store, &message, true, "bot", "Neko").unwrap();
            assert!(matches!(responses.first(), Some(Response::Reaction(true))));
            assert!(
                matches!(responses.get(1), Some(Response::BotWord(word)) if word == "nhật học")
            );
            assert!(
                matches!(responses.get(2), Some(Response::Silent(text)) if text == "Bot đã chiến thắng sau 2 lượt!")
            );
            let next = &store.games["word"];
            assert!(next.running);
            assert_eq!(next.words.len(), 1);
            assert!(next.player.is_none());
            assert_eq!(store.words, 2);
            assert_eq!(store.rounds, 1);
            assert_eq!(store.player("guild", "1").unwrap().correct, 1);
        });
        let writes = &requests[requests.len() - 4..];
        assert_eq!(writes[0].body["words"].array().len(), 1);
        assert_eq!(writes[1].body["true_count"].int(), 1);
        assert_eq!(writes[2].body["stat_key"].str(), "word_played");
        assert_eq!(writes[2].body["delta"].int(), 2);
        assert_eq!(writes[3].body["stat_key"].str(), "round_played");
        assert_eq!(writes[3].body["delta"].int(), 1);
        // An RPC must not ask for `return=minimal`: that would drop the returned total.
        assert!(
            writes[2]
                .headers
                .iter()
                .all(|header| !header.starts_with("Prefer:"))
        );
    }
}
