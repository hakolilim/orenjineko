use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_int, c_long, c_void};
use std::ptr;
use std::sync::OnceLock;
use std::time::Duration;

use crate::json::Json;

#[link(name = "libcurl.so.4", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn curl_global_init(flags: c_long) -> c_int;
    fn curl_easy_init() -> *mut c_void;
    fn curl_easy_reset(handle: *mut c_void);
    fn curl_easy_cleanup(handle: *mut c_void);
    fn curl_easy_setopt(handle: *mut c_void, option: c_int, ...) -> c_int;
    fn curl_easy_getinfo(handle: *mut c_void, info: c_int, ...) -> c_int;
    fn curl_easy_perform(handle: *mut c_void) -> c_int;
    fn curl_easy_strerror(code: c_int) -> *const c_char;
    fn curl_slist_append(list: *mut c_void, value: *const c_char) -> *mut c_void;
    fn curl_slist_free_all(list: *mut c_void);
    fn curl_easy_send(handle: *mut c_void, data: *const u8, size: usize, sent: *mut usize)
    -> c_int;
    fn curl_easy_recv(
        handle: *mut c_void,
        data: *mut u8,
        size: usize,
        received: *mut usize,
    ) -> c_int;
}

// Stable libcurl options from curl/curl.h.
const WRITEDATA: c_int = 10001;
const URL: c_int = 10002;
const WRITEFUNCTION: c_int = 20011;
const POSTFIELDS: c_int = 10015;
const HTTPHEADER: c_int = 10023;
const CUSTOMREQUEST: c_int = 10036;
const FOLLOWLOCATION: c_int = 52;
const POSTFIELDSIZE: c_int = 60;
const MAXREDIRS: c_int = 68;
const HTTP_VERSION: c_int = 84;
const NOSIGNAL: c_int = 99;
const CONNECT_ONLY: c_int = 141;
const TIMEOUT_MS: c_int = 155;
const CONNECTTIMEOUT_MS: c_int = 156;
const PROTOCOLS: c_int = 181;
const REDIR_PROTOCOLS: c_int = 182;
const HTTP_PROTOCOLS: c_long = (1 << 0) | (1 << 1);
const RESPONSE_CODE: c_int = 0x200002;
const AGAIN: c_int = 81;

struct Easy(*mut c_void);

impl Easy {
    fn new() -> Result<Self, String> {
        static INIT: OnceLock<c_int> = OnceLock::new();
        // Global initialization runs exactly once, before any handle is created.
        check(*INIT.get_or_init(|| unsafe { curl_global_init(3) }))?;
        let handle = unsafe { curl_easy_init() };
        if handle.is_null() {
            return Err("Không thể khởi tạo libcurl".into());
        }
        Ok(Self(handle))
    }

    fn long(&self, option: c_int, value: c_long) -> Result<(), String> {
        check(unsafe { curl_easy_setopt(self.0, option, value) })
    }

    fn string(&self, option: c_int, value: &str) -> Result<(), String> {
        let value = CString::new(value).map_err(|_| "Tham số HTTP chứa byte NUL")?;
        // libcurl copies strings for these options.
        check(unsafe { curl_easy_setopt(self.0, option, value.as_ptr()) })
    }

    fn perform(&self) -> Result<(), String> {
        check(unsafe { curl_easy_perform(self.0) })
    }

    fn code(&self) -> Result<u16, String> {
        let mut code: c_long = 0;
        check(unsafe { curl_easy_getinfo(self.0, RESPONSE_CODE, &mut code as *mut c_long) })?;
        Ok(code as u16)
    }
}

impl Drop for Easy {
    fn drop(&mut self) {
        unsafe { curl_easy_cleanup(self.0) }
    }
}

fn check(code: c_int) -> Result<(), String> {
    if code == 0 {
        return Ok(());
    }
    let error = unsafe { CStr::from_ptr(curl_easy_strerror(code)) };
    Err(format!("HTTP {code}: {}", error.to_string_lossy()))
}

struct Headers(*mut c_void);
impl Drop for Headers {
    fn drop(&mut self) {
        unsafe { curl_slist_free_all(self.0) }
    }
}

pub struct Request<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a [String],
    pub body: Option<&'a [u8]>,
    pub timeout: Duration,
    pub redirects: bool,
}

pub struct Response {
    pub code: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn success(&self) -> bool {
        (200..300).contains(&self.code)
    }
    pub fn json(&self) -> Result<Json, String> {
        if self.body.is_empty() {
            return Ok(Json::Null);
        }
        Json::parse(&self.body)
    }
}

thread_local! {
    static HANDLE: RefCell<Option<Easy>> = const { RefCell::new(None) };
}

struct Writer<'a>(&'a mut dyn FnMut(&[u8]) -> bool);

unsafe extern "C" fn receive(
    data: *const u8,
    size: usize,
    count: usize,
    context: *mut c_void,
) -> usize {
    let Some(length) = size.checked_mul(count) else {
        return 0;
    };
    if length == 0 {
        return 0;
    }
    // Pointers are valid for this callback, registered only during curl_easy_perform.
    let bytes = unsafe { std::slice::from_raw_parts(data, length) };
    let writer = unsafe { &mut *context.cast::<Writer<'_>>() };
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (writer.0)(bytes))).unwrap_or(false)
    {
        length
    } else {
        0
    }
}

impl Request<'_> {
    pub fn send(&self) -> Result<Response, String> {
        let mut body = Vec::new();
        let code = self.stream(&mut |bytes| {
            if body.len() + bytes.len() > 32 * 1024 * 1024 {
                return false;
            }
            body.extend_from_slice(bytes);
            true
        })?;
        Ok(Response { code, body })
    }

    pub fn stream(&self, write: &mut dyn FnMut(&[u8]) -> bool) -> Result<u16, String> {
        #[cfg(test)]
        if let Some(response) = crate::test_support::try_request(self, write) {
            return response;
        }
        HANDLE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Some(Easy::new()?);
            }
            let easy = slot.as_ref().unwrap();
            // Clear any borrowed pointers before returning. The connection cache survives resets.
            let result = self.transfer(easy, write);
            unsafe {
                curl_easy_reset(easy.0);
            }
            result
        })
    }

    fn transfer(&self, easy: &Easy, write: &mut dyn FnMut(&[u8]) -> bool) -> Result<u16, String> {
        easy.string(URL, self.url)?;
        easy.long(PROTOCOLS, HTTP_PROTOCOLS)?;
        easy.string(CUSTOMREQUEST, self.method)?;
        easy.long(NOSIGNAL, 1)?;
        let timeout = self.timeout.as_millis().clamp(1, c_long::MAX as u128) as c_long;
        easy.long(TIMEOUT_MS, timeout)?;
        easy.long(CONNECTTIMEOUT_MS, timeout.min(10_000))?;
        if self.redirects {
            easy.long(FOLLOWLOCATION, 1)?;
            easy.long(MAXREDIRS, 5)?;
            easy.long(REDIR_PROTOCOLS, HTTP_PROTOCOLS)?;
        }
        let mut headers = Headers(ptr::null_mut());
        for header in self.headers {
            if header.contains(['\r', '\n']) {
                return Err("Header HTTP không hợp lệ".into());
            }
            let value = CString::new(header.as_str()).map_err(|_| "Header HTTP chứa byte NUL")?;
            let list = unsafe { curl_slist_append(headers.0, value.as_ptr()) };
            if list.is_null() {
                return Err("Không thể thêm header HTTP".into());
            }
            headers.0 = list;
        }
        let mut writer = Writer(write);
        // All pointer arguments remain alive through the synchronous transfer.
        unsafe {
            check(curl_easy_setopt(easy.0, HTTPHEADER, headers.0))?;
            check(curl_easy_setopt(
                easy.0,
                WRITEFUNCTION,
                receive as unsafe extern "C" fn(_, _, _, _) -> _,
            ))?;
            check(curl_easy_setopt(
                easy.0,
                WRITEDATA,
                &mut writer as *mut Writer<'_>,
            ))?;
            if let Some(body) = self.body {
                easy.long(POSTFIELDSIZE, body.len() as c_long)?;
                check(curl_easy_setopt(easy.0, POSTFIELDS, body.as_ptr()))?;
            }
        }
        easy.perform()?;
        easy.code()
    }
}

pub struct Socket(Easy);
impl Socket {
    pub fn connect(url: &str) -> Result<Self, String> {
        let easy = Easy::new()?;
        easy.string(URL, url)?;
        easy.long(CONNECT_ONLY, 1)?;
        easy.long(HTTP_VERSION, 2)?; // CURL_HTTP_VERSION_1_1
        easy.long(NOSIGNAL, 1)?;
        easy.long(TIMEOUT_MS, 10_000)?;
        easy.perform()?;
        Ok(Self(easy))
    }

    pub fn send(&mut self, data: &[u8]) -> Result<usize, String> {
        let mut sent = 0;
        let code = unsafe { curl_easy_send(self.0.0, data.as_ptr(), data.len(), &mut sent) };
        if code != AGAIN {
            check(code)?;
        }
        Ok(sent)
    }

    pub fn receive(&mut self, buffer: &mut [u8]) -> Result<Option<usize>, String> {
        let mut read = 0;
        let code =
            unsafe { curl_easy_recv(self.0.0, buffer.as_mut_ptr(), buffer.len(), &mut read) };
        if code == AGAIN {
            return Ok(None);
        }
        check(code)?;
        Ok(Some(read))
    }
}

pub fn encode_component(value: &str) -> String {
    use std::fmt::Write;
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

pub fn decode_component(value: &str) -> Option<String> {
    let mut result = Vec::new();
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        result.push(match byte {
            b'+' => b' ',
            b'%' => {
                ((bytes.next()? as char).to_digit(16)? * 16
                    + (bytes.next()? as char).to_digit(16)?) as u8
            }
            other => other,
        });
    }
    String::from_utf8(result).ok()
}
