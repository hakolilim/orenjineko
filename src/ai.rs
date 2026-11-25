use std::collections::HashSet;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use crate::App;
use crate::config::AiConfig;
use crate::discord::{COLOR, Interaction, clip, display_name, field};
use crate::http::Request;
use crate::json::Json;

const MAX_REPLY: usize = 1024;
const EDIT_INTERVAL: Duration = Duration::from_millis(1500);

pub fn execute(app: &App, event: &mut Interaction) -> Result<(), String> {
    let config = &app.config.ai;
    if !config.enabled {
        return event.text("Tính năng AI hiện chưa được cấu hình!", true);
    }
    let question = event.option("message").str().to_owned();
    if question.trim().is_empty() {
        return event.text("Vui lòng nhập câu hỏi.", true);
    }
    let system = event
        .option("system")
        .opt_str()
        .filter(|system| !system.is_empty())
        .unwrap_or(&config.prompt)
        .to_owned();
    let private = matches!(event.option("visible"), Json::Bool(false));
    event.defer(private)?;
    let _ = event.edit(json!({"embeds": [(answer(config, &question, "", true, 0, 1))]}));

    let context = match app.discord.rest(
        "GET",
        &format!(
            "/channels/{}/messages?limit=10&before={}",
            event.channel(),
            event.data["id"].str()
        ),
        None,
    ) {
        Ok(context) => context,
        Err(error) => {
            eprintln!("Lấy ngữ cảnh AI: {error}");
            return event
                .edit(json!({"content": "Không thể đọc lịch sử của kênh này.", "embeds": []}));
        }
    };
    let bot = app.discord.cache.read().unwrap().user["id"]
        .str()
        .to_owned();
    let mut messages = vec![json!({"role": "system", "content": system})];
    messages.extend(context_messages(context.array().to_vec(), &bot));
    messages.push(json!({"role": "user", "content": (&question)}));

    let has_images = messages
        .iter()
        .any(|message| matches!(message["content"], Json::Array(_)));
    let (mut text, mut result) = stream(config, event, &question, &messages);
    let mut warning = None;
    if has_images
        && text.trim().is_empty()
        && result.as_ref().is_err_and(AiError::retry_without_images)
    {
        for message in &mut messages {
            if let Json::Array(parts) = &message["content"] {
                message["content"] = parts
                    .iter()
                    .filter(|part| part["type"].str() == "text")
                    .map(|part| part["text"].str())
                    .collect::<Vec<_>>()
                    .join("\n")
                    .into();
            }
        }
        warning = Some(
            "Model/provider hiện tại không hỗ trợ xem ảnh; AI đã trả lời chỉ dựa trên văn bản."
                .to_owned(),
        );
        (text, result) = stream(config, event, &question, &messages);
    }
    if let Err(error) = result {
        let message = error.display(config.timeout);
        if text.trim().is_empty() {
            return event.edit(json!({"content": message, "embeds": []}));
        }
        warning = Some(message);
    }

    let parts = split_chunks(&text);
    for (index, part) in parts.iter().enumerate() {
        let mut payload =
            json!({"embeds": [(answer(config, &question, part, false, index, parts.len()))]});
        if index + 1 == parts.len()
            && let Some(warning) = &warning
        {
            payload["content"] = format!("⚠️ {warning}").into();
        }
        if index == 0 {
            event.edit(payload)?;
        } else {
            event.follow(payload)?;
        }
    }
    let result = send_preview(event, &text);
    if let Err(error) = result {
        eprintln!("Preview LaTeX: {error}");
        event.follow(json!({"content": "⚠️ Không thể tạo preview LaTeX. Bạn vẫn có thể xem LaTeX gốc trong câu trả lời."}))?;
    }
    Ok(())
}

fn send_preview(event: &Interaction, text: &str) -> Result<(), String> {
    let preview = crate::latex::preview(text)?;
    let warning = (preview.skipped > 0).then(|| format!(
        "⚠️ {} công thức không được preview (không hợp lệ, chưa được hỗ trợ hoặc vượt giới hạn). Xem LaTeX gốc trong câu trả lời.",
        preview.skipped
    ));
    if let Some(image) = preview.image {
        let mut payload = json!({"embeds": [{
            "title": "🔎 Preview LaTeX", "color": COLOR, "image": {"url": "attachment://ai-latex.png"},
            "footer": {"text": (format!("{} công thức · theo thứ tự xuất hiện, bỏ trùng", preview.rendered))}
        }]});
        if let Some(warning) = warning {
            payload["content"] = warning.into();
        }
        return event.follow_file(payload, &image);
    }
    if let Some(warning) = warning {
        event.follow(json!({"content": warning}))?;
    }
    Ok(())
}

fn image_url(value: &Json) -> Option<&str> {
    let url = value.opt_str()?;
    let (scheme, remainder) = url.split_once("://")?;
    let host = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    (matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https")
        && !host.is_empty()
        && !url.chars().any(char::is_control))
    .then_some(url)
}

fn attachment_image(attachment: &Json) -> Option<&str> {
    let image = match attachment["content_type"].opt_str() {
        Some(value) => matches!(
            value
                .split(';')
                .next()?
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "image/gif" | "image/jpeg" | "image/jpg" | "image/png" | "image/webp"
        ),
        None => {
            let name = attachment["filename"]
                .opt_str()
                .unwrap_or(attachment["url"].str());
            let basename = name.split(['?', '#']).next()?;
            matches!(
                basename.rsplit_once('.')?.1.to_ascii_lowercase().as_str(),
                "gif" | "jpeg" | "jpg" | "png" | "webp"
            )
        }
    };
    if !image {
        return None;
    }
    image_url(&attachment["url"]).or_else(|| image_url(&attachment["proxy_url"]))
}

fn message_images(message: &Json) -> impl Iterator<Item = &str> {
    message["attachments"]
        .array()
        .iter()
        .filter_map(attachment_image)
        .chain(
            message["embeds"]
                .array()
                .iter()
                .flat_map(|embed| [&embed["image"], &embed["thumbnail"]])
                .filter_map(|asset| {
                    image_url(&asset["proxy_url"]).or_else(|| image_url(&asset["url"]))
                }),
        )
}

fn context_messages(mut messages: Vec<Json>, bot: &str) -> Vec<Json> {
    messages.sort_by_key(|message| message["id"].str().parse::<u64>().unwrap_or_default());
    let mut seen = HashSet::new();
    let mut newest = Vec::new();
    for message in messages.iter().rev() {
        let author = &message["author"];
        let assistant = author["id"].str() == bot;
        let role = if assistant { "assistant" } else { "user" };
        let text = message["content"].str().trim();
        let images = if assistant || seen.len() >= 4 {
            Vec::new()
        } else {
            let remaining = 4 - seen.len();
            message_images(message)
                .filter(|url| seen.insert(*url))
                .take(remaining)
                .collect::<Vec<_>>()
        };
        if text.is_empty() && images.is_empty() {
            continue;
        }
        let name = display_name(author, &Json::Null);
        let name = if name.is_empty() {
            "Người dùng"
        } else {
            name
        };
        let text = if text.is_empty() {
            "[đã gửi hình ảnh]"
        } else {
            text
        };
        let text = format!("{name}: {text}");
        let content = if images.is_empty() {
            Json::from(text)
        } else {
            let mut parts = vec![json!({"type": "text", "text": text})];
            parts.extend(
                images
                    .into_iter()
                    .map(|url| json!({"type": "image_url", "image_url": {"url": url}})),
            );
            parts.into()
        };
        newest.push(json!({"role": role, "content": content}));
    }
    newest.reverse();
    newest
}

enum Update {
    Text(String),
    Finished(Result<(), AiError>),
}

#[derive(Debug)]
enum AiError {
    Http(u16, Json),
    Event(Json),
    Transport(String),
    Empty,
}

impl AiError {
    fn retry_without_images(&self) -> bool {
        let (status, code) = match self {
            Self::Http(status, error) => (i64::from(*status), &error["code"]),
            Self::Event(error) => (
                error["status"].int(),
                if error["code"].is_null() {
                    &error["type"]
                } else {
                    &error["code"]
                },
            ),
            Self::Empty => return true,
            Self::Transport(_) => return false,
        };
        if matches!(status, 401 | 403 | 429) || matches!(code.int(), 401 | 403 | 429) {
            return false;
        }
        let code = code.str().to_ascii_lowercase().replace(['-', '_', ' '], "");
        if ["auth", "apikey", "permission", "ratelimit", "timeout"]
            .iter()
            .any(|part| code.contains(part))
        {
            return false;
        }
        matches!(status, 400 | 415 | 422) || matches!(self, Self::Event(_))
    }

    fn display(&self, timeout: Duration) -> String {
        match self {
            Self::Http(status, error) => error["message"]
                .opt_str()
                .map(|message| format!("Lỗi từ AI: {}", clip(message, 1500)))
                .unwrap_or_else(|| format!("Lỗi {status} từ AI")),
            Self::Event(error) => format!(
                "Lỗi từ AI: {}",
                clip(
                    error["message"]
                        .opt_str()
                        .unwrap_or("luồng phản hồi bị ngắt"),
                    1500
                )
            ),
            Self::Transport(error) if error.starts_with("HTTP 28:") => {
                format!("AI không phản hồi trong {}ms", timeout.as_millis())
            }
            Self::Transport(_) => "Không thể kết nối tới AI, vui lòng thử lại sau!".into(),
            Self::Empty => "AI trả về nội dung rỗng.".into(),
        }
    }
}

fn stream(
    config: &AiConfig,
    event: &Interaction,
    question: &str,
    messages: &[Json],
) -> (String, Result<(), AiError>) {
    let (tx, rx) = channel();
    thread::scope(|scope| {
        scope.spawn(move || {
            let result = upstream(config, messages, &tx);
            let _ = tx.send(Update::Finished(result));
        });
        let mut text = String::new();
        let mut next_edit = Instant::now() + EDIT_INTERVAL;
        let mut rendered = 0;
        loop {
            if Instant::now() >= next_edit {
                if text.len() != rendered {
                    if let Err(error) = event
                        .edit(json!({"embeds": [(answer(config, question, &text, true, 0, 1))]}))
                    {
                        eprintln!("Cập nhật luồng AI: {error}");
                    }
                    rendered = text.len();
                }
                next_edit = Instant::now() + EDIT_INTERVAL;
            }
            match rx.recv_timeout(next_edit.saturating_duration_since(Instant::now())) {
                Ok(Update::Text(delta)) => text.push_str(&delta),
                Ok(Update::Finished(result)) => {
                    let result = result.and_then(|()| {
                        if text.trim().is_empty() {
                            Err(AiError::Empty)
                        } else {
                            Ok(())
                        }
                    });
                    return (text, result);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return (text, Err(AiError::Transport("Luồng AI đã đóng".into())));
                }
            }
        }
    })
}

#[derive(Default)]
struct Sse {
    received: usize,
    pending: Vec<u8>,
    raw: Vec<u8>,
    events: bool,
    done: bool,
    error: Option<Json>,
}

impl Sse {
    fn push(&mut self, bytes: &[u8], send: &Sender<Update>) -> bool {
        self.received += bytes.len();
        if self.received > 2 * 1024 * 1024 {
            return false;
        }
        if !self.events {
            self.raw.extend_from_slice(bytes);
        }
        for segment in bytes.split_inclusive(|byte| *byte == b'\n') {
            self.pending.extend_from_slice(segment);
            if segment.ends_with(b"\n") {
                self.line(send);
                self.pending.clear();
                if self.done || self.error.is_some() {
                    return false;
                }
            }
        }
        true
    }

    fn line(&mut self, send: &Sender<Update>) {
        let Ok(line) = std::str::from_utf8(&self.pending) else {
            return;
        };
        let Some(payload) = line.strip_prefix("data:") else {
            return;
        };
        self.events = true;
        self.raw.clear();
        let payload = payload.trim();
        if payload == "[DONE]" {
            self.done = true;
            return;
        }
        let Ok(data) = Json::parse(payload.as_bytes()) else {
            return;
        };
        if !data["error"].is_null() {
            self.error = Some(data["error"].clone());
            return;
        }
        let content = data["choices"][0]["delta"]["content"].str();
        if !content.is_empty() {
            let _ = send.send(Update::Text(content.into()));
        }
    }
}

fn upstream(config: &AiConfig, messages: &[Json], send: &Sender<Update>) -> Result<(), AiError> {
    let mut headers = vec![
        "Content-Type: application/json".into(),
        "Accept: text/event-stream".into(),
    ];
    if !config.key.is_empty() {
        headers.push(format!("Authorization: Bearer {}", config.key));
    }
    let payload = json!({"model": (&config.model), "messages": (Json::Array(messages.to_vec())), "max_tokens": 2048, "stream": true}).encode();
    let mut sse = Sse::default();
    let response = Request {
        method: "POST",
        url: &format!("{}/chat/completions", config.base),
        headers: &headers,
        body: Some(payload.as_bytes()),
        timeout: config.timeout,
        redirects: false,
    }
    .stream(&mut |data| sse.push(data, send));
    if !sse.pending.is_empty() && !sse.done && sse.error.is_none() {
        sse.line(send);
    }
    if let Some(error) = sse.error {
        return Err(AiError::Event(error));
    }
    if sse.done {
        return Ok(());
    }
    let code = response.map_err(AiError::Transport)?;
    if sse.events && (200..300).contains(&code) {
        return Ok(());
    }
    let data = Json::parse(&sse.raw).unwrap_or_default();
    if !(200..300).contains(&code) {
        return Err(AiError::Http(code, data["error"].clone()));
    }
    if !data["error"].is_null() {
        return Err(AiError::Event(data["error"].clone()));
    }
    let text = data["choices"][0]["message"]["content"].str();
    if !text.is_empty() {
        let _ = send.send(Update::Text(text.into()));
    }
    Ok(())
}

fn split_chunks(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = text;
    loop {
        let max = clip(rest, MAX_REPLY).len();
        if max == rest.len() {
            break;
        }
        let start = clip(rest, MAX_REPLY - 200).len();
        let window = &rest[start..max];
        let cut = ["\n\n", "\n", " "]
            .iter()
            .find_map(|separator| {
                window
                    .rfind(separator)
                    .map(|index| start + index + separator.len())
            })
            .unwrap_or(max);
        let part = rest[..cut].trim_end();
        if !part.is_empty() {
            parts.push(part);
        }
        rest = &rest[cut..];
    }
    if !rest.trim().is_empty() {
        parts.push(rest.trim_end());
    }
    parts
}

fn answer(
    config: &AiConfig,
    question: &str,
    text: &str,
    streaming: bool,
    index: usize,
    total: usize,
) -> Json {
    let mut footer = vec![format!("Model: {}", clip(&config.model, 150))];
    if total > 1 {
        footer.push(format!("phần {}/{total}", index + 1));
    }
    if streaming {
        footer.push("⏳ đang soạn...".into());
    }
    let mut fields = Vec::new();
    if index == 0 {
        fields.push(field("Bạn hỏi", clip(question, MAX_REPLY), false));
    }
    let response = if text.is_empty() {
        "*đang soạn...*".to_owned()
    } else if streaming && clip(text, MAX_REPLY).len() < text.len() {
        format!("{}\n\n*⏳ đang soạn tiếp...*", clip(text, MAX_REPLY - 40))
    } else {
        text.to_owned()
    };
    fields.push(field("AI trả lời", response, false));
    json!({
        "title": ":crystal_ball: Trí tuệ nhân tạo", "color": COLOR, "fields": fields,
        "footer": {"text": (footer.join(" · "))}, "timestamp": (crate::web::timestamp())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_context_prioritizes_new_images_and_deduplicates() {
        let images = context_messages(
            vec![
                json!({"id": "1", "author": {"id": "1", "username": "Lan"}, "attachments": [{"filename": "a.png", "url": "https://image/a.png"}]}),
                json!({"id": "2", "author": {"id": "bot", "username": "Neko"}, "content": "Xin chào", "embeds": [{"image": {"url": "https://image/hidden"}}]}),
                json!({"id": "3", "author": {"id": "3", "username": "Nam"}, "embeds": [
                    {"image": {"url": "https://image/a.png"}, "thumbnail": {"proxy_url": "https://image/b"}},
                    {"image": {"url": "https://image/c"}, "thumbnail": {"proxy_url": "https://image/d"}},
                    {"image": {"url": "https://image/e"}}
                ]}),
            ],
            "bot",
        );
        assert_eq!(images.len(), 2);
        assert_eq!(images[0]["role"].str(), "assistant");
        assert_eq!(images[1]["content"].array().len(), 5);
        assert!(
            images[1]["content"][0]["text"]
                .str()
                .contains("[đã gửi hình ảnh]")
        );
    }

    #[test]
    fn parses_sse_split_across_network_chunks() {
        let (tx, rx) = channel();
        let mut sse = Sse::default();
        assert!(sse.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel", &tx));
        assert!(!sse.push(b"lo\"}}]}\r\n\r\ndata: [DONE]\n", &tx));
        assert!(matches!(rx.try_recv(), Ok(Update::Text(text)) if text == "Hello"));
        assert!(sse.done);
    }

    #[test]
    fn reply_respects_utf16_limits_and_authentication_blocks_fallback() {
        let text = format!("{}\n\n{}", "🍊".repeat(490), "a".repeat(1500));
        let chunks = split_chunks(&text);
        assert_eq!(chunks[0], "🍊".repeat(490));
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.encode_utf16().count() <= MAX_REPLY)
        );
        assert!(AiError::Http(415, Json::Null).retry_without_images());
        assert!(!AiError::Event(json!({"type": "rate_limit"})).retry_without_images());
    }
}

#[cfg(test)]
mod provider_tests {
    use super::*;
    use crate::test_support::{Reply, mock};

    #[test]
    fn upstream_supports_providers_that_return_json_instead_of_sse() {
        let config = AiConfig {
            enabled: true,
            base: "http://localhost/v1".into(),
            key: "test-key".into(),
            model: "test".into(),
            prompt: "Test".into(),
            timeout: Duration::from_secs(2),
        };
        let reply = Reply::json(
            "POST",
            "/v1/chat/completions",
            200,
            json!({
                "choices": [{"message": {"content": "Xin chào"}}]
            }),
        );
        let (_, requests) = mock(vec![reply], || {
            let (tx, rx) = channel();
            upstream(&config, &[json!({"role": "user", "content": "Chào"})], &tx).unwrap();
            assert!(matches!(rx.try_recv(), Ok(Update::Text(text)) if text == "Xin chào"));
        });
        let request = &requests[0];
        assert!(request.body["stream"].bool());
        assert_eq!(request.body["messages"][0]["role"].str(), "user");
        assert!(
            request
                .headers
                .iter()
                .any(|header| header == "Authorization: Bearer test-key")
        );
    }
}
