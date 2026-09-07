//! Synchronous C ABI with incremental guest output for interactive login.

use std::ffi::{CString, c_char, c_void};

use rish_guest_protocol::StreamChannel;

pub type OutputCallback = unsafe extern "C" fn(*mut c_void, *const c_char, usize);

/// Runs on the calling worker thread. Callback bytes are borrowed for the
/// duration of the callback only. Each event is versioned JSON with a
/// per-call sequence number, channel, and base64-encoded byte payload.
///
/// # Safety
/// `session` must be a live VM session. `input` must contain `input_len`
/// readable bytes. The callback and its context must remain valid throughout
/// this synchronous call and must not re-enter or free the session.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rish_vm_session_exec_stream_json(
    session: *mut c_void,
    input: *const c_char,
    input_len: usize,
    context: *mut c_void,
    callback: Option<OutputCallback>,
) -> *mut c_char {
    let invalid = || {
        CString::new(
            r#"{"protocol_version":1,"ok":false,"error":"invalid streaming session call"}"#,
        )
        .expect("static JSON")
        .into_raw()
    };
    if session.is_null() || input.is_null() || input_len > crate::MAX_ABI_REQUEST_BYTES {
        return invalid();
    }
    // SAFETY: the caller supplies the live session and bounded readable input.
    let session = unsafe { &*session.cast::<crate::vm_ffi::VmSession>() };
    let bytes = unsafe { std::slice::from_raw_parts(input.cast::<u8>(), input_len) };
    let Ok(request) = std::str::from_utf8(bytes) else {
        return invalid();
    };
    let mut sequence = 0_u64;
    let response =
        crate::vm_ffi::vm_session_exec_observed_json(session, request, &mut |channel, bytes| {
            if let Some(callback) = callback {
                let event = output_event(channel, bytes, sequence);
                // SAFETY: the callback's lifetime is guaranteed by the caller;
                // bytes stay alive until it returns, on this same thread.
                unsafe { callback(context, event.as_ptr().cast(), event.len()) };
                sequence = sequence.saturating_add(1);
            }
        });
    CString::new(response)
        .expect("serialized JSON contains no NUL")
        .into_raw()
}

fn output_event(channel: StreamChannel, bytes: &[u8], sequence: u64) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "protocol_version": 1, "event": "output", "sequence": sequence,
        "channel": channel, "data_base64": rish_guest_protocol::encode_base64(bytes),
    }))
    .expect("output metadata is serializable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_handle_returns_owned_error_without_callback() {
        unsafe extern "C" fn unexpected(_: *mut c_void, _: *const c_char, _: usize) {
            CALLED.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        static CALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        // SAFETY: null handles are explicitly rejected before dereferencing.
        let result = unsafe {
            rish_vm_session_exec_stream_json(
                std::ptr::null_mut(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                Some(unexpected),
            )
        };
        // SAFETY: the function returns one owned CString.
        let result = unsafe { CString::from_raw(result) };
        assert!(
            result
                .to_str()
                .unwrap()
                .contains("invalid streaming session call")
        );
        assert!(!CALLED.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn output_envelope_preserves_binary_bytes_and_its_sequence() {
        let event: serde_json::Value =
            serde_json::from_slice(&output_event(StreamChannel::Stderr, &[0, 255, b'a'], 7))
                .unwrap();
        assert_eq!(event["protocol_version"], 1);
        assert_eq!(event["sequence"], 7);
        assert_eq!(event["channel"], "stderr");
        assert_eq!(
            rish_guest_protocol::decode_base64(event["data_base64"].as_str().unwrap()).unwrap(),
            [0, 255, b'a']
        );
    }
}
