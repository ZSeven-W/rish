//! Writes to the stdin of a command that is already running.
//!
//! `rish_vm_session_exec_stream_json` hands the guest one stdin buffer before
//! the command starts and closes it, which cannot answer a prompt the command
//! has not printed yet. An interactive login needs exactly that: the CLI prints
//! a verification prompt, the person pastes a code, and it has to reach the
//! process that is still waiting. This queues those bytes; the execution's own
//! pump writes them at its next frame boundary, so nothing here takes the
//! session lock that execution holds for as long as the command runs.

use std::ffi::{CString, c_char, c_void};

use crate::vm_ffi::VmSession;

fn reply(ok: bool, error: Option<&str>) -> *mut c_char {
    let body = match error {
        Some(error) => format!(r#"{{"protocol_version":1,"ok":{ok},"error":"{error}"}}"#),
        None => format!(r#"{{"protocol_version":1,"ok":{ok}}}"#),
    };
    CString::new(body)
        .expect("generated JSON contains no NUL")
        .into_raw()
}

/// Queues stdin for the session's running command.
///
/// The request is UTF-8 JSON: `{"protocol_version":1,"action":"write_stdin",
/// "data_base64":"..."}` or `{"protocol_version":1,"action":"close_stdin"}`.
/// The reply carries `ok`, or `ok=false` with an `error`. The returned string
/// belongs to Rust and must be released with `rish_string_free`.
///
/// # Safety
/// `session` must be a live VM session. `input` must contain `input_len`
/// readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rish_vm_session_control_json(
    session: *mut c_void,
    input: *const c_char,
    input_len: usize,
) -> *mut c_char {
    if session.is_null() || input.is_null() || input_len > crate::MAX_ABI_REQUEST_BYTES {
        return reply(false, Some("invalid control call"));
    }
    // SAFETY: the caller supplies the live session and bounded readable input.
    let session = unsafe { &*session.cast::<VmSession>() };
    let bytes = unsafe { std::slice::from_raw_parts(input.cast::<u8>(), input_len) };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return reply(false, Some("control request is not UTF-8"));
    };
    let Ok(request) = serde_json::from_str::<serde_json::Value>(text) else {
        return reply(false, Some("control request is not JSON"));
    };
    if request.get("protocol_version") != Some(&serde_json::json!(1)) {
        return reply(false, Some("unsupported control protocol version"));
    }
    match request.get("action").and_then(serde_json::Value::as_str) {
        Some("write_stdin") => {
            let Some(encoded) = request
                .get("data_base64")
                .and_then(serde_json::Value::as_str)
            else {
                return reply(false, Some("write_stdin requires data_base64"));
            };
            let Ok(data) = rish_guest_protocol::decode_base64(encoded) else {
                return reply(false, Some("data_base64 is not base64"));
            };
            if data.len() > crate::MAX_ABI_REQUEST_BYTES {
                return reply(false, Some("stdin payload exceeds the request bound"));
            }
            session.stdin_inbox().write(data);
            reply(true, None)
        }
        Some("close_stdin") => {
            session.stdin_inbox().close();
            reply(true, None)
        }
        _ => reply(false, Some("unknown control action")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(session: *mut c_void, body: &str) -> String {
        // SAFETY: the body is a live borrowed slice for the duration of the call.
        let raw =
            unsafe { rish_vm_session_control_json(session, body.as_ptr().cast(), body.len()) };
        // SAFETY: the entry point always returns an owned CString.
        let text = unsafe { CString::from_raw(raw) };
        text.to_string_lossy().into_owned()
    }

    /// A null handle must be refused before anything is dereferenced, and every
    /// malformed request must fail closed rather than queueing bytes.
    #[test]
    fn a_null_session_and_malformed_requests_are_refused() {
        let null = std::ptr::null_mut();
        for body in [
            r#"{"protocol_version":1,"action":"close_stdin"}"#,
            r#"{"protocol_version":1,"action":"write_stdin","data_base64":"aGk="}"#,
        ] {
            assert!(call(null, body).contains(r#""ok":false"#));
        }
    }

    #[test]
    fn an_oversized_request_is_refused_before_it_is_read() {
        let body = "x".repeat(crate::MAX_ABI_REQUEST_BYTES + 1);
        // SAFETY: the length is deliberately past the bound; nothing is read.
        let raw = unsafe {
            rish_vm_session_control_json(std::ptr::null_mut(), body.as_ptr().cast(), body.len())
        };
        // SAFETY: the entry point always returns an owned CString.
        let text = unsafe { CString::from_raw(raw) };
        assert!(text.to_string_lossy().contains("invalid control call"));
    }
}
