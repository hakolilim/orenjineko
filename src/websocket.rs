use std::fs::File;
use std::io::Read;
use std::thread;
use std::time::{Duration, Instant};

use crate::http::Socket;

const MAX_FRAME: usize = 16 * 1024 * 1024;
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

#[link(name = "libcrypto.so.3", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn SHA1(data: *const u8, length: usize, digest: *mut u8) -> *mut u8;
}

pub enum Event {
    Text(Vec<u8>),
    Close(u16),
    Pending,
}

pub struct WebSocket {
    socket: Socket,
    buffer: Vec<u8>,
    fragments: Option<Vec<u8>>,
    entropy: File,
}

impl WebSocket {
    pub fn connect(url: &str) -> Result<Self, String> {
        let url = url
            .strip_prefix("wss://")
            .ok_or("Discord Gateway phải dùng wss")?;
        let (host, path) = url.split_once('/').unwrap_or((url, ""));
        if host.is_empty() || host.contains(['@', '\r', '\n']) || path.contains(['\r', '\n']) {
            return Err("Địa chỉ Discord Gateway không hợp lệ".into());
        }
        let (path, _) = path.split_once('?').unwrap_or((path, ""));
        let socket = Socket::connect(&format!("https://{host}/"))?;
        let mut ws = Self {
            socket,
            buffer: Vec::new(),
            fragments: None,
            entropy: File::open("/dev/urandom").map_err(|error| error.to_string())?,
        };
        let mut nonce = [0; 16];
        ws.entropy
            .read_exact(&mut nonce)
            .map_err(|error| error.to_string())?;
        let key = base64(&nonce);
        let request = format!(
            "GET /{path}?v=10&encoding=json HTTP/1.1\r\nHost: {host}\r\n\
             Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n\r\n"
        );
        ws.write(request.as_bytes())?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(end) = ws
                .buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
            {
                let response = std::str::from_utf8(&ws.buffer[..end])
                    .map_err(|_| "HTTP Upgrade không hợp lệ")?;
                check_upgrade(response, &key)?;
                ws.buffer.drain(..end + 4);
                return Ok(ws);
            }
            if ws.buffer.len() > 16_384 || Instant::now() > deadline {
                return Err("Discord Gateway không hoàn tất WebSocket Upgrade".into());
            }
            if !ws.read()? {
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn write(&mut self, mut bytes: &[u8]) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !bytes.is_empty() {
            let written = self.socket.send(bytes)?;
            bytes = &bytes[written..];
            if written == 0 {
                if Instant::now() > deadline {
                    return Err("WebSocket gửi quá hạn".into());
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    }

    fn read(&mut self) -> Result<bool, String> {
        let mut buffer = [0; 16384];
        match self.socket.receive(&mut buffer)? {
            Some(0) => Err("WebSocket đã đóng".into()),
            Some(used) => {
                self.buffer.extend_from_slice(&buffer[..used]);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub fn text(&mut self, text: &str) -> Result<(), String> {
        self.send(1, text.as_bytes())
    }

    fn send(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let mut mask = [0; 4];
        self.entropy
            .read_exact(&mut mask)
            .map_err(|error| error.to_string())?;
        let bytes = encode_frame(opcode, payload, mask);
        self.write(&bytes)
    }

    pub fn receive(&mut self) -> Result<Event, String> {
        loop {
            let Some((fin, opcode, start, end)) = frame(&self.buffer)? else {
                if !self.read()? {
                    return Ok(Event::Pending);
                }
                continue;
            };
            let payload = self.buffer[start..end].to_vec();
            self.buffer.drain(..end);
            match opcode {
                1 if self.fragments.is_none() && fin => return Ok(Event::Text(payload)),
                1 if self.fragments.is_none() => self.fragments = Some(payload),
                0 => {
                    let fragments = self
                        .fragments
                        .as_mut()
                        .ok_or("WebSocket continuation không hợp lệ")?;
                    if fragments.len() + payload.len() > MAX_FRAME {
                        return Err("WebSocket message quá lớn".into());
                    }
                    fragments.extend(payload);
                    if fin {
                        return Ok(Event::Text(self.fragments.take().unwrap()));
                    }
                }
                8 => {
                    let code = payload
                        .get(..2)
                        .map(|bytes| u16::from_be_bytes(bytes.try_into().unwrap()))
                        .unwrap_or(1005);
                    let _ = self.send(8, &payload);
                    return Ok(Event::Close(code));
                }
                9 => self.send(10, &payload)?,
                10 => {}
                _ => return Err("WebSocket opcode không hỗ trợ".into()),
            }
        }
    }
}

fn check_upgrade(response: &str, key: &str) -> Result<(), String> {
    let mut lines = response.split("\r\n");
    if lines.next().unwrap_or_default().split_whitespace().nth(1) != Some("101") {
        return Err("Discord Gateway từ chối WebSocket Upgrade".into());
    }
    let hash = format!("{key}{GUID}");
    let mut digest = [0; 20];
    // SHA-1 is mandated by the WebSocket handshake. TLS authenticates the remote server.
    unsafe {
        SHA1(hash.as_ptr(), hash.len(), digest.as_mut_ptr());
    }
    let expected = base64(&digest);
    let mut accept = false;
    let mut upgrade = false;
    let mut connection = false;
    for (name, value) in lines.filter_map(|line| line.split_once(':')) {
        if name.eq_ignore_ascii_case("sec-websocket-accept") {
            accept = value.trim() == expected;
        }
        if name.eq_ignore_ascii_case("upgrade") {
            upgrade = value.trim().eq_ignore_ascii_case("websocket");
        }
        if name.eq_ignore_ascii_case("connection") {
            connection = value
                .split(',')
                .any(|item| item.trim().eq_ignore_ascii_case("upgrade"));
        }
    }
    if accept && upgrade && connection {
        Ok(())
    } else {
        Err("WebSocket Upgrade thiếu header hợp lệ".into())
    }
}

fn encode_frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut bytes = vec![0x80 | opcode];
    match payload.len() {
        size @ 0..=125 => bytes.push(0x80 | size as u8),
        size @ 126..=65535 => {
            bytes.push(0x80 | 126);
            bytes.extend_from_slice(&(size as u16).to_be_bytes());
        }
        size => {
            bytes.push(0x80 | 127);
            bytes.extend_from_slice(&(size as u64).to_be_bytes());
        }
    }
    bytes.extend_from_slice(&mask);
    bytes.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % 4]),
    );
    bytes
}

fn frame(bytes: &[u8]) -> Result<Option<(bool, u8, usize, usize)>, String> {
    if bytes.len() < 2 {
        return Ok(None);
    }
    let fin = bytes[0] & 0x80 != 0;
    let opcode = bytes[0] & 0x0f;
    if bytes[0] & 0x70 != 0 || bytes[1] & 0x80 != 0 {
        return Err("WebSocket frame không hợp lệ".into());
    }
    let (start, size) = match bytes[1] & 0x7f {
        126 => {
            let Some(size) = bytes.get(2..4) else {
                return Ok(None);
            };
            (4, u16::from_be_bytes(size.try_into().unwrap()) as u64)
        }
        127 => {
            let Some(size) = bytes.get(2..10) else {
                return Ok(None);
            };
            (10, u64::from_be_bytes(size.try_into().unwrap()))
        }
        size => (2, u64::from(size)),
    };
    if size > MAX_FRAME as u64 || (opcode >= 8 && (!fin || size > 125)) {
        return Err("WebSocket frame vượt giới hạn".into());
    }
    let end = start + size as usize;
    Ok((bytes.len() >= end).then_some((fin, opcode, start, end)))
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for block in data.chunks(3) {
        let a = block[0] as usize;
        let b = block.get(1).copied().unwrap_or_default() as usize;
        let c = block.get(2).copied().unwrap_or_default() as usize;
        result.push(ALPHABET[a >> 2] as char);
        result.push(ALPHABET[((a & 3) << 4) | (b >> 4)] as char);
        result.push(if block.len() > 1 {
            ALPHABET[((b & 15) << 2) | (c >> 6)] as char
        } else {
            '='
        });
        result.push(if block.len() > 2 {
            ALPHABET[c & 63] as char
        } else {
            '='
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_rfc_handshake_and_masks_client_frames() {
        let response = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";
        assert!(check_upgrade(response, "dGhlIHNhbXBsZSBub25jZQ==").is_ok());
        assert!(check_upgrade(response, "wrong").is_err());
        assert_eq!(
            encode_frame(1, b"Hi", [1, 2, 3, 4]),
            [0x81, 0x82, 1, 2, 3, 4, 0x49, 0x6b]
        );
        assert_eq!(frame(b"\x81\x02H").unwrap(), None);
        assert_eq!(frame(b"\x81\x02Hi").unwrap(), Some((true, 1, 2, 4)));
    }
}
