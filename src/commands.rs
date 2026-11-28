use std::sync::Arc;

use crate::discord::{
    ADD_REACTIONS, COLOR, EPHEMERAL, Interaction, MANAGE_GUILD, READ_HISTORY, SEND_MESSAGES,
    SILENT, VIEW_CHANNEL, avatar, clip, display_name, embed, field, has, now_ms, snowflake_ms,
};
use crate::json::Json;
use crate::store::{increment, normalize_word};
use crate::{App, ai};

pub fn definitions() -> Json {
    let choice = |name: &str, value: &str| json!({"name": name, "value": value});
    let mut channel = option(7, "channel", "Kênh chơi nối từ hoặc đếm số", true);
    channel["channel_types"] = json!([0]);
    let mut unset_channel = channel.clone();
    unset_channel["description"] = "Kênh cần xoá cài đặt trò chơi".into();
    let mut modes = option(
        3,
        "bot_mode",
        "Chọn chế độ chơi (mặc định: nối từ không có bot)",
        false,
    );
    modes["choices"] = json!([
        (choice("Nối từ (False)", "false")),
        (choice("Nối từ với bot (True)", "true")),
        (choice("Đếm số", "counting"))
    ]);
    let mut action = option(3, "action", "Hành động cần thực hiện", false);
    action["choices"] = json!([
        (choice("Kiểm tra", "check")),
        (choice("Gỡ khỏi blacklist", "remove"))
    ]);
    let mut report_type = option(3, "type", "Chọn báo cáo hay đề xuất thêm từ", false);
    report_type["choices"] = json!([(choice("Báo cáo", "report")), (choice("Thêm từ", "add"))]);
    let mut report_word = option(3, "word", "Từ muốn báo cáo", true);
    report_word["max_length"] = 500.into();
    let mut broadcast = option(3, "message", "Nội dung cần gửi (tối đa 2000 ký tự)", true);
    broadcast["max_length"] = 2000.into();
    let mut selection = option(
        3,
        "guild",
        "Chọn máy chủ hoặc nhập tên/ID (chỉ dùng trong REPORT_CHANNEL; mặc định: máy chủ này)",
        false,
    );
    selection["autocomplete"] = true.into();
    selection["min_length"] = 1.into();
    selection["max_length"] = 100.into();
    let mut leave = command(
        "leave",
        "Xoá dữ liệu và cho bot rời máy chủ hiện tại hoặc máy chủ được chọn",
        vec![selection],
    );
    leave["contexts"] = json!([0]);
    leave["default_member_permissions"] = MANAGE_GUILD.to_string().into();

    Json::Array(vec![
        command("help", "Xem hướng dẫn sử dụng OrenjiNeko", vec![]),
        command("rank", "Xem bảng xếp hạng nối từ", vec![]),
        command("me", "Xem thống kê nối từ của bạn", vec![]),
        command("stats", "Xem các thống kê của BOT", vec![]),
        command("server", "Xem thông tin máy chủ", vec![]),
        command("ping", "Kiểm tra ping của bot", vec![]),
        command(
            "set-channel",
            "Cài đặt kênh chơi nối từ hoặc đếm số",
            vec![channel, modes],
        ),
        command(
            "unset-channel",
            "Xoá cài đặt kênh chơi nối từ hoặc đếm số",
            vec![unset_channel],
        ),
        command(
            "unblacklist",
            "Kiểm tra và gỡ từ khỏi blacklist của bot",
            vec![
                option(3, "word", "Cụm từ cần kiểm tra hoặc gỡ", true),
                action,
            ],
        ),
        command(
            "report",
            "Báo cáo từ ngữ không phù hợp trong từ điển",
            vec![
                report_word,
                option(3, "reason", "Lý do (không bắt buộc)", false),
                report_type,
            ],
        ),
        command(
            "broadcast",
            "Gửi tin nhắn đến tất cả kênh nối từ và đếm số đã cấu hình",
            vec![broadcast],
        ),
        leave,
        command(
            "ai",
            "Chat với AI, đọc văn bản và tối đa 4 ảnh trong 10 tin nhắn gần nhất",
            vec![
                option(3, "message", "Tin nhắn gửi cho AI", true),
                option(
                    3,
                    "system",
                    "System prompt tùy chỉnh (mặc định: trợ lý tiếng Việt)",
                    false,
                ),
                option(
                    5,
                    "visible",
                    "Để mọi người trong kênh cùng thấy câu trả lời (mặc định: mọi người đều thấy; tắt để chỉ bạn thấy)",
                    false,
                ),
            ],
        ),
    ])
}

fn command(name: &str, description: &str, options: Vec<Json>) -> Json {
    json!({"name": name, "description": description, "type": 1, "options": options})
}

fn option(kind: u32, name: &str, description: &str, required: bool) -> Json {
    json!({"type": kind, "name": name, "description": description, "required": required})
}

pub fn interaction(app: &Arc<App>, data: Json) {
    let mut event = Interaction::new(&app.discord, data);
    let kind = event.data["type"].int();
    let command = event.data["data"]["name"].str().to_owned();
    let result = if kind == 4 {
        autocomplete(app, &mut event)
    } else if kind == 3 {
        moderate(app, &mut event)
    } else if kind == 2 && command == "leave" {
        leave(app, &mut event)
    } else if kind == 2 {
        let guild = event.guild().to_owned();
        let Some(_activity) = app.activity.begin(&guild) else {
            let _ = event.text(
                "Bot đang xoá dữ liệu và rời máy chủ này. Vui lòng thử lại sau.",
                true,
            );
            return;
        };
        slash(app, &mut event, &command)
    } else {
        return;
    };
    if let Err(error) = result {
        eprintln!("Tương tác Discord ({command}): {error}");
        if kind != 4 {
            event.error();
        }
    }
}

fn slash(app: &App, event: &mut Interaction, command: &str) -> Result<(), String> {
    if event.guild().is_empty()
        && matches!(
            command,
            "me" | "rank" | "server" | "set-channel" | "unset-channel" | "report"
        )
    {
        return event.text("Lệnh này chỉ dùng được trong máy chủ.", true);
    }
    match command {
        "help" => event.reply(json!({"embeds": [(help())], "flags": SILENT})),
        "stats" => event.reply(json!({"embeds": [(stats(app))], "flags": SILENT})),
        "me" => event.reply(json!({"embeds": [(profile(app, event))]})),
        "rank" => event.reply(json!({"embeds": [(ranking(app, event.guild()))]})),
        "server" => {
            let server = server(app, event)?;
            event.reply(json!({"embeds": [server]}))
        }
        "ping" => ping(app, event),
        "set-channel" => configure(app, event, true),
        "unset-channel" => configure(app, event, false),
        "broadcast" => broadcast(app, event),
        "report" => report(app, event),
        "unblacklist" => unblacklist(app, event),
        "ai" => ai::execute(app, event),
        _ => event.text("Không tìm thấy lệnh này.", true),
    }
}

fn configure(app: &App, event: &mut Interaction, set: bool) -> Result<(), String> {
    if !event.can_manage() {
        return event.text(
            "Bạn cần có quyền \x60MANAGE_GUILD\x60 để thực hiện thao tác này!",
            true,
        );
    }
    let guild = event.guild().to_owned();
    let channel = event.option("channel").str().to_owned();
    if channel.is_empty() {
        return event.text("Vui lòng chọn một kênh văn bản.", true);
    }
    let mode = event.option("bot_mode").str().to_owned();
    let guild_name = app.discord.guild_name(&guild);
    if !set {
        let exists = {
            let store = app.store.lock().unwrap();
            store.configs.contains_key(&channel) || store.counting.contains_key(&channel)
        };
        if !exists {
            return event.text(
                format!("Kênh <#{channel}> chưa được cài đặt trò chơi!"),
                true,
            );
        }
        event.defer(false)?;
        let result = app.store.lock().unwrap().unset(&channel)?;
        return event.edit_text(match result {
            Some(mode) => {
                format!("Đã xoá cài đặt kênh {mode} <#{channel}> của máy chủ **{guild_name}**!")
            }
            None => format!("Kênh <#{channel}> chưa được cài đặt trò chơi!"),
        });
    }

    event.defer(false)?;
    let channel_data = match app.discord.channel(&guild, &channel) {
        Some(data) => data,
        None => app
            .discord
            .rest("GET", &format!("/channels/{channel}"), None)?,
    };
    if channel_data["type"].int() != 0
        || channel_data["guild_id"]
            .opt_str()
            .is_some_and(|owner| owner != guild)
    {
        return event.edit_text("Vui lòng chọn một kênh văn bản trong máy chủ này.");
    }
    let counting = mode == "counting";
    let required =
        VIEW_CHANNEL | SEND_MESSAGES | ADD_REACTIONS | if counting { READ_HISTORY } else { 0 };
    if !has(
        app.discord.bot_permissions(&guild, &channel_data)?,
        required,
    ) {
        return event.edit_text(if counting {
            "Bot cần quyền xem kênh, gửi tin nhắn, đọc lịch sử và thả cảm xúc trong kênh đếm số."
        } else {
            "Bot cần quyền xem kênh, gửi tin nhắn và thả cảm xúc trong kênh nối từ."
        });
    }

    let response = {
        let mut store = app.store.lock().unwrap();
        if counting {
            let count = store.set_counting_channel(&guild, &channel)?;
            let next = increment(&count);
            format!(
                "Đã chọn kênh <#{channel}> làm kênh đếm số! Số hiện tại: **{count}**. Hãy gửi **{next}** để đếm tiếp. Đếm sai hoặc một người đếm hai lần liên tiếp sẽ reset về 0. Trò chơi không có xếp hạng."
            )
        } else {
            let bot = mode == "true";
            store.set_word_channel(&guild, &channel, bot)?;
            let suffix = if bot {
                " Chế độ nối từ với bot đã được **bật**."
            } else {
                ""
            };
            format!(
                "Bạn đã chọn kênh <#{channel}> làm kênh chơi nối từ của máy chủ **{guild_name}**!{suffix}"
            )
        }
    };
    event.edit_text(response)
}

fn stats(app: &App) -> Json {
    let guilds = app.total_guilds();
    let store = app.store.lock().unwrap();
    embed(vec![
        field(
            "Tổng số server đang sử dụng",
            format!("{guilds} servers"),
            true,
        ),
        field("Tổng số người đã chơi", store.players().to_string(), true),
        field("Tổng số từ đã nối", store.words.to_string(), true),
        field("Tổng số vòng đã diễn ra", store.rounds.to_string(), true),
        field("Tổng số truy vấn dữ liệu", store.query.to_string(), true),
        field(
            "Tổng số từ trong ngân hàng từ",
            store.dictionary.count().to_string(),
            true,
        ),
    ])
}

fn profile(app: &App, event: &Interaction) -> Json {
    let user = event.user();
    let store = app.store.lock().unwrap();
    let fields = match store.player(event.guild(), user["id"].str()) {
        Some(player) => vec![
            field("Thắng", format!("\x60{}\x60", player.wins), true),
            field(
                "Đã trả lời đúng",
                format!(
                    "\x60{}/{} từ ({:.2}%)\x60",
                    player.correct,
                    player.total,
                    player.accuracy() * 100.0
                ),
                true,
            ),
        ],
        None => vec![field(
            "Hồ sơ trống",
            "Bạn chưa chơi nối từ ở server này!",
            false,
        )],
    };
    let mut result = embed(fields);
    result["color"] = 0x0099ff.into();
    result["title"] = display_name(user, &event.data["member"]).into();
    result["description"] = "Hồ sơ nối từ".into();
    result["thumbnail"] = json!({"url": (avatar(user))});
    result
}

fn ranking(app: &App, guild_id: &str) -> Json {
    let author = {
        let cache = app.discord.cache.read().unwrap();
        let guild = cache
            .guilds
            .get(guild_id)
            .map(|state| &state.data)
            .unwrap_or(&Json::Null);
        guild_author(
            guild_id,
            guild,
            format!(
                "BXH nối từ của {}",
                guild["name"].opt_str().unwrap_or("máy chủ")
            ),
        )
    };
    let store = app.store.lock().unwrap();
    let mut rows: Vec<_> = store.ranks.get(guild_id).into_iter().flatten().collect();
    rows.sort_by(|(a_id, a), (b_id, b)| {
        b.wins
            .cmp(&a.wins)
            .then_with(|| b.accuracy().total_cmp(&a.accuracy()))
            .then_with(|| b.correct.cmp(&a.correct))
            .then_with(|| a_id.cmp(b_id))
    });
    let fields = if rows.is_empty() {
        vec![field(
            "Top 10",
            "Chưa có ai chơi nối từ ở server này.",
            false,
        )]
    } else {
        rows.into_iter()
            .take(10)
            .enumerate()
            .map(|(index, (_, player))| {
                let name = clip(player.name.as_deref().unwrap_or("Người chơi"), 80)
                    .replace(['\n', '\r'], " ");
                field(
                    &format!("#{} · {}", index + 1, name),
                    format!(
                        "Win: **{}** · Từ đúng: **{}/{} ({:.2}%)**",
                        player.wins,
                        player.correct,
                        player.total,
                        player.accuracy() * 100.0,
                    ),
                    false,
                )
            })
            .collect()
    };
    let mut result = embed(fields);
    result["author"] = author;
    result
}

fn guild_icon(id: &str, guild: &Json) -> Option<String> {
    let hash = guild["icon"].opt_str()?;
    Some(format!(
        "https://cdn.discordapp.com/icons/{id}/{hash}.{}?size=128",
        if hash.starts_with("a_") { "gif" } else { "png" }
    ))
}

fn guild_author(id: &str, guild: &Json, name: String) -> Json {
    let mut author = json!({"name": name});
    if let Some(url) = guild_icon(id, guild) {
        author["icon_url"] = url.into();
    }
    author
}

fn server(app: &App, event: &Interaction) -> Result<Json, String> {
    let guild_id = event.guild();
    let (cached, member, bot) = {
        let cache = app.discord.cache.read().unwrap();
        let guild = cache.guilds.get(guild_id);
        (
            guild.map(|guild| guild.data.clone()).unwrap_or_default(),
            guild
                .map(|guild| guild.bot_member.clone())
                .unwrap_or_default(),
            cache.user["id"].str().to_owned(),
        )
    };
    let guild = if cached["name"].is_null() {
        app.discord
            .rest("GET", &format!("/guilds/{guild_id}"), None)?
    } else {
        cached
    };
    let member = if member["joined_at"].is_null() {
        app.discord
            .rest("GET", &format!("/guilds/{guild_id}/members/{bot}"), None)
            .unwrap_or_default()
    } else {
        member
    };
    let joined = crate::web::parse_discord_time(member["joined_at"].str())
        .map(|timestamp| format!("<t:{timestamp}:R>"))
        .unwrap_or_else(|| "Không rõ".into());
    let premium = app.store.lock().unwrap().premium.contains(guild_id);
    let mut result = embed(vec![
        field(":id: ID Server", guild_id, true),
        field(
            ":calendar: Thành lập",
            format!("<t:{}:R>", snowflake_ms(guild_id) / 1000),
            true,
        ),
        field(
            ":crown: Owner",
            format!("<@{}>", guild["owner_id"].str()),
            true,
        ),
        field(":robot: Ngày thêm Bot", joined, true),
        field(
            ":star: PhoBo Premium",
            if premium {
                ":white_check_mark: Đã kích hoạt"
            } else {
                ":closed_lock_with_key: Chưa kích hoạt"
            },
            true,
        ),
    ]);
    result["author"] = guild_author(
        guild_id,
        &guild,
        guild["name"].opt_str().unwrap_or("Máy chủ").to_owned(),
    );
    if let Some(url) = guild_icon(guild_id, &guild) {
        result["thumbnail"] = json!({"url": url});
    }
    Ok(result)
}

fn ping(app: &App, event: &mut Interaction) -> Result<(), String> {
    let (user, ping) = {
        let cached = app.discord.cache.read().unwrap();
        (cached.user.clone(), cached.ping)
    };
    let icon = avatar(&user);
    event.reply(json!({"embeds": [{"author": {"name": "Đang kiểm tra ping...", "icon_url": (&icon)}}], "flags": EPHEMERAL}))?;
    let total = now_ms().saturating_sub(snowflake_ms(event.data["id"].str()));
    let ping = ping
        .map(|ms| format!("{ms} ms"))
        .unwrap_or_else(|| "Đang đo".into());
    let mut result = embed(vec![
        field("Ping", format!("{total}ms"), true),
        field("Websocket", ping, true),
    ]);
    result["author"] =
        json!({"name": (format!("Ping của {}", user["username"].str())), "icon_url": icon});
    event.edit(json!({"embeds": [result]}))
}

fn help() -> Json {
    let mut result = embed(vec![
        field(
            "Nối từ",
            "Chọn kênh với \x60/set-channel\x60. Dùng \x60!start\x60 để bắt đầu, \x60!stop\x60 để sang ván mới. Mỗi lượt gửi một từ hai tiếng, bắt đầu bằng tiếng cuối của từ trước. Không lặp từ hoặc chơi hai lượt liên tiếp.",
            false,
        ),
        field(
            "Đếm số",
            "Chọn \x60/set-channel bot_mode:counting\x60 rồi đếm từ **1**. Đếm sai hoặc đếm hai lần liên tiếp sẽ reset về 0.",
            false,
        ),
        field(
            "Thống kê và xếp hạng",
            "\x60/rank\x60, \x60/me\x60, \x60/stats\x60, \x60/server\x60 và \x60/ping\x60.",
            false,
        ),
        field(
            "Báo cáo từ",
            "\x60/report word:<từ> type:report\x60 để báo cáo, hoặc \x60type:add\x60 để đề xuất từ mới.",
            false,
        ),
        field(
            "AI",
            "\x60/ai message:<câu hỏi>\x60 đọc ngữ cảnh kênh. Chọn \x60visible:false\x60 để câu trả lời chỉ hiện cho bạn.",
            false,
        ),
        field(
            "Quản lý",
            "\x60/set-channel\x60 và \x60/unset-channel\x60 cần **MANAGE_GUILD**. Lệnh prefix cho kênh hiện tại là \x60?phobo set\x60 và \x60?phobo unset\x60. \x60/leave\x60 xoá dữ liệu và cho bot rời máy chủ.",
            false,
        ),
        field(
            "Kênh báo cáo",
            "\x60/unblacklist\x60 kiểm tra hoặc gỡ từ bị chặn. \x60/broadcast\x60 gửi tin đến các kênh đã cấu hình. \x60/leave guild:<ID hoặc tên>\x60 chọn máy chủ khác để bot rời.",
            false,
        ),
        field(
            "Hỗ trợ và mã nguồn",
            "[Discord hỗ trợ](https://discord.gg/NC7Ks2c5wE) · [Mã nguồn OrenjiNeko](https://github.com/hakolilim/orenjineko)",
            false,
        ),
        field(
            "Chính sách và điều khoản",
            "[Quyền riêng tư](https://github.com/hakolilim/orenjineko/blob/main/PRIVACY_POLICY.md) · [Điều khoản](https://github.com/hakolilim/orenjineko/blob/main/TERMS_OF_SERVICE.md)",
            false,
        ),
    ]);
    result["title"] = "Hướng dẫn sử dụng OrenjiNeko".into();
    result
}

fn in_report_channel(app: &App, event: &Interaction) -> bool {
    !app.config.report_channel.is_empty() && app.config.report_channel == event.channel()
}

fn broadcast(app: &App, event: &mut Interaction) -> Result<(), String> {
    if app.config.report_channel.is_empty() {
        return event.text("Lệnh gửi tin hàng loạt hiện chưa được cấu hình.", true);
    }
    if !in_report_channel(app, event) {
        return event.text("Lệnh này chỉ dùng được trong kênh báo cáo.", true);
    }
    let message = event.option("message").str().to_owned();
    if message.trim().is_empty() {
        return event.text("Nội dung tin nhắn không được để trống.", true);
    }
    event.defer(true)?;
    let channels = app.store.lock().unwrap().channel_configs();
    if channels.is_empty() {
        return event.edit_text("Hiện chưa có kênh nối từ hoặc đếm số nào được cấu hình.");
    }
    let total = channels.len();
    let mut sent = 0;
    let mut failures = Vec::new();
    for (channel, guild) in channels {
        match broadcast_to(app, &channel, &guild, &message) {
            Ok(()) => sent += 1,
            Err(reason) => {
                eprintln!("Broadcast tới kênh {channel}: {reason}");
                failures.push((channel, reason));
            }
        }
    }
    let mut result = format!("Đã gửi thành công đến {sent}/{total} kênh.");
    let failed = failures.len();
    if failed > 0 {
        result.push_str(&format!("\nThất bại: {failed}"));
        for (channel, reason) in failures.into_iter().take(5) {
            result.push_str(&format!("\n• \x60{channel}\x60: {reason}"));
        }
        if failed > 5 {
            result.push_str(&format!("\n… và {} kênh khác.", failed - 5));
        }
    }
    event.edit_text(result)
}

fn broadcast_to(app: &App, channel: &str, guild: &str, message: &str) -> Result<(), &'static str> {
    let _activity = app.activity.begin(guild).ok_or("bot đang rời máy chủ")?;
    let channel_data = app
        .discord
        .rest("GET", &format!("/channels/{channel}"), None)
        .map_err(|_| "không tìm thấy hoặc bot không truy cập được")?;
    if channel_data["guild_id"].str() != guild {
        return Err("không khớp máy chủ đã cấu hình");
    }
    let kind = channel_data["type"].int();
    if !matches!(kind, 0 | 2 | 5 | 10..=13) {
        return Err("không phải kênh văn bản có thể gửi tin");
    }
    let required = VIEW_CHANNEL
        | if matches!(kind, 10..=12) {
            1 << 38
        } else {
            SEND_MESSAGES
        };
    if !has(
        app.discord
            .bot_permissions(guild, &channel_data)
            .unwrap_or_default(),
        required,
    ) {
        return Err("bot thiếu quyền xem hoặc gửi tin");
    }
    app.discord
        .send(channel, json!({"content": message}))
        .map_err(|_| "gửi tin thất bại")?;
    Ok(())
}

fn report(app: &App, event: &mut Interaction) -> Result<(), String> {
    let channel = &app.config.report_channel;
    if channel.is_empty() {
        return event.text("Tính năng báo cáo hiện không hoạt động!", true);
    }
    let word = normalize_word(event.option("word").str());
    if word.split_whitespace().count() != 2 || clip(&word, 500) != word {
        return event.text("Cụm từ không hợp lệ.", true);
    }
    let reason = event
        .option("reason")
        .opt_str()
        .filter(|reason| !reason.trim().is_empty())
        .unwrap_or("No reason provided.");
    let add = event.option("type").str() == "add";
    let invalid = {
        let store = app.store.lock().unwrap();
        let exists = store.dictionary.words.contains(&word);
        match (add, exists, store.dictionary.blacklist.contains(&word)) {
            (true, true, _) => Some("Cụm từ này đã có trong từ điển của Bot"),
            (false, false, _) => Some("Cụm từ này không có trong từ điển của Bot"),
            (false, true, true) => Some("Cụm từ này đã có trong danh sách đen của Bot"),
            _ => None,
        }
    };
    if let Some(error) = invalid {
        return event.text(error, true);
    }
    let user = event.user();
    let guild_id = event.guild();
    let mut report = embed(vec![
        field(
            if add {
                ":heavy_plus_sign: Từ đề xuất thêm"
            } else {
                ":regional_indicator_p: Từ báo cáo"
            },
            format!("**{word}**"),
            true,
        ),
        field(":bulb: Lý do", clip(reason, 1024), true),
        field(
            ":bust_in_silhouette: Người gửi",
            user["username"].str(),
            true,
        ),
        field(":shield: Máy chủ", app.discord.guild_name(guild_id), true),
        field(":id: ID server", guild_id, true),
        field("Trạng thái", ":clock12: Đang chờ", true),
    ]);
    report["timestamp"] = crate::web::timestamp().into();
    let cache = app.discord.cache.read().unwrap();
    if let Some(url) = cache
        .guilds
        .get(guild_id)
        .and_then(|guild| guild_icon(guild_id, &guild.data))
    {
        report["thumbnail"] = json!({"url": url});
    }
    drop(cache);
    let kind = if add { "add" } else { "report" };
    let owner = user["id"].str();
    let buttons = json!([{"type": 1, "components": [
        {"type": 2, "style": 3, "label": "Đồng ý", "custom_id": (format!("report:approve:{kind}:{owner}"))},
        {"type": 2, "style": 4, "label": "Từ chối", "custom_id": (format!("report:reject:{kind}:{owner}"))}
    ]}]);
    event.defer(true)?;
    app.discord
        .send(channel, json!({"embeds": [report], "components": buttons}))?;
    let action = if add { "thêm" } else { "báo cáo" };
    event.edit_text(format!("Đã gửi yêu cầu {action} từ **{word}**"))
}

fn moderate(app: &App, event: &mut Interaction) -> Result<(), String> {
    if !in_report_channel(app, event) {
        return event.text("Nút duyệt chỉ dùng được trong kênh báo cáo.", true);
    }
    let channel = event.channel().to_owned();
    let custom = event.data["data"]["custom_id"].str().to_owned();
    let parts = custom.split(':').collect::<Vec<_>>();
    let (approved, kind, owner) = match parts.as_slice() {
        [
            "report",
            action @ ("approve" | "reject"),
            kind @ ("add" | "report"),
            owner,
        ] => (*action == "approve", Some(*kind), Some(*owner)),
        // Reports posted by the JavaScript runtime encoded the category in the embed.
        ["accept"] => (true, None, None),
        ["decline"] => (false, None, None),
        _ => return event.text("Nút duyệt không hợp lệ.", true),
    };
    let moderator = display_name(event.user(), &event.data["member"]).to_owned();
    let message_id = event.data["message"]["id"].str().to_owned();
    event.callback(6, json!({}))?;
    let path = format!("/channels/{channel}/messages/{message_id}");
    let word;
    let blacklist;
    {
        let _moderation = app.moderation.lock().unwrap();
        let message = app.discord.rest("GET", &path, None)?;
        let button_exists = message["components"]
            .array()
            .iter()
            .flat_map(|row| row["components"].array())
            .any(|button| button["custom_id"].str() == custom);
        if !button_exists {
            return Ok(());
        }
        if message["author"]["id"].str() != app.discord.cache.read().unwrap().user["id"].str() {
            return Err("Phiếu duyệt không thuộc bot này".into());
        }
        let mut report = message["embeds"][0].clone();
        let mut fields = report["fields"].array().to_vec();
        if fields.len() != 6 {
            return Err("Phiếu duyệt không đầy đủ".into());
        }
        let marked = fields[0]["value"].str();
        word = marked
            .strip_prefix("**")
            .and_then(|text| text.strip_suffix("**"))
            .ok_or("Từ duyệt không hợp lệ")?
            .to_owned();
        blacklist = match kind {
            Some(kind) => kind == "report",
            None => match fields[0]["name"].str() {
                ":regional_indicator_p: Từ báo cáo" => true,
                ":heavy_plus_sign: Từ đề xuất thêm" => false,
                _ => return Err("Không xác định được loại phiếu duyệt cũ".into()),
            },
        };
        if approved {
            app.store.lock().unwrap().add_word(&word, blacklist)?;
        }
        fields[5]["value"] = (if approved {
            ":white_check_mark: Đã đồng ý"
        } else {
            ":x: Đã từ chối"
        })
        .into();
        report["fields"] = fields.into();
        for key in ["type", "provider", "video"] {
            if let Json::Object(fields) = &mut report {
                fields.remove(key);
            }
        }
        app.discord.rest(
            "PATCH",
            &path,
            Some(json!({"embeds": [report], "components": []})),
        )?;
    }
    let Some(owner) = owner else {
        return Ok(());
    };
    let action_label = if approved {
        "được đồng ý"
    } else {
        "bị từ chối"
    };
    let change_label = if blacklist {
        "gỡ bỏ"
    } else {
        "thêm vào từ điển"
    };
    let description = format!(
        "Từ \x60{word}\x60 của bạn đã {action_label} {change_label} bởi mod \x60{moderator}\x60"
    );
    let notified = app
        .discord
        .rest(
            "POST",
            "/users/@me/channels",
            Some(json!({"recipient_id": owner})),
        )
        .and_then(|dm| {
            app.discord.send(
                dm["id"].str(),
                json!({"embeds": [{"color": COLOR, "description": description}]}),
            )
        });
    if let Err(error) = notified {
        eprintln!("Không gửi được DM báo cáo từ: {error}");
    }
    Ok(())
}

fn unblacklist(app: &App, event: &mut Interaction) -> Result<(), String> {
    if app.config.report_channel.is_empty() {
        return event.text("Tính năng blacklist hiện không hoạt động!", true);
    }
    if !in_report_channel(app, event) {
        return event.text("Lệnh này chỉ dùng được trong kênh báo cáo.", true);
    }
    let word = normalize_word(event.option("word").str());
    if word.split_whitespace().count() != 2 || clip(&word, 500) != word {
        return event.text("Cụm từ không hợp lệ.", true);
    }
    let remove = event.option("action").str() == "remove";
    if remove && !event.can_manage() {
        return event.text("Bạn cần có quyền admin để gỡ từ khỏi blacklist.", true);
    }
    if !remove {
        let blacklisted = app
            .store
            .lock()
            .unwrap()
            .dictionary
            .blacklist
            .contains(&word);
        let description = format!(
            "**{word}** {} nằm trong blacklist của bot.",
            if blacklisted { "đang" } else { "không" }
        );
        return event.reply(json!({"flags": EPHEMERAL, "embeds": [{"title": "Kết quả kiểm tra", "color": COLOR, "description": description}]}));
    }
    event.defer(true)?;
    let removed = app.store.lock().unwrap().unblacklist(&word)?;
    if !removed {
        return event.edit_text(format!("**{word}** không nằm trong blacklist."));
    }
    event.edit(json!({"embeds": [{"title": "Đã gỡ blacklist", "color": COLOR,
        "description": (format!("**{word}** đã được gỡ khỏi blacklist và được nạp lại vào từ điển runtime."))
    }]}))?;
    app.discord.send(&app.config.report_channel, json!({"embeds": [{
        "title": "Blacklist updated", "color": COLOR,
        "description": (format!("**{word}** đã được gỡ khỏi blacklist bởi **{}**.", event.user()["username"].str()))
    }]}))?;
    Ok(())
}

fn normalize_guild(text: &str) -> String {
    const VIETNAMESE: [(char, &str); 7] = [
        ('a', "àáảãạăằắẳẵặâầấẩẫậ"),
        ('e', "èéẻẽẹêềếểễệ"),
        ('i', "ìíỉĩị"),
        ('o', "òóỏõọôồốổỗộơờớởỡợ"),
        ('u', "ùúủũụưừứửữự"),
        ('y', "ỳýỷỹỵ"),
        ('d', "đ"),
    ];
    text.trim()
        .to_lowercase()
        .chars()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .map(|c| {
            VIETNAMESE
                .iter()
                .find(|(_, letters)| letters.contains(c))
                .map_or(c, |(plain, _)| *plain)
        })
        .collect()
}

fn guild_choices(app: &App) -> Vec<(String, String)> {
    let cache = app.discord.cache.read().unwrap();
    cache
        .guilds
        .iter()
        .filter_map(|(id, guild)| {
            guild.data["name"]
                .opt_str()
                .map(|name| (id.clone(), name.to_owned()))
        })
        .collect()
}

/// Names are only known for the guilds this shard serves; an ID works for any guild.
fn other_shards_note(app: &App) -> String {
    if app.config.shard_count == 1 {
        return String::new();
    }
    format!(
        " Gợi ý và tìm theo tên chỉ gồm máy chủ thuộc shard {}/{}; máy chủ ở shard khác hãy nhập ID.",
        app.config.shard_id, app.config.shard_count
    )
}

fn autocomplete(app: &App, event: &mut Interaction) -> Result<(), String> {
    let mut choices = Vec::new();
    if event.data["data"]["name"].str() == "leave"
        && !event.guild().is_empty()
        && event.can_manage()
        && in_report_channel(app, event)
        && let Some(focused) = event.data["data"]["options"]
            .array()
            .iter()
            .find(|option| option["name"].str() == "guild" && option["focused"].bool())
    {
        let query = normalize_guild(focused["value"].str());
        let mut guilds = guild_choices(app);
        guilds.retain(|(id, name)| id.contains(&query) || normalize_guild(name).contains(&query));
        guilds.sort_by_cached_key(|(id, name)| (normalize_guild(name), name.clone(), id.clone()));
        choices = guilds
            .into_iter()
            .take(25)
            .map(|(id, name)| {
                let name = format!("{} ({id})", clip(&name, 100 - id.len().min(97) - 3));
                json!({"name": name, "value": id})
            })
            .collect();
    }
    event.callback(8, json!({"choices": choices}))
}

fn leave(app: &App, event: &mut Interaction) -> Result<(), String> {
    if event.guild().is_empty() {
        return event.text("Lệnh này chỉ dùng được trong máy chủ.", true);
    }
    if !event.can_manage() {
        return event.text(
            "Bạn cần có quyền \x60MANAGE_GUILD\x60 để thực hiện thao tác này!",
            true,
        );
    }
    let selected = event
        .option("guild")
        .opt_str()
        .or_else(|| event.option("guild_id").opt_str());
    let guild = match selected {
        Some(selection) => {
            if !in_report_channel(app, event) {
                return event.text("Tuỳ chọn \x60guild\x60 chỉ dùng được trong kênh báo cáo đã cấu hình (\x60REPORT_CHANNEL\x60).", true);
            }
            let input = selection.trim();
            if (17..=20).contains(&input.len()) && input.bytes().all(|byte| byte.is_ascii_digit()) {
                input.to_owned()
            } else {
                let input = normalize_guild(input);
                let matches = guild_choices(app)
                    .into_iter()
                    .filter(|(_, name)| !input.is_empty() && normalize_guild(name) == input)
                    .collect::<Vec<_>>();
                match matches.as_slice() {
                    [(id, _)] => id.clone(),
                    [] => return event.text(format!("Không tìm thấy máy chủ theo tên này. Hãy chọn từ danh sách gợi ý hoặc nhập ID Discord gồm 17–20 chữ số.{}", other_shards_note(app)), true),
                    _ => return event.text("Có nhiều máy chủ trùng tên. Hãy chọn đúng máy chủ từ danh sách gợi ý có kèm ID.", true),
                }
            }
        }
        None => event.guild().to_owned(),
    };
    let retry = if selected.is_some() {
        format!("/leave guild:{guild}")
    } else {
        "/leave".into()
    };
    let Some(lock) = app.activity.leave(&guild) else {
        return event.text(
            format!("Bot đang xoá dữ liệu và rời máy chủ \x60{guild}\x60."),
            true,
        );
    };
    event.defer(true)?;
    lock.drain();
    let channels = match app
        .discord
        .rest("GET", &format!("/guilds/{guild}/channels"), None)
    {
        Ok(channels) => channels,
        Err(error) => {
            eprintln!("Tìm máy chủ để rời {guild}: {error}");
            drop(lock);
            return event.edit_text(format!(
                "Không thể truy cập danh sách kênh của máy chủ \x60{guild}\x60. Chưa xoá dữ liệu."
            ));
        }
    };
    let cleaned = app.store.lock().unwrap().remove_guild(
        &guild,
        channels
            .array()
            .iter()
            .filter_map(|channel| channel["id"].opt_str().map(str::to_owned)),
    );
    if let Err(error) = cleaned {
        eprintln!("Xoá dữ liệu máy chủ {guild}: {error}");
        drop(lock);
        return event.edit_text(format!("Chưa thể xoá hết dữ liệu của máy chủ \x60{guild}\x60 nên bot chưa rời. Một phần dữ liệu có thể đã được xoá; vui lòng thử lại \x60{retry}\x60."));
    }
    if let Err(error) = event.edit_text(format!("Đã xoá toàn bộ cấu hình, trạng thái trò chơi, bảng xếp hạng và Premium của máy chủ \x60{guild}\x60 trên Supabase. Bot sẽ rời máy chủ ngay bây giờ.")) {
        eprintln!("Phản hồi lệnh /leave: {error}");
    }
    if let Err(error) = app
        .discord
        .rest("DELETE", &format!("/users/@me/guilds/{guild}"), None)
    {
        eprintln!("Rời máy chủ {guild}: {error}");
        drop(lock);
        return event.edit_text(format!("Đã xoá dữ liệu của máy chủ \x60{guild}\x60, nhưng bot chưa rời được. Vui lòng thử lại \x60{retry}\x60."));
    }
    lock.complete();
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn slash_registry_keeps_all_original_commands_and_guild_selection() {
        let definitions = definitions();
        assert_eq!(definitions.array().len(), 13);
        let leave = definitions
            .array()
            .iter()
            .find(|item| item["name"].str() == "leave")
            .unwrap();
        assert_eq!(leave["default_member_permissions"].str(), "32");
        assert!(leave["options"][0]["autocomplete"].bool());
        assert_eq!(normalize_guild("  Đắk Lắk  "), "dak lak");
        assert_eq!(normalize_guild("Đa\u{306}\u{301}k"), "dak");
    }

    #[test]
    fn ranking_keeps_player_stats_together_in_rank_order() {
        let (app, _) = crate::test_support::mock(
            crate::test_support::database(&[("words", crate::test_support::dictionary())]),
            || crate::test_support::app(crate::test_support::store()),
        );
        {
            let mut store = app.store.lock().unwrap();
            store.ranks.insert(
                "guild".into(),
                HashMap::from([
                    (
                        "first".into(),
                        crate::store::Player {
                            name: Some("Tên dài hơn".into()),
                            wins: 4,
                            total: 20,
                            correct: 15,
                            ..Default::default()
                        },
                    ),
                    (
                        "second".into(),
                        crate::store::Player {
                            name: Some("An".into()),
                            wins: 2,
                            total: 4,
                            correct: 3,
                            ..Default::default()
                        },
                    ),
                ]),
            );
        }
        let result = ranking(&app, "guild");
        let fields = result["fields"].array();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0]["name"].str(), "#1 · Tên dài hơn");
        assert_eq!(
            fields[0]["value"].str(),
            "Win: **4** · Từ đúng: **15/20 (75.00%)**"
        );
        assert_eq!(fields[1]["name"].str(), "#2 · An");
        assert_eq!(
            fields[1]["value"].str(),
            "Win: **2** · Từ đúng: **3/4 (75.00%)**"
        );
        assert!(fields.iter().all(|field| !field["inline"].bool()));
        assert!(result["description"].is_null());
    }

    #[test]
    fn ranking_shows_empty_state_without_a_table() {
        let (app, _) = crate::test_support::mock(
            crate::test_support::database(&[("words", crate::test_support::dictionary())]),
            || crate::test_support::app(crate::test_support::store()),
        );
        let result = ranking(&app, "guild");
        assert_eq!(
            result["fields"][0]["value"].str(),
            "Chưa có ai chơi nối từ ở server này."
        );
        assert!(!result["fields"][0]["inline"].bool());
    }
}

#[cfg(test)]
mod broadcast_tests {
    use super::*;
    use crate::discord::Guild;
    use crate::test_support::{Reply, app, database, dictionary, mock, store};

    #[test]
    fn broadcast_deduplicates_channels_and_never_mentions_anyone() {
        let mut replies = database(&[
            ("words", dictionary()),
            (
                "guild_config",
                vec![json!({"channel_id": "one", "guild_id": "guild"})],
            ),
            (
                "counting_channels",
                vec![
                    json!({"channel_id": "one", "guild_id": "guild", "current_count": "2"}),
                    json!({"channel_id": "two", "guild_id": "guild", "current_count": "0"}),
                ],
            ),
        ]);
        replies.extend([
            Reply::empty("POST", "/api/v10/interactions/interaction/token/callback"),
            Reply::json(
                "GET",
                "/api/v10/channels/one",
                200,
                json!({"id": "one", "guild_id": "guild", "type": 0}),
            ),
            Reply::json(
                "POST",
                "/api/v10/channels/one/messages",
                200,
                json!({"id": "message"}),
            ),
            Reply::json(
                "GET",
                "/api/v10/channels/two",
                404,
                json!({"message": "unknown channel"}),
            ),
            Reply::empty("PATCH", "/api/v10/webhooks/app/token/messages/@original"),
        ]);
        let (_, requests) = mock(replies, || {
            let app = app(store());
            {
                let mut cache = app.discord.cache.write().unwrap();
                cache.user = json!({"id": "bot", "username": "Neko"});
                cache.guilds.insert("guild".into(), Guild::from_gateway(json!({
                    "id": "guild", "owner_id": "owner",
                    "roles": [{"id": "guild", "permissions": ((VIEW_CHANNEL | SEND_MESSAGES).to_string())}],
                    "members": [{"user": {"id": "bot"}, "roles": []}]
                }), "bot"));
            }
            let data = json!({
                "type": 2, "id": "interaction", "token": "token", "application_id": "app",
                "channel_id": "admin", "guild_id": "guild",
                "data": {"name": "broadcast", "options": [{"name": "message", "value": "@everyone <@123>"}]}
            });
            interaction(&Arc::new(app), data);
        });
        let writes = &requests[requests.len() - 5..];
        assert_eq!(writes[0].body["data"]["flags"].int(), i64::from(EPHEMERAL));
        assert!(writes[0].headers.iter().any(|header| header
            == concat!(
                "User-Agent: DiscordBot (https://github.com/hakolilim/orenjineko, ",
                env!("CARGO_PKG_VERSION"),
                ")"
            )));
        assert!(
            writes[0]
                .headers
                .iter()
                .all(|header| !header.starts_with("Authorization:"))
        );
        assert_eq!(writes[2].path, "/api/v10/channels/one/messages");
        assert_eq!(writes[2].body["content"].str(), "@everyone <@123>");
        assert_eq!(writes[2].body["allowed_mentions"]["parse"], json!([]));
        assert!(writes[4].body["content"].str().contains("1/2 kênh"));
    }
}
