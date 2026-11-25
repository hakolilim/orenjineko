use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::discord::{
    ADD_REACTIONS, ATTACH_FILES, EMBED_LINKS, READ_HISTORY, SEND_MESSAGES, VIEW_CHANNEL, avatar,
    now_ms,
};
use crate::http::decode_component;
use crate::json::Json;
use crate::{App, SHUTDOWN};

const CSS: &str = include_str!("../templates/site.css");
const STATUS_CSS: &str = include_str!("../templates/status.css");
const SUGGEST_CSS: &str = include_str!("../templates/suggest.css");
const LEGAL_CSS: &str = include_str!("../templates/legal.css");
const PRIVACY: &str = include_str!("../PRIVACY_POLICY.md");
const TERMS: &str = include_str!("../TERMS_OF_SERVICE.md");
const STATUS_REFRESH: u32 = 30;
const HEADER_LIMIT: u64 = 16 * 1024;

pub fn start(app: Arc<App>) -> Result<JoinHandle<()>, String> {
    let listener = TcpListener::bind(("0.0.0.0", app.config.port))
        .map_err(|error| format!("Mở cổng web: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    eprintln!("Web đang nghe tại 0.0.0.0:{}", app.config.port);
    Ok(thread::spawn(move || {
        let (send, receive) = sync_channel(64);
        let receive = Arc::new(Mutex::new(receive));
        let workers = (0..4)
            .map(|_| {
                let receive = Arc::clone(&receive);
                let app = Arc::clone(&app);
                thread::spawn(move || {
                    loop {
                        let stream = receive.lock().unwrap().recv();
                        let Ok(stream) = stream else {
                            break;
                        };
                        if SHUTDOWN.load(Ordering::Relaxed) {
                            break;
                        }
                        let _ = serve(stream, &app);
                    }
                })
            })
            .collect::<Vec<_>>();
        while !SHUTDOWN.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = send.try_send(stream);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(30))
                }
                Err(error) => {
                    eprintln!("Web nhận kết nối: {error}");
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
        drop(send);
        for worker in workers {
            let _ = worker.join();
        }
    }))
}

fn serve(mut stream: TcpStream, app: &App) -> std::io::Result<()> {
    use std::io::Read;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut reader = BufReader::new(&mut stream).take(HEADER_LIMIT);
    let mut request = String::new();
    if reader.read_line(&mut request)? == 0 {
        return Ok(());
    }
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            return Response::text(400, "Yêu cầu HTTP không hợp lệ.").write(&mut stream, false);
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
    }
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    let response = if !target.starts_with('/')
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || parts.next().is_some()
    {
        Response::text(400, "Yêu cầu HTTP không hợp lệ.")
    } else if matches!(method, "GET" | "HEAD") {
        route(app, target)
    } else {
        let mut response = Response::text(405, "Phương thức HTTP không được hỗ trợ.");
        response.header = Some(("Allow", "GET, HEAD".into()));
        response
    };
    response.write(&mut stream, method == "HEAD")
}

struct Response {
    code: u16,
    kind: &'static str,
    body: String,
    header: Option<(&'static str, String)>,
}

impl Response {
    fn text(code: u16, text: impl Into<String>) -> Self {
        Self::new(code, "text/plain; charset=utf-8", text.into())
    }
    fn html(body: String) -> Self {
        Self::new(200, "text/html; charset=utf-8", body)
    }
    fn json(code: u16, body: Json) -> Self {
        Self::new(code, "application/json; charset=utf-8", body.encode())
    }
    fn new(code: u16, kind: &'static str, body: String) -> Self {
        Self {
            code,
            kind,
            body,
            header: None,
        }
    }

    fn write(self, stream: &mut TcpStream, head: bool) -> std::io::Result<()> {
        let phrase = match self.code {
            200 => "OK",
            302 => "Found",
            400 => "Bad Request",
            404 => "Not Found",
            405 => "Method Not Allowed",
            503 => "Service Unavailable",
            _ => "Error",
        };
        let mut headers = format!(
            "HTTP/1.1 {} {phrase}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
             Connection: close\r\nX-Content-Type-Options: nosniff\r\n",
            self.code,
            self.kind,
            self.body.len()
        );
        if let Some((name, value)) = self.header {
            let _ = write!(headers, "{name}: {value}\r\n");
        }
        headers.push_str("\r\n");
        stream.write_all(headers.as_bytes())?;
        if !head {
            stream.write_all(self.body.as_bytes())?;
        }
        Ok(())
    }
}

fn route(app: &App, target: &str) -> Response {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let path = if path == "/" {
        "/"
    } else {
        path.trim_end_matches('/')
    };
    match path {
        "/health" => Response::json(200, json!({"status": "ok"})),
        "/api/status" => Response::json(200, status(app)),
        "/" => Response::html(render_status(&status(app))),
        "/api/suggest" | "/suggest" => {
            let word = query_word(query);
            if path == "/api/suggest" && word.is_empty() {
                return Response::json(400, json!({"error": "Missing word parameter"}));
            }
            let results = if word.is_empty() {
                Vec::new()
            } else {
                app.store
                    .lock()
                    .unwrap()
                    .dictionary
                    .next(&word)
                    .map(str::to_owned)
                    .collect()
            };
            if path == "/api/suggest" {
                Response::json(200, json!({"word": word, "suggestions": results}))
            } else {
                Response::html(render_suggest(&word, &results))
            }
        }
        "/invite" => {
            let app_id = app.discord.cache.read().unwrap().application.clone();
            if app_id.is_empty() {
                return Response::text(503, "Chưa thể tạo liên kết mời bot. Vui lòng thử lại sau.");
            }
            let permissions = VIEW_CHANNEL
                | SEND_MESSAGES
                | EMBED_LINKS
                | ATTACH_FILES
                | ADD_REACTIONS
                | READ_HISTORY;
            let location = format!(
                "https://discord.com/oauth2/authorize?client_id={app_id}&permissions={permissions}&scope=bot%20applications.commands"
            );
            let mut response = Response::text(302, "Đang chuyển tới Discord.");
            response.header = Some(("Location", location));
            response
        }
        "/privacy-policy" => {
            static PAGE: OnceLock<String> = OnceLock::new();
            Response::html(PAGE.get_or_init(|| legal_page(path, "Privacy Policy",
                "How OrenjiNeko processes game data, AI context, and requests to delete your data.", PRIVACY)).clone())
        }
        "/terms-of-service" => {
            static PAGE: OnceLock<String> = OnceLock::new();
            Response::html(PAGE.get_or_init(|| legal_page(path, "Terms of Service",
                "Terms for using OrenjiNeko, user responsibilities, and how to contact support.", TERMS)).clone())
        }
        _ => Response::text(404, "Không tìm thấy trang."),
    }
}

fn query_word(query: &str) -> String {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| decode_component(key).as_deref() == Some("word"))
        .and_then(|(_, word)| decode_component(word))
        .map(|word| word.trim().to_lowercase())
        .unwrap_or_default()
}

fn status(app: &App) -> Json {
    let (ready, guilds, tag, icon) = {
        let cache = app.discord.cache.read().unwrap();
        if cache.ready {
            let user = &cache.user;
            let username = user["username"].str();
            let discriminator = user["discriminator"].str();
            let tag = if matches!(discriminator, "" | "0") {
                username.to_owned()
            } else {
                format!("{username}#{discriminator}")
            };
            (true, cache.guilds.len(), Some(tag), Some(avatar(user)))
        } else {
            (false, 0, None, None)
        }
    };
    let store = app.store.lock().unwrap();
    let uptime = app.started.elapsed();
    json!({
        "status": (if ready { "online" } else { "booting" }), "ready": ready, "bot": {"tag": tag, "avatar": icon},
        "guilds": guilds, "uptimeMs": (uptime.as_millis()), "uptime": (format_uptime(uptime.as_secs())),
        "stats": {
            "wordPlayed": (store.words), "roundPlayed": (store.rounds), "players": (store.players()),
            "dictionaryWords": (store.dictionary.count())
        }, "timestamp": (timestamp())
    })
}

fn format_uptime(seconds: u64) -> String {
    let parts = [
        (seconds / 86_400, "d"),
        (seconds / 3600 % 24, "h"),
        (seconds / 60 % 60, "m"),
        (seconds % 60, "s"),
    ];
    let first = parts.iter().position(|(value, _)| *value > 0).unwrap_or(3);
    parts[first..]
        .iter()
        .map(|(value, unit)| format!("{value}{unit}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn escape(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '"' => result.push_str("&quot;"),
            '\'' => result.push_str("&#39;"),
            character => result.push(character),
        }
    }
    result
}

struct Page<'a> {
    lang: &'a str,
    route: &'a str,
    title: &'a str,
    description: &'a str,
    styles: &'a str,
    content: &'a str,
    sidebar: Option<&'a str>,
    refresh: Option<u32>,
}

impl Page<'_> {
    fn html(&self) -> String {
        let refresh = self
            .refresh
            .map(|seconds| format!(r#"<meta http-equiv="refresh" content="{seconds}">"#))
            .unwrap_or_default();
        let skip = if self.lang == "vi" {
            "Đến nội dung chính"
        } else {
            "Skip to main content"
        };
        let english = if self.lang == "vi" {
            r#" lang="en""#
        } else {
            ""
        };
        let class = if self.sidebar.is_some() {
            ""
        } else {
            " layout--single"
        };
        let sidebar = self.sidebar.unwrap_or_default();
        let mut navigation = String::new();
        for (href, name, vi) in [
            ("/", "Bot Status", false),
            ("/suggest", "Gợi ý từ nối tiếp", true),
            ("/invite", "Mời bot", true),
            ("/privacy-policy", "Privacy Policy", false),
            ("/terms-of-service", "Terms of Service", false),
        ] {
            let attr_lang = if vi { r#" lang="vi""# } else { "" };
            let current = if href == self.route {
                r#" aria-current="page""#
            } else {
                ""
            };
            let _ = write!(
                navigation,
                r#"<a href="{href}"{attr_lang}{current}>{name}</a>"#
            );
        }
        format!(
            r##"<!DOCTYPE html>
<html lang="{lang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="description" content="{description}">
{refresh}<title>{title} — OrenjiNeko</title>
<style>{CSS}{styles}</style>
</head>
<body>
<a class="skip-link" href="#content">{skip}</a>
<header class="site-header"><div class="header-inner">
<a class="brand" href="/">Orenji<span>Neko</span></a>
<nav class="site-nav"{english} aria-label="Main navigation">{navigation}</nav>
</div></header>
<div class="layout{class}">{sidebar}<main id="content" tabindex="-1">{content}</main></div>
<footer class="site-footer"{english}>
<p>OrenjiNeko · Word-chain games, counting games, and AI chat.</p>
<p>Contact hakolilim through the <a href="https://discord.gg/NC7Ks2c5wE">Support Discord Server</a> · <a href="https://github.com/hakolilim/Phobophake">GitHub</a></p>
</footer>
</body>
</html>"##,
            lang = self.lang,
            title = escape(self.title),
            description = escape(self.description),
            styles = self.styles,
            content = self.content
        )
    }
}

fn number(value: i64) -> String {
    let raw = value.to_string();
    let mut output = String::new();
    let count = raw.trim_start_matches('-').len();
    for (index, item) in raw.trim_start_matches('-').chars().enumerate() {
        if index > 0 && (count - index).is_multiple_of(3) {
            output.push('.');
        }
        output.push(item);
    }
    if value < 0 {
        output.insert(0, '-');
    }
    output
}

fn render_status(data: &Json) -> String {
    let bot = &data["bot"];
    let name = escape(bot["tag"].opt_str().unwrap_or("OrenjiNeko"));
    let icon = bot["avatar"]
        .opt_str()
        .map(|avatar| {
            format!(
                r#"<img class="status-avatar" src="{}" width="56" height="56" alt="">"#,
                escape(avatar)
            )
        })
        .unwrap_or_else(|| r#"<span class="status-avatar" aria-hidden="true">🍊</span>"#.into());
    let ready = data["ready"].bool();
    let label = if ready {
        "Đang hoạt động"
    } else {
        "Đang khởi động"
    };
    let badge = if ready { "" } else { " is-booting" };
    let stats = &data["stats"];
    let mut metrics = String::new();
    for (title, value) in [
        ("Máy chủ Discord", number(data["guilds"].int())),
        ("Thời gian hoạt động", escape(data["uptime"].str())),
        ("Người chơi", number(stats["players"].int())),
        ("Từ đã nối", number(stats["wordPlayed"].int())),
        ("Ván đã chơi", number(stats["roundPlayed"].int())),
        ("Từ trong từ điển", number(stats["dictionaryWords"].int())),
    ] {
        let _ = write!(
            metrics,
            r#"<div class="status-metric"><dt>{title}</dt><dd>{value}</dd></div>"#
        );
    }
    let timestamp = escape(data["timestamp"].str());
    let updated = format!("{} {}", &timestamp[11..19], &timestamp[..10]);
    let content = format!(
        r#"<section class="panel" aria-labelledby="status-title">
<h1 id="status-title">Trạng thái bot</h1>
<div class="status-header"><div class="status-identity">{icon}<div>
<p class="status-name">{name}</p><p class="status-description">Bot nối từ tiếng Việt</p>
</div></div><span class="status-badge{badge}">{label}</span></div>
<dl class="status-metrics">{metrics}</dl>
<p class="status-meta"><span>Cập nhật lúc <time datetime="{timestamp}">{updated} UTC</time></span>
<span>Tự làm mới mỗi {STATUS_REFRESH} giây</span></p></section>"#
    );
    Page {
        lang: "vi",
        route: "/",
        title: "Trạng thái bot",
        description: "Trạng thái kết nối Discord và thống kê trò chơi nối từ của OrenjiNeko.",
        styles: STATUS_CSS,
        content: &content,
        sidebar: None,
        refresh: Some(STATUS_REFRESH),
    }
    .html()
}

fn render_suggest(word: &str, suggestions: &[String]) -> String {
    let feedback = match (word.is_empty(), suggestions.len()) {
        (true, _) => String::new(),
        (false, 0) => "Không tìm thấy từ nào có thể nối tiếp.".into(),
        (false, count) => format!("Tìm thấy {count} từ có thể nối tiếp."),
    };
    let results = suggestions
        .iter()
        .map(|item| format!("<li>{}</li>", escape(item)))
        .collect::<String>();
    let word = escape(word);
    let content = format!(
        r#"<section class="panel" aria-labelledby="suggest-title">
<h1 id="suggest-title">Gợi ý từ nối tiếp</h1>
<p class="suggest-intro">Các từ gợi ý sẽ bắt đầu bằng tiếng cuối của từ bạn nhập.</p>
<form class="suggest-form" action="/suggest" method="get">
<label for="suggest-word">Từ cần nối</label><div class="suggest-controls">
<input id="suggest-word" name="word" type="text" placeholder="Ví dụ: phở bò" value="{word}" required>
<button type="submit">Gợi ý</button></div></form>
<p class="suggest-feedback" role="status">{feedback}</p>
<ul class="suggest-results" role="list" aria-label="Các từ có thể nối tiếp">{results}</ul>
</section>"#
    );
    Page {
        lang: "vi",
        route: "/suggest",
        title: "Gợi ý từ nối tiếp",
        description: "Tra các từ có thể nối tiếp từ bạn nhập trong từ điển OrenjiNeko.",
        styles: SUGGEST_CSS,
        content: &content,
        sidebar: None,
        refresh: None,
    }
    .html()
}

fn legal_page(route: &str, title: &str, description: &str, doc: &str) -> String {
    let mut sections = String::new();
    let mut article = String::from("<article class=\"panel\">");
    let mut index = 0;
    for block in doc
        .trim()
        .split("\n\n")
        .filter(|block| !block.trim().is_empty())
    {
        if let Some(title) = block.strip_prefix("# ") {
            let _ = write!(article, "<h1>{}</h1>", inline_markdown(title));
        } else if let Some(title) = block.strip_prefix("## ") {
            index += 1;
            let _ = write!(
                article,
                r#"<h2 id="section-{index}">{}</h2>"#,
                inline_markdown(title)
            );
            let _ = write!(
                sections,
                r##"<li><a href="#section-{index}">{}</a></li>"##,
                escape(title)
            );
        } else if block.starts_with("- ") {
            article.push_str("<ul>");
            for item in block.lines() {
                let _ = write!(
                    article,
                    "<li>{}</li>",
                    inline_markdown(item.trim_start_matches("- "))
                );
            }
            article.push_str("</ul>");
        } else {
            let _ = write!(
                article,
                "<p>{}</p>",
                inline_markdown(&block.replace('\n', " "))
            );
        }
    }
    article.push_str("</article>");
    let sidebar = format!(
        r#"<aside aria-label="Table of contents"><h2>On this page</h2>
<nav aria-label="Document sections"><ul class="sidebar-links">{sections}</ul></nav></aside>"#
    );
    Page {
        lang: "en",
        route,
        title,
        description,
        styles: LEGAL_CSS,
        content: &article,
        sidebar: Some(&sidebar),
        refresh: None,
    }
    .html()
}

fn inline_markdown(mut text: &str) -> String {
    let mut output = String::new();
    while !text.is_empty() {
        let match_at = text.find(['\x60', '*', '[']).unwrap_or(text.len());
        output.push_str(&escape(&text[..match_at]));
        text = &text[match_at..];
        if text.is_empty() {
            break;
        }
        if let Some((tag, body, tail)) = markdown_span(text) {
            let _ = write!(output, "<{tag}>{}</{tag}>", escape(body));
            text = tail;
        } else if let Some((href, label, tail)) = markdown_link(text) {
            let _ = write!(
                output,
                r#"<a href="{}">{}</a>"#,
                escape(href),
                escape(label)
            );
            text = tail;
        } else {
            output.push_str(&escape(&text[..1]));
            text = &text[1..];
        }
    }
    output
}

fn markdown_span(text: &str) -> Option<(&'static str, &str, &str)> {
    let (tag, marker) = if text.starts_with('\x60') {
        ("code", "\x60")
    } else {
        ("strong", "**")
    };
    let body = text.strip_prefix(marker)?;
    let (body, tail) = body.split_once(marker)?;
    (!body.is_empty()).then_some((tag, body, tail))
}

fn markdown_link(text: &str) -> Option<(&str, &str, &str)> {
    let (label, url) = text.strip_prefix('[')?.split_once("](")?;
    let (url, tail) = url.split_once(')')?;
    let safe = match url {
        "PRIVACY_POLICY.md" => "/privacy-policy",
        "TERMS_OF_SERVICE.md" => "/terms-of-service",
        link if link.starts_with("https://") => link,
        _ => return None,
    };
    Some((safe, label, tail))
}

pub fn timestamp() -> String {
    let ms = now_ms() as i64;
    let seconds = ms / 1000;
    let (year, month, day) = civil_from_days(seconds / 86_400);
    let (hour, minute, second) = (seconds / 3600 % 24, seconds / 60 % 60, seconds % 60);
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        ms % 1000
    )
}

// Civil date conversion uses the Gregorian 400-year cycle.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let in_era = days.rem_euclid(146_097);
    let year_in_era = (in_era - in_era / 1460 + in_era / 36_524 - in_era / 146_096) / 365;
    let day_in_year = in_era - (365 * year_in_era + year_in_era / 4 - year_in_era / 100);
    let month_march = (5 * day_in_year + 2) / 153;
    let day = day_in_year - (153 * month_march + 2) / 5 + 1;
    let month = if month_march < 10 {
        month_march + 3
    } else {
        month_march - 9
    };
    (year_in_era + era * 400 + i64::from(month <= 2), month, day)
}

pub fn parse_discord_time(timestamp: &str) -> Option<i64> {
    let (date, time) = timestamp.split_once('T')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let (time, offset) = if let Some(time) = time.strip_suffix('Z') {
        (time, 0)
    } else {
        let position = time.find(['+', '-'])?;
        let (time, offset) = time.split_at(position);
        let (hours, minutes) = offset[1..].split_once(':')?;
        let sign = if offset.starts_with('+') { 1 } else { -1 };
        (
            time,
            sign * (3600 * hours.parse::<i64>().ok()? + 60 * minutes.parse::<i64>().ok()?),
        )
    };
    let mut time = time.split('.').next()?.split(':').map(str::parse::<i64>);
    let (hour, minute, second) = (time.next()?.ok()?, time.next()?.ok()?, time.next()?.ok()?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_in_era = year.rem_euclid(400);
    let month_march = if month > 2 { month - 3 } else { month + 9 };
    let day_in_era =
        year_in_era * 365 + year_in_era / 4 - year_in_era / 100 + (153 * month_march + 2) / 5 + day
            - 1;
    Some(
        (era * 146_097 + day_in_era - 719_468) * 86_400 + 3600 * hour + 60 * minute + second
            - offset,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_html_and_preserves_status_queries() {
        assert_eq!(
            query_word("other=1&word=%20H%E1%BB%8CC+sinh%20"),
            "học sinh"
        );
        assert!(query_word("word=%FF").is_empty());
        let page = render_suggest("<script>", &["học <b>".into()]);
        assert!(page.contains("value=\"&lt;script&gt;\""));
        assert!(page.contains("<li>học &lt;b&gt;</li>"));
        let terms = legal_page("/terms-of-service", "Terms of Service", "", TERMS);
        assert!(terms.contains("href=\"/privacy-policy\">Privacy Policy</a>"));
        assert!(terms.contains("<h2 id=\"section-1\">Acceptance of These Terms</h2>"));
        assert_eq!(
            inline_markdown("[test](javascript:alert(1)) <b>"),
            "[test](javascript:alert(1)) &lt;b&gt;"
        );
    }

    #[test]
    fn formats_uptime_and_converts_discord_dates() {
        assert_eq!(format_uptime(90_061), "1d 1h 1m 1s");
        assert_eq!(parse_discord_time("1970-01-01T07:00:00.123+07:00"), Some(0));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use crate::test_support::{app, database, dictionary, mock, store};

    #[test]
    fn public_routes_use_the_same_loaded_dictionary_and_discord_status() {
        let (app, _) = mock(database(&[("words", dictionary())]), || app(store()));
        assert_eq!(route(&app, "/health").body, r#"{"status":"ok"}"#);
        let pending = Json::parse(route(&app, "/api/status").body.as_bytes()).unwrap();
        assert_eq!(pending["status"].str(), "booting");
        assert_eq!(route(&app, "/invite").code, 503);
        {
            let mut cache = app.discord.cache.write().unwrap();
            cache.ready = true;
            cache.user = json!({"id": "1", "username": "Neko", "discriminator": "0"});
            cache.application = "123".into();
        }
        let online = Json::parse(route(&app, "/api/status").body.as_bytes()).unwrap();
        assert_eq!(online["bot"]["tag"].str(), "Neko");
        assert_eq!(online["stats"]["dictionaryWords"].int(), 3);
        assert_eq!(route(&app, "/").code, 200);
        assert_eq!(route(&app, "/privacy-policy").code, 200);
        assert_eq!(route(&app, "/terms-of-service").code, 200);
        assert_eq!(route(&app, "/invite").code, 302);
        assert_eq!(route(&app, "/api/suggest").code, 400);
        let api = route(&app, "/api/suggest?word=h%e1%bb%8dc+sinh");
        let body = Json::parse(api.body.as_bytes()).unwrap();
        assert_eq!(body["suggestions"], json!(["sinh nhật"]));
        assert!(
            route(&app, "/suggest?word=h%e1%bb%8dc+sinh")
                .body
                .contains("<li>sinh nhật</li>")
        );
    }
}
