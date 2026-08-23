use std::ffi::{CString, c_char};

use rish_applets::{AppletContext, AppletExecutor, AppletLimits};
use rish_core::{CapabilityProfile, ExecutionOutcome, GuestCommand, Platform, PrivilegeMode};
use rish_runtime::{
    BackendCandidate, CommandPlan, OffloadRegistry, Planner, portable_offload_profile,
};
use serde::{Deserialize, Serialize};

mod pull_ffi;
pub mod vm_ffi;

pub use pull_ffi::{RishRegistryFetchCallback, pull_image_json};

const MAX_ABI_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const MAX_FFI_APPLET_BYTES: usize = 1024 * 1024;
const MAX_FFI_FILESYSTEM_ENTRIES: usize = 10_000;
const MAX_FFI_RECURSION_DEPTH: usize = 64;

#[derive(Debug, Deserialize)]
pub struct PlanRequest {
    pub platform: Platform,
    #[serde(default = "default_privilege")]
    pub privilege: PrivilegeMode,
    pub command: GuestCommand,
}

const fn default_privilege() -> PrivilegeMode {
    PrivilegeMode::AppSandbox
}

#[derive(Debug, Serialize)]
pub struct PlanResponse {
    pub protocol_version: u32,
    pub ok: bool,
    pub profile: Option<CapabilityProfile>,
    pub plan: Option<CommandPlan>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteAppletRequest {
    pub protocol_version: u32,
    pub sandbox_root: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default = "default_user")]
    pub user: String,
    #[serde(default = "default_hostname")]
    pub hostname: String,
    #[serde(default = "default_ffi_limits")]
    pub limits: AppletLimits,
    pub command: GuestCommand,
}

fn default_user() -> String {
    "rish".to_owned()
}

fn default_hostname() -> String {
    "rish".to_owned()
}

fn default_ffi_limits() -> AppletLimits {
    AppletLimits {
        max_input_bytes: MAX_FFI_APPLET_BYTES,
        max_output_bytes: MAX_FFI_APPLET_BYTES,
        max_filesystem_entries: MAX_FFI_FILESYSTEM_ENTRIES,
        max_recursion_depth: MAX_FFI_RECURSION_DEPTH,
    }
}

#[derive(Debug, Serialize)]
pub struct ExecuteAppletResponse {
    pub protocol_version: u32,
    pub ok: bool,
    pub outcome: Option<ExecutionOutcome>,
    pub error: Option<String>,
}

#[must_use]
pub fn plan_json(input: &str) -> String {
    let response = match serde_json::from_str::<PlanRequest>(input) {
        Ok(request) => {
            let profile = portable_offload_profile(request.platform, request.privilege);
            let candidate = BackendCandidate::portable_offload(profile.clone(), 0);
            let plan = candidate
                .map_err(|error| RuntimePlanError::Candidate(error.to_string()))
                .and_then(|candidate| {
                    Planner::new(candidate, OffloadRegistry::portable_defaults())
                        .plan(&request.command)
                        .map_err(|error| RuntimePlanError::Planning(error.to_string()))
                });
            match plan {
                Ok(plan) => PlanResponse {
                    protocol_version: 1,
                    ok: true,
                    profile: Some(profile),
                    plan: Some(plan),
                    error: None,
                },
                Err(error) => PlanResponse {
                    protocol_version: 1,
                    ok: false,
                    profile: Some(profile),
                    plan: None,
                    error: Some(error.to_string()),
                },
            }
        }
        Err(error) => PlanResponse {
            protocol_version: 1,
            ok: false,
            profile: None,
            plan: None,
            error: Some(format!("invalid JSON request: {error}")),
        },
    };

    serde_json::to_string(&response).unwrap_or_else(|error| {
        format!(
            "{{\"protocol_version\":1,\"ok\":false,\"error\":\"serialization failed: {}\"}}",
            error
        )
    })
}

enum RuntimePlanError {
    Candidate(String),
    Planning(String),
}

impl std::fmt::Display for RuntimePlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Candidate(error) => write!(formatter, "invalid backend candidate: {error}"),
            Self::Planning(error) => formatter.write_str(error),
        }
    }
}

#[must_use]
pub fn execute_applet_json(input: &str) -> String {
    let response = match serde_json::from_str::<ExecuteAppletRequest>(input) {
        Ok(request) if request.protocol_version != 1 => ExecuteAppletResponse {
            protocol_version: 1,
            ok: false,
            outcome: None,
            error: Some(format!(
                "unsupported protocol version: {}",
                request.protocol_version
            )),
        },
        Ok(request) => {
            let context = validate_ffi_limits(request.limits)
                .and_then(|limits| {
                    AppletContext::new(&request.sandbox_root).map(|value| (value, limits))
                })
                .and_then(|(context, limits)| context.with_limits(limits))
                .map(|context| {
                    context
                        .read_only(request.read_only)
                        .identity(request.user, request.hostname)
                });
            match context.and_then(|context| AppletExecutor::new(context).execute(&request.command))
            {
                Ok(outcome) => ExecuteAppletResponse {
                    protocol_version: 1,
                    ok: true,
                    outcome: Some(outcome),
                    error: None,
                },
                Err(error) => ExecuteAppletResponse {
                    protocol_version: 1,
                    ok: false,
                    outcome: None,
                    error: Some(error.to_string()),
                },
            }
        }
        Err(error) => ExecuteAppletResponse {
            protocol_version: 1,
            ok: false,
            outcome: None,
            error: Some(format!("invalid JSON request: {error}")),
        },
    };

    serde_json::to_string(&response).unwrap_or_else(|error| {
        format!(
            "{{\"protocol_version\":1,\"ok\":false,\"error\":\"serialization failed: {}\"}}",
            error
        )
    })
}

fn validate_ffi_limits(limits: AppletLimits) -> rish_applets::Result<AppletLimits> {
    if limits.max_input_bytes > MAX_FFI_APPLET_BYTES
        || limits.max_output_bytes > MAX_FFI_APPLET_BYTES
        || limits.max_filesystem_entries > MAX_FFI_FILESYSTEM_ENTRIES
        || limits.max_recursion_depth > MAX_FFI_RECURSION_DEPTH
    {
        return Err(rish_applets::AppletError::usage(
            "limits",
            "FFI limits exceed the platform bridge maximum",
        ));
    }
    limits.validate()
}

/// Plans one guest command and returns an owned UTF-8 JSON C string.
///
/// # Safety
///
/// `input` must point to `input_len` readable bytes. The returned pointer must
/// be released exactly once with [`rish_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rish_plan_json(input: *const c_char, input_len: usize) -> *mut c_char {
    unsafe { invoke_json_abi(input, input_len, plan_json) }
}

/// Executes one bounded portable applet inside an app-owned sandbox root.
///
/// # Safety
///
/// `input` must point to `input_len` readable bytes. The returned pointer must
/// be released exactly once with [`rish_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rish_execute_applet_json(
    input: *const c_char,
    input_len: usize,
) -> *mut c_char {
    unsafe { invoke_json_abi(input, input_len, execute_applet_json) }
}

/// Pulls and verifies one OCI image through a trusted host HTTP callback.
///
/// The operation is synchronous and may block on network and filesystem I/O.
/// Mobile callers must invoke it on a worker thread. Response bodies travel
/// through the borrowed file descriptor supplied to `fetch`; they are never
/// encoded into the JSON control envelopes.
///
/// # Safety
///
/// `input` must point to `input_len` readable bytes. `fetch`, when present,
/// must obey [`RishRegistryFetchCallback`]'s pointer and file-descriptor
/// contract. `context` is passed through unchanged and must remain valid until
/// this function returns. The returned pointer must be released exactly once
/// with [`rish_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rish_pull_image_json(
    input: *const c_char,
    input_len: usize,
    fetch: Option<RishRegistryFetchCallback>,
    context: *mut std::ffi::c_void,
) -> *mut c_char {
    unsafe { pull_ffi::invoke_pull_image_abi(input, input_len, fetch, context) }
}

unsafe fn invoke_json_abi(
    input: *const c_char,
    input_len: usize,
    operation: fn(&str) -> String,
) -> *mut c_char {
    if input.is_null() {
        return CString::new(r#"{"protocol_version":1,"ok":false,"error":"null request"}"#)
            .expect("static JSON has no NUL")
            .into_raw();
    }
    if input_len > MAX_ABI_REQUEST_BYTES {
        return CString::new(
            r#"{"protocol_version":1,"ok":false,"error":"request exceeds ABI size limit"}"#,
        )
        .expect("static JSON has no NUL")
        .into_raw();
    }

    // SAFETY: The caller guarantees that `input_len` bytes are readable.
    let request = unsafe { std::slice::from_raw_parts(input.cast::<u8>(), input_len) };
    let response = match std::str::from_utf8(request) {
        Ok(request) => operation(request),
        Err(error) => format!(
            r#"{{"protocol_version":1,"ok":false,"error":"request is not UTF-8: {error}"}}"#
        ),
    };

    CString::new(response)
        .expect("serialized JSON cannot contain an interior NUL")
        .into_raw()
}

/// Releases a string returned by [`rish_plan_json`] or
/// [`rish_execute_applet_json`].
///
/// # Safety
///
/// `value` must be null or a pointer previously returned by
/// [`rish_plan_json`] that has not yet been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rish_string_free(value: *mut c_char) {
    if !value.is_null() {
        // SAFETY: The caller guarantees ownership and provenance.
        drop(unsafe { CString::from_raw(value) });
    }
}

/// Boots the pure-Rust x86_64 interpreter with an app-supplied kernel and
/// initramfs and runs one command inside the guest (the full docker surface).
///
/// The operation is synchronous and boots a Linux guest, so it is slow and
/// must run on a worker thread. Kernel and initramfs are named by path in the
/// request JSON so the large binaries never cross the ABI as data.
///
/// # Safety
///
/// `input` must point to `input_len` readable bytes. The returned pointer must
/// be released exactly once with [`rish_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rish_vm_run_docker_json(
    input: *const c_char,
    input_len: usize,
) -> *mut c_char {
    unsafe { invoke_json_abi(input, input_len, vm_ffi::vm_run_docker_json) }
}

/// Boots an interactive guest session and returns an opaque handle, or null on
/// failure. Many commands then run over the same booted guest with
/// [`rish_vm_session_exec_json`]; release the handle with
/// [`rish_vm_session_free`]. Boots a Linux guest and blocks — call on a worker
/// thread. The request JSON matches [`rish_vm_run_docker_json`] minus `command`.
///
/// # Safety
///
/// `input` must point to `input_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rish_vm_boot_session(
    input: *const c_char,
    input_len: usize,
) -> *mut std::ffi::c_void {
    if input.is_null() || input_len > MAX_ABI_REQUEST_BYTES {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller guarantees `input_len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(input.cast::<u8>(), input_len) };
    let Ok(request) = std::str::from_utf8(bytes) else {
        return std::ptr::null_mut();
    };
    match vm_ffi::vm_boot_session(request) {
        Ok(session) => Box::into_raw(session).cast::<std::ffi::c_void>(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Runs one command in a live session (from [`rish_vm_boot_session`]) and
/// returns an owned JSON reply. The request is `{"command":["argv0",...]}`.
///
/// # Safety
///
/// `session` must be a live handle from [`rish_vm_boot_session`] that has not
/// been freed. `input` must point to `input_len` readable bytes. The returned
/// pointer must be released once with [`rish_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rish_vm_session_exec_json(
    session: *mut std::ffi::c_void,
    input: *const c_char,
    input_len: usize,
) -> *mut c_char {
    if session.is_null() || input.is_null() || input_len > MAX_ABI_REQUEST_BYTES {
        return CString::new(r#"{"protocol_version":1,"ok":false,"error":"invalid session call"}"#)
            .expect("static JSON has no NUL")
            .into_raw();
    }
    // SAFETY: the caller guarantees a live session handle and readable bytes.
    let session = unsafe { &*(session.cast::<vm_ffi::VmSession>()) };
    let bytes = unsafe { std::slice::from_raw_parts(input.cast::<u8>(), input_len) };
    let response = match std::str::from_utf8(bytes) {
        Ok(request) => vm_ffi::vm_session_exec_json(session, request),
        Err(_) => r#"{"protocol_version":1,"ok":false,"error":"request is not UTF-8"}"#.to_owned(),
    };
    CString::new(response)
        .expect("serialized JSON cannot contain an interior NUL")
        .into_raw()
}

/// Releases a session handle from [`rish_vm_boot_session`], shutting the guest
/// down.
///
/// # Safety
///
/// `session` must be null or a live handle from [`rish_vm_boot_session`] that
/// has not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rish_vm_session_free(session: *mut std::ffi::c_void) {
    if !session.is_null() {
        // SAFETY: the caller guarantees ownership and provenance.
        drop(unsafe { Box::from_raw(session.cast::<vm_ffi::VmSession>()) });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rish_protocol_version() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn ffi_plans_grep_as_a_portable_applet() {
        let response = plan_json(
            r#"{
                "platform":"ios",
                "command":{
                    "program":"grep",
                    "args":["needle"],
                    "cwd":"/"
                }
            }"#,
        );
        let response: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["ok"], true);
        assert_eq!(response["plan"]["kind"], "portable_applet");
        assert_eq!(response["plan"]["name"], "grep");
    }

    #[test]
    fn ffi_rejects_dockerd_on_stock_ios() {
        let response = plan_json(
            r#"{
                "platform":"ios",
                "command":{"program":"dockerd","cwd":"/"}
            }"#,
        );
        let response: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["ok"], false);
        assert!(
            response["error"]
                .as_str()
                .unwrap()
                .contains("nested_containers")
        );
    }

    #[test]
    fn ffi_rejects_unbound_optional_docker_handler() {
        let response = plan_json(
            r#"{
                "platform":"ios",
                "command":{"program":"docker","args":["ps"],"cwd":"/"}
            }"#,
        );
        let response: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["ok"], false);
        assert!(response["error"].as_str().unwrap().contains("oci_images"));
    }

    #[test]
    fn ffi_rejects_real_systemctl_on_stock_ios() {
        let response = plan_json(
            r#"{
                "platform":"ios",
                "command":{
                    "program":"systemctl",
                    "args":["status","demo.service"],
                    "cwd":"/"
                }
            }"#,
        );
        let response: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["ok"], false);
        assert!(response["error"].as_str().unwrap().contains("real kernel"));
    }

    #[test]
    fn ffi_executes_binary_safe_portable_applet() {
        let root = tempfile::TempDir::new().unwrap();
        let canonical_root = root.path().canonicalize().unwrap();
        let response = execute_applet_json(
            &serde_json::json!({
                "protocol_version": 1,
                "sandbox_root": canonical_root,
                "command": {
                    "program": "sha256sum",
                    "stdin": [97, 98, 99],
                    "cwd": "/"
                }
            })
            .to_string(),
        );
        let response: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["ok"], true);
        assert_eq!(response["outcome"]["path"]["kind"], "portable_applet");
        assert_eq!(response["outcome"]["path"]["name"], "sha256sum");
    }

    #[test]
    fn ffi_rejects_relative_sandbox_root() {
        let response = execute_applet_json(
            r#"{
                "protocol_version":1,
                "sandbox_root":"relative",
                "command":{"program":"true","cwd":"/"}
            }"#,
        );
        let response: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["ok"], false);
        assert!(response["error"].as_str().unwrap().contains("absolute"));
    }

    #[test]
    fn ffi_rejects_limits_that_could_amplify_json_excessively() {
        let root = tempfile::TempDir::new().unwrap();
        let response = execute_applet_json(
            &serde_json::json!({
                "protocol_version": 1,
                "sandbox_root": root.path().canonicalize().unwrap(),
                "limits": {
                    "max_input_bytes": MAX_FFI_APPLET_BYTES + 1,
                    "max_output_bytes": MAX_FFI_APPLET_BYTES,
                    "max_filesystem_entries": MAX_FFI_FILESYSTEM_ENTRIES,
                    "max_recursion_depth": MAX_FFI_RECURSION_DEPTH
                },
                "command": {"program": "true", "cwd": "/"}
            })
            .to_string(),
        );
        let response: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["ok"], false);
        assert!(response["error"].as_str().unwrap().contains("FFI limits"));
    }

    #[test]
    fn c_abi_rejects_an_oversized_length_before_json_parsing() {
        let request = vec![b' '; MAX_ABI_REQUEST_BYTES + 1];
        // SAFETY: `request` contains exactly the number of readable bytes
        // passed to the length-delimited ABI.
        let response = unsafe { rish_plan_json(request.as_ptr().cast::<c_char>(), request.len()) };
        assert!(!response.is_null());
        // SAFETY: the Rust ABI returns an owned NUL-terminated C string.
        let response_text = unsafe { std::ffi::CStr::from_ptr(response) }
            .to_str()
            .unwrap();
        assert!(response_text.contains("ABI size limit"));
        // SAFETY: `response` came from `rish_plan_json` and is freed once.
        unsafe { rish_string_free(response) };
    }
}
