use std::collections::BTreeMap;
use std::str::FromStr;

use rish_core::{Capability, CapabilityProfile, CapabilityRequirement, GuestCommand};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const HANDLER_LABEL: &str = "io.rish.offload.handler";
pub const REQUIRES_LABEL: &str = "io.rish.requires";
pub const REQUIRES_KERNEL_LABEL: &str = "io.rish.requires-kernel";

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
        if handler.trim().is_empty() {
            return Err(OciError::InvalidContract(
                "offload handler must not be empty".to_owned(),
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
    if image.os != "linux" {
        return ImageExecutionPlan::Rejected {
            reason: format!("unsupported OCI operating system: {}", image.os),
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
}
