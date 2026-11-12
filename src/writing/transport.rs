//! One fixed HTTPS request. The request guard owns all asynchronous lifetimes.
use super::{
    credentials::SecretKey, stream::SseDecoder, WritingError, WritingRequest, MAX_RESPONSE_BYTES,
    REQUEST_TIMEOUT_SECS,
};
#[cfg(test)]
use super::{API_HOST, API_PATH};
use std::{
    ffi::c_void,
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::*;

const POLL: Duration = Duration::from_millis(25);
const READ_SIZE: usize = 8192;
const ERROR_LIMIT: usize = 8192;

#[derive(Default)]
struct Context {
    ready: AtomicU32,
    read: AtomicU32,
    error: AtomicU32,
    closed: AtomicBool,
}

// No allocations, locks, user callbacks, indexing or panicking operations here.
unsafe extern "system" fn callback(
    _: *mut c_void,
    context: usize,
    status: u32,
    info: *mut c_void,
    length: u32,
) {
    if context == 0 {
        return;
    }
    let ptr = context as *const Context;
    // The handle owns a strong ref until HANDLE_CLOSING, WinHTTP's final
    // notification for this handle. Every invocation also takes a temporary ref.
    Arc::increment_strong_count(ptr);
    let state = Arc::from_raw(ptr);
    match status {
        WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING => {
            drop(Arc::from_raw(ptr)); // release the handle-owned ref exactly once
            state.closed.store(true, Ordering::Release);
        }
        WINHTTP_CALLBACK_STATUS_READ_COMPLETE => {
            state.read.store(length, Ordering::Relaxed);
            state.ready.fetch_or(status, Ordering::Release);
        }
        WINHTTP_CALLBACK_STATUS_REQUEST_ERROR => {
            let error = if !info.is_null()
                && length as usize >= std::mem::size_of::<WINHTTP_ASYNC_RESULT>()
            {
                std::ptr::read_unaligned(info.cast::<WINHTTP_ASYNC_RESULT>()).dwError
            } else {
                0
            };
            state.error.store(error, Ordering::Relaxed);
            state.ready.fetch_or(status, Ordering::Release);
        }
        WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE
        | WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE => {
            state.ready.fetch_or(status, Ordering::Release);
        }
        _ => {}
    }
}

struct Handle(*mut c_void);
impl Handle {
    fn new(ptr: *mut c_void) -> Result<Self, WritingError> {
        if ptr.is_null() {
            Err(last_error())
        } else {
            Ok(Self(ptr))
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

struct Request {
    handle: Handle,
    state: Arc<Context>,
    // Stable heap allocations, retained through HANDLE_CLOSING even on unwind.
    body: Vec<u8>,
    buffer: Box<[u8; READ_SIZE]>,
    callback_installed: bool,
}
impl Request {
    unsafe fn new(handle: Handle, body: Vec<u8>) -> Result<Self, WritingError> {
        let mut owned = Self {
            handle,
            state: Arc::new(Context::default()),
            body,
            buffer: Box::new([0; READ_SIZE]),
            callback_installed: false,
        };
        let context = Arc::into_raw(Arc::clone(&owned.state));
        let context_value = context as usize;
        // Install context before the callback, including the never-sent path.
        if WinHttpSetOption(
            Some(owned.handle.0.cast_const()),
            WINHTTP_OPTION_CONTEXT_VALUE,
            Some(&context_value.to_ne_bytes()),
        )
        .is_err()
        {
            let error = last_error();
            drop(Arc::from_raw(context));
            return Err(error);
        }
        let previous = WinHttpSetStatusCallback(
            owned.handle.0,
            Some(callback),
            WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE
                | WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE
                | WINHTTP_CALLBACK_STATUS_READ_COMPLETE
                | WINHTTP_CALLBACK_STATUS_REQUEST_ERROR
                | WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING,
            0,
        );
        if previous.map(|f| f as usize) == Some(usize::MAX) {
            let error = last_error();
            drop(Arc::from_raw(context));
            return Err(error);
        }
        owned.callback_installed = true;
        Ok(owned)
    }

    fn read(
        &mut self,
        limit: usize,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> Result<usize, WritingError> {
        check(cancel, deadline)?;
        let limit = limit.min(READ_SIZE);
        unsafe {
            WinHttpReadData(
                self.handle.0,
                self.buffer.as_mut_ptr().cast(),
                limit as u32,
                null_mut(),
            )
            .map_err(|_| last_error())?;
        }
        wait(
            &self.state,
            WINHTTP_CALLBACK_STATUS_READ_COMPLETE,
            cancel,
            deadline,
        )?;
        let length = self.state.read.load(Ordering::Relaxed) as usize;
        if length > limit {
            Err(WritingError::Protocol)
        } else {
            Ok(length)
        }
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.handle.0);
        }
        // Only this guard closes the request. Prevent Handle's second close.
        self.handle.0 = null_mut();
        if self.callback_installed {
            // The network budget has ended; this is the required WinHTTP
            // lifetime barrier, not another network operation. Never detach.
            while !self.state.closed.load(Ordering::Acquire) {
                thread::sleep(POLL);
            }
        }
        // body/buffer and the parent's Arc drop only after the final callback.
    }
}

fn network_error(code: u32) -> WritingError {
    if code == 12002 {
        WritingError::Timeout
    } else {
        WritingError::Network
    }
}
fn last_error() -> WritingError {
    network_error(unsafe { windows::Win32::Foundation::GetLastError() }.0)
}
fn check(cancel: &AtomicBool, deadline: Instant) -> Result<(), WritingError> {
    if cancel.load(Ordering::Acquire) {
        Err(WritingError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(WritingError::Timeout)
    } else {
        Ok(())
    }
}
fn wait(
    state: &Context,
    expected: u32,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<(), WritingError> {
    loop {
        check(cancel, deadline)?;
        let ready = state.ready.load(Ordering::Acquire);
        if ready & WINHTTP_CALLBACK_STATUS_REQUEST_ERROR != 0 {
            return Err(network_error(state.error.load(Ordering::Relaxed)));
        }
        if ready & expected != 0 {
            state.ready.fetch_and(!expected, Ordering::AcqRel);
            return Ok(());
        }
        thread::sleep(POLL);
    }
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
unsafe fn option(handle: *mut c_void, name: u32, value: u32) -> Result<(), WritingError> {
    WinHttpSetOption(Some(handle.cast_const()), name, Some(&value.to_ne_bytes()))
        .map_err(|_| last_error())
}

// Deserialize only code. Provider message fields are skipped, never retained
// in the parsed structure or propagated through an error.
#[derive(serde::Deserialize)]
struct ErrorEnvelope {
    error: ErrorCode,
}
#[derive(serde::Deserialize)]
struct ErrorCode {
    code: serde_json::Value,
}
fn classify_error(status: u32, body: &[u8]) -> WritingError {
    let fallback = WritingError::from_http_status(status);
    if body.len() > ERROR_LIMIT {
        return fallback;
    }
    let Ok(envelope) = serde_json::from_slice::<ErrorEnvelope>(body) else {
        return fallback;
    };
    let code = match envelope.error.code {
        serde_json::Value::String(code) => code,
        serde_json::Value::Number(number) => number.to_string(),
        _ => return fallback,
    };
    WritingError::from_provider_code(&code).unwrap_or(fallback)
}

fn classify_coding_error(status: u32, body: &[u8]) -> WritingError {
    let error = classify_error(status, body);
    if !matches!(
        error,
        WritingError::HttpRequest(_) | WritingError::HttpService(_) | WritingError::Protocol
    ) {
        return error;
    }
    let numeric = if body.len() <= ERROR_LIMIT {
        serde_json::from_slice::<ErrorEnvelope>(body)
            .ok()
            .and_then(|envelope| {
                let value = match envelope.error.code {
                    serde_json::Value::String(value) => value,
                    serde_json::Value::Number(value) => value.to_string(),
                    _ => return None,
                };
                if value.is_empty() || value.len() > 9 || !value.bytes().all(|b| b.is_ascii_digit())
                {
                    return None;
                }
                value.parse::<u32>().ok()
            })
    } else {
        None
    };
    match numeric {
        Some(code) => WritingError::CodingPlanCode { status, code },
        None => WritingError::CodingPlanHttp(status),
    }
}

fn response_error(
    owned: &mut Request,
    status: u32,
    cancel: &AtomicBool,
    deadline: Instant,
    bigmodel: bool,
    coding_plan: bool,
) -> WritingError {
    let fallback = if coding_plan {
        classify_coding_error(status, &[])
    } else {
        WritingError::from_http_status(status)
    };
    if !bigmodel {
        return fallback;
    }
    let mut body = Vec::with_capacity(ERROR_LIMIT);
    while body.len() < ERROR_LIMIT {
        let length = match owned.read(ERROR_LIMIT - body.len(), cancel, deadline) {
            Ok(length) => length,
            Err(_) if cancel.load(Ordering::Acquire) => return WritingError::Cancelled,
            Err(_) => return fallback,
        };
        if length == 0 {
            break;
        }
        body.extend_from_slice(&owned.buffer[..length]);
    }
    if cancel.load(Ordering::Acquire) {
        WritingError::Cancelled
    } else {
        let error = if coding_plan {
            classify_coding_error(status, &body)
        } else {
            classify_error(status, &body)
        };
        let seconds = unsafe { retry_after(owned.handle.0) };
        with_retry_after(error, seconds)
    }
}

fn with_retry_after(error: WritingError, seconds: u32) -> WritingError {
    match error {
        WritingError::AccountRateLimited(_) => WritingError::AccountRateLimited(seconds),
        WritingError::ServiceBusy(_) => WritingError::ServiceBusy(seconds),
        other => other,
    }
}

fn parse_retry_after(value: &[u16]) -> u32 {
    if value.is_empty() || !value.iter().all(|c| (48..=57).contains(c)) {
        return 0;
    }
    // Never turn an oversized numeric delay into "unknown" and retry early.
    if value.len() > 16 {
        return u32::MAX;
    }
    let mut seconds = 0u32;
    for c in value {
        seconds = seconds.saturating_mul(10).saturating_add((*c - 48) as u32);
    }
    seconds
}

unsafe fn retry_after(handle: *mut c_void) -> u32 {
    let mut buffer = [0u16; 17];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    if WinHttpQueryHeaders(
        handle,
        WINHTTP_QUERY_CUSTOM,
        w!("Retry-After"),
        Some(buffer.as_mut_ptr().cast()),
        &mut size,
        null_mut(),
    )
    .is_err()
    {
        // Keep the fixed allocation bound. If the header cannot fit, its
        // numeric delay might exceed the total budget; conservatively stop.
        return if size > std::mem::size_of_val(&buffer) as u32 {
            u32::MAX
        } else {
            0
        };
    }
    let Some(end) = buffer.iter().position(|c| *c == 0) else {
        return u32::MAX;
    };
    parse_retry_after(&buffer[..end])
}

fn is_event_stream(value: &str) -> bool {
    value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("text/event-stream")
}
unsafe fn validate_content_type(handle: *mut c_void) -> Result<(), WritingError> {
    validate_media_type(handle, is_event_stream)
}

fn is_json(value: &str) -> bool {
    value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("application/json")
}

unsafe fn validate_media_type(
    handle: *mut c_void,
    accepts: fn(&str) -> bool,
) -> Result<(), WritingError> {
    // A fixed buffer prevents an untrusted header from causing allocation growth.
    let mut buffer = [0u16; 256];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    WinHttpQueryHeaders(
        handle,
        WINHTTP_QUERY_CONTENT_TYPE,
        PCWSTR::null(),
        Some(buffer.as_mut_ptr().cast()),
        &mut size,
        null_mut(),
    )
    .map_err(|_| WritingError::Protocol)?;
    let length = buffer
        .iter()
        .position(|c| *c == 0)
        .ok_or(WritingError::Protocol)?;
    let value = String::from_utf16(&buffer[..length]).map_err(|_| WritingError::Protocol)?;
    if accepts(&value) {
        Ok(())
    } else {
        Err(WritingError::Protocol)
    }
}

/// Explicit discovery only: no inference, model substitution, redirects or fallback.
pub fn fetch_models_for(
    profile: &super::config::ApiProfile,
    key: &SecretKey,
    cancel: &AtomicBool,
) -> Result<Vec<String>, WritingError> {
    let deadline = Instant::now() + Duration::from_secs(30);
    check(cancel, deadline)?;
    let profile = profile.connection_normalized()?;
    let endpoint = profile.catalog_endpoint()?;
    unsafe {
        let session = Handle::new(WinHttpOpen(
            w!("Flower/OnlineWriting"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            WINHTTP_FLAG_ASYNC,
        ))?;
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 30_000, 30_000).map_err(|_| last_error())?;
        let host = wide(&endpoint.host);
        let connection = Handle::new(WinHttpConnect(
            session.0,
            PCWSTR(host.as_ptr()),
            endpoint.port,
            0,
        ))?;
        let path = wide(&endpoint.path);
        let handle = Handle::new(WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        option(
            handle.0,
            WINHTTP_OPTION_REDIRECT_POLICY,
            WINHTTP_OPTION_REDIRECT_POLICY_NEVER,
        )?;
        option(
            handle.0,
            WINHTTP_OPTION_AUTOLOGON_POLICY,
            WINHTTP_AUTOLOGON_SECURITY_LEVEL_HIGH,
        )?;
        option(
            handle.0,
            WINHTTP_OPTION_DISABLE_FEATURE,
            WINHTTP_DISABLE_AUTHENTICATION | WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_REDIRECTS,
        )?;
        let mut owned = Request::new(handle, Vec::new())?;
        let headers = catalog_headers(key);
        check(cancel, deadline)?;
        WinHttpSendRequest(
            owned.handle.0,
            Some(&headers),
            None,
            0,
            0,
            Arc::as_ptr(&owned.state) as usize,
        )
        .map_err(|_| last_error())?;
        wait(
            &owned.state,
            WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE,
            cancel,
            deadline,
        )?;
        WinHttpReceiveResponse(owned.handle.0, null_mut()).map_err(|_| last_error())?;
        wait(
            &owned.state,
            WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE,
            cancel,
            deadline,
        )?;
        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            owned.handle.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut size,
            null_mut(),
        )
        .map_err(|_| last_error())?;
        if status != 200 {
            return Err(response_error(
                &mut owned,
                status,
                cancel,
                deadline,
                profile.is_bigmodel_api(),
                profile.is_coding_plan(),
            ));
        }
        validate_media_type(owned.handle.0, is_json)?;
        read_catalog(cancel, deadline, || {
            let length = owned.read(READ_SIZE, cancel, deadline)?;
            Ok(owned.buffer[..length].to_vec())
        })
    }
}

fn catalog_headers(key: &SecretKey) -> Vec<u16> {
    format!(
        "Accept: application/json\r\nAuthorization: Bearer {}\r\n",
        key.expose()
    )
    .encode_utf16()
    .collect()
}

fn read_catalog(
    cancel: &AtomicBool,
    deadline: Instant,
    mut read: impl FnMut() -> Result<Vec<u8>, WritingError>,
) -> Result<Vec<String>, WritingError> {
    let mut body = Vec::new();
    loop {
        check(cancel, deadline)?;
        let chunk = read()?;
        check(cancel, deadline)?;
        if chunk.is_empty() {
            let result = parse_catalog(&body)?;
            check(cancel, deadline)?;
            return Ok(result);
        }
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
            return Err(WritingError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
}

fn parse_catalog(body: &[u8]) -> Result<Vec<String>, WritingError> {
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(WritingError::TooLarge);
    }
    #[derive(serde::Deserialize)]
    struct Catalog {
        data: Vec<Model>,
    }
    #[derive(serde::Deserialize)]
    struct Model {
        id: String,
    }
    let catalog: Catalog = serde_json::from_slice(body).map_err(|_| WritingError::Protocol)?;
    let mut seen = std::collections::HashSet::new();
    let mut ids = Vec::new();
    for model in catalog.data {
        if !super::config::valid_model_id(&model.id) {
            return Err(WritingError::Protocol);
        }
        if seen.insert(model.id.clone()) {
            if ids.len() == 512 {
                return Err(WritingError::TooLarge);
            }
            ids.push(model.id);
        }
    }
    if ids.is_empty() {
        Err(WritingError::Protocol)
    } else {
        Ok(ids)
    }
}

pub fn generate(
    request: &WritingRequest,
    key: &SecretKey,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(String),
) -> Result<String, WritingError> {
    let deadline = Instant::now() + Duration::from_secs(REQUEST_TIMEOUT_SECS);
    generate_until(request, key, cancel, emit, deadline)
}

pub fn generate_until(
    request: &WritingRequest,
    key: &SecretKey,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(String),
    deadline: Instant,
) -> Result<String, WritingError> {
    generate_until_for(
        request,
        &super::config::ApiProfile::default(),
        key,
        cancel,
        emit,
        deadline,
    )
}

pub fn generate_until_for(
    request: &WritingRequest,
    profile: &super::config::ApiProfile,
    key: &SecretKey,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(String),
    deadline: Instant,
) -> Result<String, WritingError> {
    check(cancel, deadline)?;
    let profile = profile.normalized()?;
    let endpoint = profile.endpoint()?;
    let body = request.body_for(&profile)?;
    unsafe {
        let session = Handle::new(WinHttpOpen(
            w!("Flower/OnlineWriting"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            WINHTTP_FLAG_ASYNC,
        ))?;
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 30_000, 30_000).map_err(|_| last_error())?;
        let host = wide(&endpoint.host);
        let connection = Handle::new(WinHttpConnect(
            session.0,
            PCWSTR(host.as_ptr()),
            endpoint.port,
            0,
        ))?;
        let path = wide(&endpoint.path);
        let handle = Handle::new(WinHttpOpenRequest(
            connection.0,
            w!("POST"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        option(
            handle.0,
            WINHTTP_OPTION_REDIRECT_POLICY,
            WINHTTP_OPTION_REDIRECT_POLICY_NEVER,
        )?;
        option(
            handle.0,
            WINHTTP_OPTION_AUTOLOGON_POLICY,
            WINHTTP_AUTOLOGON_SECURITY_LEVEL_HIGH,
        )?;
        option(
            handle.0,
            WINHTTP_OPTION_DISABLE_FEATURE,
            WINHTTP_DISABLE_AUTHENTICATION | WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_REDIRECTS,
        )?;

        let mut owned = Request::new(handle, body)?;
        let context_value = Arc::as_ptr(&owned.state) as usize;
        check(cancel, deadline)?;
        let headers: Vec<u16> = format!(
            "Content-Type: application/json\r\nAccept: text/event-stream\r\nAuthorization: Bearer {}\r\n",
            key.expose()
        ).encode_utf16().collect();
        WinHttpSendRequest(
            owned.handle.0,
            Some(&headers),
            Some(owned.body.as_ptr().cast()),
            owned.body.len() as u32,
            owned.body.len() as u32,
            context_value,
        )
        .map_err(|_| last_error())?;
        wait(
            &owned.state,
            WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE,
            cancel,
            deadline,
        )?;
        WinHttpReceiveResponse(owned.handle.0, null_mut()).map_err(|_| last_error())?;
        wait(
            &owned.state,
            WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE,
            cancel,
            deadline,
        )?;
        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            owned.handle.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut size,
            null_mut(),
        )
        .map_err(|_| last_error())?;
        if status != 200 {
            return Err(response_error(
                &mut owned,
                status,
                cancel,
                deadline,
                profile.is_bigmodel_api(),
                profile.is_coding_plan(),
            ));
        }
        validate_content_type(owned.handle.0)?;
        let mut decoder = SseDecoder::with_protocol(profile.protocol, profile.is_bigmodel_api());
        let mut total = 0usize;
        loop {
            let length = owned.read(READ_SIZE, cancel, deadline)?;
            if length == 0 {
                return decoder.finish();
            }
            total += length;
            if total > MAX_RESPONSE_BYTES {
                return Err(WritingError::TooLarge);
            }
            // Deliver completed events before parsing later events in this read.
            // Otherwise a following error could discard decoded content and
            // incorrectly make this attempt appear safe to resend.
            for line in owned.buffer[..length].split_inclusive(|b| *b == b'\n') {
                for delta in decoder.push(line)? {
                    check(cancel, deadline)?;
                    emit(delta);
                }
            }
            if decoder.is_done() {
                return decoder.finish();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_plan_unknown_errors_only_expose_bounded_numeric_codes() {
        for body in [
            r#"{"error":{"code":"987654","message":"PRIVATE INPUT sk-secret"}} "#,
            r#"{"error":{"code":987654,"message":"PRIVATE INPUT sk-secret"}} "#,
        ] {
            let error = classify_coding_error(500, body.as_bytes());
            assert_eq!(
                error,
                WritingError::CodingPlanCode {
                    status: 500,
                    code: 987654
                }
            );
            let text = error.to_string();
            assert!(text.contains("987654") && text.contains("500"));
            assert!(!text.contains("PRIVATE") && !text.contains("sk-secret"));
        }
        for body in [
            r#"{"error":{"code":"sk-secret","message":"PRIVATE"}}"#,
            r#"{"error":{"code":"1234567890"}}"#,
            r#"{"error":{"code":-1}}"#,
            r#"{"error":{"code":1.5}}"#,
            r#"{"error":{"code":"123\n"}}"#,
            r#"{"error":{"message":"PRIVATE"}}"#,
            r#"{"error":{"code":"987654"}}}"#,
        ] {
            assert_eq!(
                classify_coding_error(500, body.as_bytes()),
                WritingError::CodingPlanHttp(500)
            );
        }
        assert_eq!(
            classify_coding_error(500, &vec![b' '; ERROR_LIMIT + 1]),
            WritingError::CodingPlanHttp(500)
        );
        for status in [401, 403, 402, 408, 429, 504] {
            assert_eq!(
                classify_coding_error(status, b"bad"),
                WritingError::from_http_status(status)
            );
        }
        assert_eq!(
            classify_coding_error(500, br#"{"error":{"code":"1305","message":"PRIVATE"}}"#),
            WritingError::ServiceBusy(0)
        );
        assert_eq!(
            classify_error(500, br#"{"error":{"code":"987654"}}"#),
            WritingError::HttpService(500)
        );
    }

    #[test]
    fn catalog_parser_is_bounded_strict_and_order_preserving() {
        assert_eq!(
            parse_catalog(br#"{"data":[{"id":"b"},{"id":"a"},{"id":"b"}]}"#).unwrap(),
            vec!["b", "a"]
        );
        for body in [
            r#"{"data":[]}"#,
            "{}",
            r#"{"data":null}"#,
            r#"{"data":[{}]}"#,
            r#"{"data":[{"id":3}]}"#,
            r#"{"data":[{"id":"bad id"}]}"#,
            r#"{"data":[{"id":""}]}"#,
            r#"{"error":{"message":"PRIVATE","code":"1211"}}"#,
        ] {
            let error = parse_catalog(body.as_bytes()).unwrap_err();
            assert_eq!(error, WritingError::Protocol);
            assert!(!error.to_string().contains("PRIVATE"));
        }
        let models: Vec<_> = (0..512)
            .map(|n| serde_json::json!({"id":format!("model-{n}")}))
            .collect();
        let mut value = serde_json::json!({"data":models});
        assert_eq!(
            parse_catalog(&serde_json::to_vec(&value).unwrap())
                .unwrap()
                .len(),
            512
        );
        value["data"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"id":"extra"}));
        assert_eq!(
            parse_catalog(&serde_json::to_vec(&value).unwrap()),
            Err(WritingError::TooLarge)
        );
        assert_eq!(
            parse_catalog(&vec![b' '; MAX_RESPONSE_BYTES + 1]),
            Err(WritingError::TooLarge)
        );
    }

    #[test]
    fn catalog_fake_fragment_reads_enforce_cancel_deadline_and_byte_cap() {
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(2);
        let data = br#"{"data":[{"id":"deepseek-chat"}]}"#;
        let mut chunks = data.chunks(3).map(|s| s.to_vec());
        assert_eq!(
            read_catalog(&cancel, deadline, || Ok(chunks.next().unwrap_or_default())).unwrap(),
            vec!["deepseek-chat"]
        );
        assert_eq!(
            read_catalog(&cancel, deadline, || Ok(vec![b' '; MAX_RESPONSE_BYTES + 1])),
            Err(WritingError::TooLarge)
        );
        assert_eq!(
            read_catalog(&cancel, Instant::now(), || panic!("no read")),
            Err(WritingError::Timeout)
        );
        assert_eq!(
            read_catalog(&cancel, deadline, || {
                cancel.store(true, Ordering::Release);
                Ok(data.to_vec())
            }),
            Err(WritingError::Cancelled)
        );
        assert_eq!(
            read_catalog(&cancel, deadline, || panic!("no read")),
            Err(WritingError::Cancelled)
        );
    }

    #[test]
    fn catalog_target_headers_and_early_cancel_need_no_model_or_network() {
        let p = super::super::config::ApiProfile {
            base_url: "https://api.deepseek.com".into(),
            model: String::new(),
            ..Default::default()
        };
        let p = p.connection_normalized().unwrap();
        assert_eq!(p.catalog_endpoint().unwrap().path, "/models");
        assert!(p.normalized().is_err());
        let key = SecretKey::new("synthetic-key").unwrap();
        let headers = String::from_utf16(&catalog_headers(&key)).unwrap();
        assert_eq!(
            headers,
            "Accept: application/json\r\nAuthorization: Bearer synthetic-key\r\n"
        );
        assert!(is_json("Application/JSON; charset=utf-8"));
        for value in ["text/html", "text/event-stream", "application/jsonp", ""] {
            assert!(!is_json(value));
        }
        assert_eq!(
            fetch_models_for(&p, &key, &AtomicBool::new(true)),
            Err(WritingError::Cancelled)
        );
    }

    #[test]
    fn deepseek_synthetic_standard_sse_decodes_fragmented_utf8_without_relaxing_terminal() {
        let sse = concat!(
            "data: {\"id\":\"synthetic\",\"object\":\"chat.completion.chunk\",\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"你好\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n"
        );
        for size in [1, 7, 8192] {
            let mut decoder = SseDecoder::with_protocol(
                super::super::config::ApiProtocol::ChatCompletions,
                false,
            );
            let mut text = String::new();
            for chunk in sse.as_bytes().chunks(size) {
                for delta in decoder.push(chunk).unwrap() {
                    text.push_str(&delta);
                }
            }
            assert_eq!(text, "你好");
            assert_eq!(decoder.finish().unwrap(), text);
        }
        let mut decoder =
            SseDecoder::with_protocol(super::super::config::ApiProtocol::ChatCompletions, false);
        decoder
            .push(sse.split("data: [DONE]").next().unwrap().as_bytes())
            .unwrap();
        assert_eq!(decoder.finish(), Err(WritingError::Incomplete));
    }

    #[test]
    fn retry_after_is_bounded_seconds_not_dates_or_truncated_numbers() {
        for (text, expected) in [
            ("0", 0),
            ("120", 120),
            ("3600", 3600),
            ("3601", 3601),
            ("7200", 7200),
            ("4294967295", u32::MAX),
            ("4294967296", u32::MAX),
            ("9999999999999999", u32::MAX),
            ("12345678901234567", u32::MAX),
            ("Wed, 21 Oct", 0),
            ("-1", 0),
            ("1.5", 0),
            ("", 0),
        ] {
            assert_eq!(
                parse_retry_after(&text.encode_utf16().collect::<Vec<_>>()),
                expected
            );
        }
    }

    #[test]
    fn retry_after_over_budget_codes_stop_after_exactly_one_attempt() {
        for text in [
            "3601",
            "7200",
            "4294967295",
            "4294967296",
            "9999999999999999",
            "12345678901234567",
        ] {
            for code in ["1302", "1305"] {
                let seconds = parse_retry_after(&text.encode_utf16().collect::<Vec<_>>());
                let error =
                    with_retry_after(WritingError::from_provider_code(code).unwrap(), seconds);
                assert!(
                    matches!(error, WritingError::AccountRateLimited(s) | WritingError::ServiceBusy(s) if s >= 3601)
                );
                let start = Instant::now();
                let mut attempts = 0;
                let result = super::super::retry::run(
                    true,
                    &AtomicBool::new(false),
                    start + Duration::from_secs(90),
                    |_| {
                        attempts += 1;
                        Err(error)
                    },
                    &mut |_| panic!("no content"),
                    |_, _| panic!("no waiting"),
                    super::super::retry::Runtime {
                        now: || start,
                        sleep: |_| panic!("must not sleep"),
                        jitter: 0,
                    },
                );
                assert_eq!(result, Err(WritingError::Timeout), "{text}/{code}");
                assert_eq!(attempts, 1, "{text}/{code}");
            }
        }
    }

    #[test]
    fn real_winhttp_unsent_request_closes_callback_context() {
        unsafe {
            for catalog in [false, true] {
                // Connect/OpenRequest only create handles. No SendRequest is called,
                // so this does not perform DNS, HTTPS or access any account.
                let session = Handle::new(WinHttpOpen(
                    w!("Flower/OnlineWriting/LifetimeTest"),
                    WINHTTP_ACCESS_TYPE_NO_PROXY,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    WINHTTP_FLAG_ASYNC,
                ))
                .unwrap();
                let host = wide(API_HOST);
                let connection =
                    Handle::new(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0)).unwrap();
                let path = wide(if catalog { "/models" } else { API_PATH });
                let handle = Handle::new(WinHttpOpenRequest(
                    connection.0,
                    if catalog { w!("GET") } else { w!("POST") },
                    PCWSTR(path.as_ptr()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    null(),
                    WINHTTP_FLAG_SECURE,
                ))
                .unwrap();
                let mut request =
                    Request::new(handle, if catalog { Vec::new() } else { vec![0; 32] }).unwrap();
                if catalog {
                    assert!(request.body.is_empty());
                    // A non-BigModel failure is classified by HTTP alone, without
                    // reading an error body or sending this unsent request.
                    assert_eq!(
                        response_error(
                            &mut request,
                            404,
                            &AtomicBool::new(false),
                            Instant::now() + Duration::from_secs(1),
                            false,
                            false
                        ),
                        WritingError::HttpRequest(404)
                    );
                }
                let state = Arc::clone(&request.state);
                let weak = Arc::downgrade(&state);
                assert_eq!(Arc::strong_count(&state), 3);
                drop(request); // production guard closes and waits for HANDLE_CLOSING
                assert!(state.closed.load(Ordering::Acquire));
                drop(state);
                // The closing callback may still be returning after publishing ack.
                let deadline = Instant::now() + Duration::from_secs(2);
                while weak.strong_count() != 0 && Instant::now() < deadline {
                    thread::sleep(POLL);
                }
                assert_eq!(weak.strong_count(), 0);
            }
        }
    }

    #[test]
    fn provider_codes_override_status_without_retaining_messages() {
        for (status, code, expected) in [
            (429, 1113, WritingError::Quota),
            (400, 1000, WritingError::Authentication),
            (400, 1001, WritingError::Authentication),
            (400, 1003, WritingError::Authentication),
            (400, 1005, WritingError::Authentication),
            (400, 1220, WritingError::Authentication),
            (429, 1302, WritingError::AccountRateLimited(0)),
            (429, 1305, WritingError::ServiceBusy(0)),
            (400, 1308, WritingError::UsageLimit),
            (400, 1310, WritingError::UsageLimit),
            (400, 1316, WritingError::UsageLimit),
            (400, 1321, WritingError::UsageLimit),
            (400, 1309, WritingError::WrongKeyType),
            (400, 1311, WritingError::WrongKeyType),
            (400, 1314, WritingError::WrongKeyType),
            (400, 1315, WritingError::WrongKeyType),
            (400, 1301, WritingError::Rejected),
            (400, 1211, WritingError::InvalidModel),
            (400, 1221, WritingError::Service),
            (400, 1222, WritingError::Service),
        ] {
            for value in [format!("{code}"), format!("\"{code}\"")] {
                let body = format!(r#"{{"error":{{"code":{value},"message":"PRIVATE"}}}}"#);
                let error = classify_error(status, body.as_bytes());
                assert_eq!(error, expected);
                assert!(!error.to_string().contains("PRIVATE"));
            }
        }
        for body in [
            b"invalid".as_slice(),
            br#"{"error":{"code":"unknown"}}"#,
            br#"{"error":{"message":"PRIVATE"}}"#,
        ] {
            assert_eq!(classify_error(429, body), WritingError::RateLimited);
            assert_eq!(classify_error(400, body), WritingError::HttpRequest(400));
        }
        assert_eq!(
            classify_error(429, &vec![b' '; ERROR_LIMIT + 1]),
            WritingError::RateLimited
        );
    }

    #[test]
    fn content_type_and_done_do_not_require_connection_eof() {
        assert!(is_event_stream("Text/Event-Stream; charset=utf-8"));
        for invalid in ["application/json", "text/event-streaming", ""] {
            assert!(!is_event_stream(invalid));
        }
        let mut decoder = SseDecoder::new();
        let chunks = decoder.push(
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        ).unwrap();
        assert_eq!(chunks, vec!["ok"]);
        assert!(decoder.is_done());
        assert_eq!(decoder.finish().unwrap(), "ok");
    }

    #[test]
    fn callback_notifications_are_bounded_and_close_releases_handle_reference() {
        let state = Arc::new(Context::default());
        let weak = Arc::downgrade(&state);
        let ptr = Arc::into_raw(Arc::clone(&state));
        assert_eq!(Arc::strong_count(&state), 2);
        unsafe {
            callback(
                null_mut(),
                ptr as usize,
                WINHTTP_CALLBACK_STATUS_READ_COMPLETE,
                null_mut(),
                8192,
            );
        }
        assert_eq!(Arc::strong_count(&state), 2);
        assert_eq!(
            state.ready.swap(0, Ordering::Acquire),
            WINHTTP_CALLBACK_STATUS_READ_COMPLETE
        );
        assert_eq!(state.read.load(Ordering::Relaxed), 8192);
        unsafe {
            callback(
                null_mut(),
                ptr as usize,
                WINHTTP_CALLBACK_STATUS_READ_COMPLETE,
                null_mut(),
                0,
            );
        }
        assert_eq!(
            state.ready.swap(0, Ordering::Acquire),
            WINHTTP_CALLBACK_STATUS_READ_COMPLETE
        );
        assert_eq!(state.read.load(Ordering::Relaxed), 0);
        let mut error = WINHTTP_ASYNC_RESULT {
            dwResult: 0,
            dwError: 12002,
        };
        unsafe {
            callback(
                null_mut(),
                ptr as usize,
                WINHTTP_CALLBACK_STATUS_REQUEST_ERROR,
                (&mut error as *mut WINHTTP_ASYNC_RESULT).cast(),
                std::mem::size_of::<WINHTTP_ASYNC_RESULT>() as u32,
            );
        }
        assert_eq!(
            wait(
                &state,
                WINHTTP_CALLBACK_STATUS_READ_COMPLETE,
                &AtomicBool::new(false),
                Instant::now() + Duration::from_secs(1)
            ),
            Err(WritingError::Timeout)
        );
        unsafe {
            callback(
                null_mut(),
                ptr as usize,
                WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING,
                null_mut(),
                0,
            );
        }
        assert!(state.closed.load(Ordering::Acquire));
        assert_eq!(Arc::strong_count(&state), 1);
        drop(state);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn fixed_error_mapping_and_cancel_precedence() {
        assert_eq!(network_error(12002), WritingError::Timeout);
        assert_eq!(network_error(12029), WritingError::Network);
        let cancel = AtomicBool::new(true);
        assert_eq!(check(&cancel, Instant::now()), Err(WritingError::Cancelled));
        cancel.store(false, Ordering::Release);
        assert_eq!(check(&cancel, Instant::now()), Err(WritingError::Timeout));
    }
}
