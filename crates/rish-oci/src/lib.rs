use std::collections::BTreeMap;
use std::str::FromStr;

use rish_core::{Capability, CapabilityProfile, CapabilityRequirement, GuestCommand};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const HANDLER_LABEL: &str = "io.rish.offload.handler";
pub const REQUIRES_LABEL: &str = "io.rish.requires";
pub const REQUIRES_KERNEL_LABEL: &str = "io.rish.requires-kernel";
const MAX_HANDLER_LENGTH: usize = 128;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageConfig {
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub entrypoint: Vec<String>,
    #[serde(default)]
    pub cmd: Vec<String>,
    #[serde(default)]
    pub working_dir: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub user: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RootFilesystem {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub diff_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageConfiguration {
    pub architecture: String,
    pub os: String,
    pub config: ImageConfig,
    pub rootfs: RootFilesystem,
}

impl ImageConfiguration {
    pub fn validate(&self) -> Result<(), OciError> {
        if self.os != "linux" {
            return Err(OciError::UnsupportedPlatform {
                os: self.os.clone(),
                architecture: self.architecture.clone(),
            });
        }
        if self.architecture != "arm64" {
            return Err(OciError::UnsupportedPlatform {
                os: self.os.clone(),
                architecture: self.architecture.clone(),
            });
        }
        if self.rootfs.kind != "layers" {
            return Err(OciError::InvalidRootFilesystem(format!(
                "rootfs type must be layers, got {}",
                self.rootfs.kind
            )));
        }
        for diff_id in &self.rootfs.diff_ids {
            validate_sha256_diff_id(diff_id)?;
        }
        if !self.config.working_dir.is_empty() && !self.config.working_dir.starts_with('/') {
            return Err(OciError::InvalidWorkingDirectory(
                self.config.working_dir.clone(),
            ));
        }
        if self
            .config
            .env
            .iter()
            .any(|entry| !valid_environment_entry(entry))
        {
            return Err(OciError::InvalidEnvironment);
        }
        if self.config.entrypoint.is_empty() && self.config.cmd.is_empty() {
            return Err(OciError::MissingCommand);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OffloadContract {
    pub handler: String,
    #[serde(default)]
    pub requirements: Vec<CapabilityRequirement>,
}

impl OffloadContract {
    pub fn from_image(image: &ImageConfiguration) -> Result<Option<Self>, OciError> {
        let Some(handler) = image.config.labels.get(HANDLER_LABEL) else {
            return Ok(None);
        };
        if !valid_handler(handler) {
            return Err(OciError::InvalidContract(
                "offload handler must be a 1-128 byte ASCII identifier beginning with an \
                 alphanumeric and containing only letters, digits, '.', '_' or '-'"
                    .to_owned(),
            ));
        }

        let mut requirements = parse_requirements(image.config.labels.get(REQUIRES_LABEL), false)?;
        requirements.extend(parse_requirements(
            image.config.labels.get(REQUIRES_KERNEL_LABEL),
            true,
        )?);

        Ok(Some(Self {
            handler: handler.clone(),
            requirements,
        }))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ImageExecutionPlan {
    NativeOffload {
        handler: String,
        command: GuestCommand,
    },
    NativeLinux {
        command: GuestCommand,
    },
    VirtualMachine {
        command: GuestCommand,
    },
    Rejected {
        reason: String,
    },
}

#[must_use]
pub fn plan_image(image: &ImageConfiguration, profile: &CapabilityProfile) -> ImageExecutionPlan {
    if let Err(error) = image.validate() {
        return ImageExecutionPlan::Rejected {
            reason: error.to_string(),
        };
    }

    match OffloadContract::from_image(image) {
        Ok(Some(contract)) => {
            if let Err(error) = profile.require_all(&contract.requirements) {
                return ImageExecutionPlan::Rejected {
                    reason: error.to_string(),
                };
            }
            ImageExecutionPlan::NativeOffload {
                handler: contract.handler,
                command: image_command(image),
            }
        }
        Err(error) => ImageExecutionPlan::Rejected {
            reason: error.to_string(),
        },
        Ok(None) if profile.level(Capability::LinuxElf).is_runnable() => {
            ImageExecutionPlan::NativeLinux {
                command: image_command(image),
            }
        }
        Ok(None) if profile.level(Capability::FullVirtualMachine).is_runnable() => {
            ImageExecutionPlan::VirtualMachine {
                command: image_command(image),
            }
        }
        Ok(None) => ImageExecutionPlan::Rejected {
            reason: "image has no rish offload contract and no Linux ELF/VM backend is available"
                .to_owned(),
        },
    }
}

fn image_command(image: &ImageConfiguration) -> GuestCommand {
    let mut words = image.config.entrypoint.clone();
    words.extend(image.config.cmd.clone());

    let program = words.first().cloned().unwrap_or_default();
    let args = words.into_iter().skip(1).collect();
    let env = image
        .config
        .env
        .iter()
        .filter_map(|entry| entry.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();

    GuestCommand {
        program,
        args,
        env,
        cwd: if image.config.working_dir.is_empty() {
            "/".to_owned()
        } else {
            image.config.working_dir.clone()
        },
        stdin: Vec::new(),
    }
}

fn valid_handler(handler: &str) -> bool {
    !handler.is_empty()
        && handler.len() <= MAX_HANDLER_LENGTH
        && handler
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && handler
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_environment_entry(entry: &str) -> bool {
    let Some((name, _value)) = entry.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !name.contains('\0')
        && !name.contains('=')
        && name.bytes().all(|byte| !byte.is_ascii_control())
}

fn validate_sha256_diff_id(diff_id: &str) -> Result<(), OciError> {
    let Some(encoded) = diff_id.strip_prefix("sha256:") else {
        return Err(OciError::UnsupportedDiffId(diff_id.to_owned()));
    };
    if encoded.len() != 64
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(OciError::InvalidDiffId(diff_id.to_owned()));
    }
    Ok(())
}

fn parse_requirements(
    value: Option<&String>,
    kernel_semantics_required: bool,
) -> Result<Vec<CapabilityRequirement>, OciError> {
    value
        .map_or("", String::as_str)
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| {
            Capability::from_str(item)
                .map(|capability| CapabilityRequirement {
                    capability,
                    kernel_semantics_required,
                })
                .map_err(|_| OciError::UnknownCapability(item.to_owned()))
        })
        .collect()
}

#[derive(Debug, Error)]
pub enum OciError {
    #[error("invalid rish OCI contract: {0}")]
    InvalidContract(String),

    #[error("unknown capability in OCI contract: {0}")]
    UnknownCapability(String),

    #[error("unsupported OCI platform: {os}/{architecture}")]
    UnsupportedPlatform { os: String, architecture: String },

    #[error("invalid OCI root filesystem: {0}")]
    InvalidRootFilesystem(String),

    #[error("unsupported OCI diff ID algorithm: {0}")]
    UnsupportedDiffId(String),

    #[error("invalid OCI diff ID: {0}")]
    InvalidDiffId(String),

    #[error("OCI WorkingDir must be empty or absolute: {0}")]
    InvalidWorkingDirectory(String),

    #[error("OCI Env entries must have a non-empty NAME=VALUE form")]
    InvalidEnvironment,

    #[error("OCI image has neither Entrypoint nor Cmd")]
    MissingCommand,
}

#[cfg(test)]
mod tests {
    use rish_core::{Platform, PrivilegeMode};
    use rish_runtime::portable_offload_profile;

    use super::*;

    fn image(labels: BTreeMap<String, String>) -> ImageConfiguration {
        ImageConfiguration {
            architecture: "arm64".to_owned(),
            os: "linux".to_owned(),
            config: ImageConfig {
                entrypoint: vec!["/usr/bin/demo".to_owned()],
                cmd: vec!["serve".to_owned()],
                labels,
                ..ImageConfig::default()
            },
            rootfs: RootFilesystem {
                kind: "layers".to_owned(),
                diff_ids: Vec::new(),
            },
        }
    }

    #[test]
    fn portable_backend_accepts_declared_native_offload() {
        let labels = BTreeMap::from([
            (HANDLER_LABEL.to_owned(), "sample.demo".to_owned()),
            (REQUIRES_LABEL.to_owned(), "port_forwarding".to_owned()),
        ]);
        let profile = portable_offload_profile(Platform::Ios, PrivilegeMode::AppSandbox);

        assert!(matches!(
            plan_image(&image(labels), &profile),
            ImageExecutionPlan::NativeOffload { handler, .. } if handler == "sample.demo"
        ));
    }

    #[test]
    fn portable_backend_rejects_unknown_elf() {
        let profile = portable_offload_profile(Platform::Ios, PrivilegeMode::AppSandbox);

        assert!(matches!(
            plan_image(&image(BTreeMap::new()), &profile),
            ImageExecutionPlan::Rejected { .. }
        ));
    }

    #[test]
    fn native_requirement_rejects_emulated_namespace() {
        let labels = BTreeMap::from([
            (HANDLER_LABEL.to_owned(), "sample.demo".to_owned()),
            (
                REQUIRES_KERNEL_LABEL.to_owned(),
                "network_namespace".to_owned(),
            ),
        ]);
        let profile = portable_offload_profile(Platform::Android, PrivilegeMode::AppSandbox);

        assert!(matches!(
            plan_image(&image(labels), &profile),
            ImageExecutionPlan::Rejected { .. }
        ));
    }

    #[test]
    fn rejects_malformed_offload_handler() {
        let labels = BTreeMap::from([(HANDLER_LABEL.to_owned(), "sample.demo\nforged".to_owned())]);
        let profile = portable_offload_profile(Platform::Ios, PrivilegeMode::AppSandbox);

        assert!(matches!(
            plan_image(&image(labels), &profile),
            ImageExecutionPlan::Rejected { reason } if reason.contains("offload handler")
        ));
    }

    #[test]
    fn rejects_invalid_image_process_configuration() {
        let profile = portable_offload_profile(Platform::Ios, PrivilegeMode::AppSandbox);
        let mut invalid = image(BTreeMap::new());
        invalid.config.working_dir = "relative".to_owned();

        assert!(matches!(
            plan_image(&invalid, &profile),
            ImageExecutionPlan::Rejected { reason } if reason.contains("WorkingDir")
        ));

        invalid.config.working_dir = "/".to_owned();
        invalid.config.env = vec!["MISSING_EQUALS".to_owned()];
        assert!(matches!(
            plan_image(&invalid, &profile),
            ImageExecutionPlan::Rejected { reason } if reason.contains("Env")
        ));
    }

    #[test]
    fn accepts_canonical_sha256_diff_ids() {
        let mut valid = image(BTreeMap::new());
        valid.rootfs.diff_ids = vec![format!("sha256:{}", "a".repeat(64))];

        valid.validate().unwrap();
    }
}
