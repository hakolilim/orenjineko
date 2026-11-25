use std::collections::BTreeMap;
use std::fmt::Write;
use std::ops::{Index, IndexMut};

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Json {
    #[default]
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

macro_rules! json {
    (null) => { $crate::json::Json::Null };
    ([$($item:tt),* $(,)?]) => { $crate::json::Json::Array(vec![$(json!($item)),*]) };
    ({$($key:literal : $item:tt),* $(,)?}) => { $crate::json::Json::Object(
        [$(($key.to_string(), json!($item))),*].into_iter().collect()
    ) };
    ($value:expr) => { $crate::json::Json::from($value) };
}

impl Json {
    pub fn parse(input: &[u8]) -> Result<Self, String> {
        let input = std::str::from_utf8(input).map_err(|_| "JSON không phải UTF-8")?;
        let mut parser = Parser { input, pos: 0 };
        let value = parser.value(0)?;
        parser.space();
        if parser.pos != input.len() {
            return Err("Dữ liệu thừa sau JSON".into());
        }
        Ok(value)
    }

    pub fn str(&self) -> &str {
        if let Self::Str(value) = self {
            value
        } else {
            ""
        }
    }

    pub fn opt_str(&self) -> Option<&str> {
        if let Self::Str(value) = self {
            Some(value)
        } else {
            None
        }
    }

    pub fn bool(&self) -> bool {
        matches!(self, Self::Bool(true))
    }

    pub fn int(&self) -> i64 {
        match self {
            Self::Num(value) | Self::Str(value) => value.parse().unwrap_or_default(),
            _ => 0,
        }
    }

    pub fn decimal(&self) -> Option<f64> {
        match self {
            Self::Num(value) | Self::Str(value) => value.parse().ok(),
            _ => None,
        }
    }

    pub fn array(&self) -> &[Json] {
        if let Self::Array(value) = self {
            value
        } else {
            &[]
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    pub fn encode(&self) -> String {
        let mut output = String::new();
        self.write(&mut output);
        output
    }

    fn write(&self, output: &mut String) {
        match self {
            Self::Null => output.push_str("null"),
            Self::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
            Self::Num(value) => output.push_str(value),
            Self::Str(value) => quote(output, value),
            Self::Array(values) => {
                output.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    value.write(output);
                }
                output.push(']');
            }
            Self::Object(values) => {
                output.push('{');
                for (index, (key, value)) in values.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    quote(output, key);
                    output.push(':');
                    value.write(output);
                }
                output.push('}');
            }
        }
    }
}

fn quote(output: &mut String, text: &str) {
    output.push('"');
    for character in text.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            c if c < ' ' => {
                let _ = write!(output, "\\u{:04x}", c as u32);
            }
            c => output.push(c),
        }
    }
    output.push('"');
}

impl Index<&str> for Json {
    type Output = Json;
    fn index(&self, key: &str) -> &Json {
        match self {
            Self::Object(values) => values.get(key).unwrap_or(&Self::Null),
            _ => &Self::Null,
        }
    }
}

impl IndexMut<&str> for Json {
    fn index_mut(&mut self, key: &str) -> &mut Json {
        if !matches!(self, Self::Object(_)) {
            *self = Self::Object(BTreeMap::new());
        }
        let Self::Object(values) = self else {
            unreachable!()
        };
        values.entry(key.into()).or_default()
    }
}

impl Index<usize> for Json {
    type Output = Json;
    fn index(&self, key: usize) -> &Json {
        self.array().get(key).unwrap_or(&Self::Null)
    }
}

impl From<&str> for Json {
    fn from(value: &str) -> Self {
        Self::Str(value.into())
    }
}
impl From<&String> for Json {
    fn from(value: &String) -> Self {
        Self::Str(value.clone())
    }
}
impl From<String> for Json {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}
impl From<bool> for Json {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}
impl<T: Into<Json>> From<Vec<T>> for Json {
    fn from(value: Vec<T>) -> Self {
        Self::Array(value.into_iter().map(Into::into).collect())
    }
}
impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(value: Option<T>) -> Self {
        value.map(Into::into).unwrap_or_default()
    }
}
macro_rules! integer {
    ($($kind:ty),+ $(,)?) => {$(
        impl From<$kind> for Json {
            fn from(value: $kind) -> Self { Self::Num(value.to_string()) }
        }
    )+};
}
integer!(i32, i64, u16, u32, u64, usize, u128);

struct Parser<'a> {
    input: &'a str,
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.pos).copied()
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() != Some(byte) {
            return false;
        }
        self.pos += 1;
        true
    }

    fn value(&mut self, depth: u8) -> Result<Json, String> {
        if depth > 100 {
            return Err("JSON lồng quá sâu".into());
        }
        self.space();
        match self.peek() {
            Some(b'"') => return self.string().map(Json::Str),
            Some(b'[') => {
                self.pos += 1;
                self.space();
                let mut values = Vec::new();
                if self.eat(b']') {
                    return Ok(Json::Array(values));
                }
                loop {
                    values.push(self.value(depth + 1)?);
                    self.space();
                    if self.eat(b']') {
                        return Ok(Json::Array(values));
                    }
                    if !self.eat(b',') {
                        break;
                    }
                }
            }
            Some(b'{') => {
                self.pos += 1;
                self.space();
                let mut values = BTreeMap::new();
                if self.eat(b'}') {
                    return Ok(Json::Object(values));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    self.space();
                    if !self.eat(b':') {
                        break;
                    }
                    values.insert(key, self.value(depth + 1)?);
                    self.space();
                    if self.eat(b'}') {
                        return Ok(Json::Object(values));
                    }
                    if !self.eat(b',') {
                        break;
                    }
                }
            }
            Some(b'n') if self.keyword("null") => return Ok(Json::Null),
            Some(b't') if self.keyword("true") => return Ok(Json::Bool(true)),
            Some(b'f') if self.keyword("false") => return Ok(Json::Bool(false)),
            Some(b'-' | b'0'..=b'9') => return self.number(),
            _ => {}
        }
        Err(format!("JSON không hợp lệ tại byte {}", self.pos))
    }

    fn keyword(&mut self, value: &str) -> bool {
        if !self.input[self.pos..].starts_with(value) {
            return false;
        }
        self.pos += value.len();
        true
    }

    fn digits(&mut self) -> bool {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.pos > start
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        self.eat(b'-');
        if !self.eat(b'0') && !self.digits() {
            return Err("Số JSON không hợp lệ".into());
        }
        if self.eat(b'.') && !self.digits() {
            return Err("Phần thập phân JSON không hợp lệ".into());
        }
        if self.eat(b'e') || self.eat(b'E') {
            if !self.eat(b'+') {
                self.eat(b'-');
            }
            if !self.digits() {
                return Err("Số mũ JSON không hợp lệ".into());
            }
        }
        Ok(Json::Num(self.input[start..self.pos].into()))
    }

    fn hex(&mut self) -> Result<u32, String> {
        let digits = self
            .input
            .get(self.pos..self.pos + 4)
            .ok_or("Unicode JSON không hợp lệ")?;
        if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Unicode JSON không hợp lệ".into());
        }
        self.pos += 4;
        u32::from_str_radix(digits, 16).map_err(|_| "Unicode JSON không hợp lệ".into())
    }

    fn string(&mut self) -> Result<String, String> {
        if !self.eat(b'"') {
            return Err("Thiếu chuỗi JSON".into());
        }
        let mut value = String::new();
        while let Some(character) = self.input[self.pos..].chars().next() {
            self.pos += character.len_utf8();
            match character {
                '"' => return Ok(value),
                '\\' => {
                    let next = self.peek().ok_or("Thiếu escape JSON")?;
                    self.pos += 1;
                    value.push(match next {
                        b'"' => '"',
                        b'/' => '/',
                        b'\\' => '\\',
                        b'b' => '\x08',
                        b'f' => '\x0c',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut code = self.hex()?;
                            if (0xd800..=0xdbff).contains(&code) {
                                if !self.eat(b'\\') || !self.eat(b'u') {
                                    return Err("Thiếu surrogate JSON".into());
                                }
                                let low = self.hex()?;
                                if !(0xdc00..=0xdfff).contains(&low) {
                                    return Err("Surrogate JSON không hợp lệ".into());
                                }
                                code = 0x10000 + ((code - 0xd800) << 10) + low - 0xdc00;
                            }
                            char::from_u32(code).ok_or("Unicode JSON không hợp lệ")?
                        }
                        _ => return Err("Escape JSON không hợp lệ".into()),
                    });
                }
                c if c < ' ' => return Err("Ký tự điều khiển trong chuỗi JSON".into()),
                c => value.push(c),
            }
        }
        Err("Chuỗi JSON chưa đóng".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_unicode_and_exact_numbers() {
        let source = br#"{"number":18446744073709551615,"text":"Vi\u1ec7t \ud83c\udf4a"}"#;
        let parsed = Json::parse(source).unwrap();
        assert_eq!(parsed["text"].str(), "Việt 🍊");
        assert_eq!(Json::parse(parsed.encode().as_bytes()).unwrap(), parsed);
        assert!(Json::parse(b"[1,]").is_err());
    }
}
