use std::ffi::{CStr, CString, c_char};

use rish_core::{CapabilityProfile, GuestCommand, Platform, PrivilegeMode};
use rish_runtime::{CommandPlan, OffloadRegistry, Planner, portable_offload_profile};
use serde::{Deserialize, Serialize};

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

#[must_use]
pub fn plan_json(input: &str) -> String {
    let response = match serde_json::from_str::<PlanRequest>(input) {
        Ok(request) => {
            let profile = portable_offload_profile(request.platform, request.privilege);
            let planner = Planner::new(profile.clone(), OffloadRegistry::portable_defaults());
            match planner.plan(&request.command) {
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

/// Plans one guest command and returns an owned UTF-8 JSON C string.
///
/// # Safety
///
/// `input` must point to a valid NUL-terminated UTF-8 string. The returned
/// pointer must be released exactly once with [`rish_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rish_plan_json(input: *const c_char) -> *mut c_char {
    if input.is_null() {
        return CString::new(r#"{"protocol_version":1,"ok":false,"error":"null request"}"#)
            .expect("static JSON has no NUL")
            .into_raw();
    }

    // SAFETY: The caller guarantees `input` is a valid NUL-terminated string.
    let request = unsafe { CStr::from_ptr(input) };
    let response = match request.to_str() {
        Ok(request) => plan_json(request),
        Err(error) => format!(
            r#"{{"protocol_version":1,"ok":false,"error":"request is not UTF-8: {error}"}}"#
        ),
    };

    CString::new(response)
        .expect("serialized JSON cannot contain an interior NUL")
        .into_raw()
}

/// Releases a string returned by [`rish_plan_json`].
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

#[unsafe(no_mangle)]
pub extern "C" fn rish_protocol_version() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn ffi_plans_systemctl_as_a_host_call() {
        let response = plan_json(
            r#"{
                "platform":"ios",
                "command":{
                    "program":"systemctl",
                    "args":["start","demo"],
                    "cwd":"/"
                }
            }"#,
        );
        let response: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["ok"], true);
        assert_eq!(response["plan"]["call"]["operation"], "service.systemctl");
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
}
