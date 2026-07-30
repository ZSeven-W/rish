use std::collections::BTreeSet;

use rish_core::Capability;
use serde::{Deserialize, Serialize};

use crate::VmError;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GuestKernelContract {
    required_kconfig: BTreeSet<String>,
    required_capabilities: BTreeSet<Capability>,
}

impl GuestKernelContract {
    #[must_use]
    pub fn new<K, S>(
        required_kconfig: K,
        required_capabilities: impl IntoIterator<Item = Capability>,
    ) -> Self
    where
        K: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            required_kconfig: required_kconfig.into_iter().map(Into::into).collect(),
            required_capabilities: required_capabilities.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn container_host() -> Self {
        Self::new(
            [
                "CONFIG_NAMESPACES",
                "CONFIG_PID_NS",
                "CONFIG_USER_NS",
                "CONFIG_UTS_NS",
                "CONFIG_IPC_NS",
                "CONFIG_NET_NS",
                "CONFIG_CGROUPS",
                "CONFIG_CGROUP_BPF",
                "CONFIG_CGROUP_CPUACCT",
                "CONFIG_CGROUP_DEVICE",
                "CONFIG_CGROUP_FREEZER",
                "CONFIG_CGROUP_PIDS",
                "CONFIG_MEMCG",
                "CONFIG_BLK_CGROUP",
                "CONFIG_CPUSETS",
                "CONFIG_SECCOMP",
                "CONFIG_SECCOMP_FILTER",
                "CONFIG_OVERLAY_FS",
                "CONFIG_MODULES",
                "CONFIG_DEVTMPFS",
                "CONFIG_DEVTMPFS_MOUNT",
                "CONFIG_TUN",
                "CONFIG_VETH",
                "CONFIG_BRIDGE",
                "CONFIG_NETFILTER",
                "CONFIG_NF_TABLES",
            ],
            [
                Capability::LinuxElf,
                Capability::ProcessNamespace,
                Capability::UserNamespace,
                Capability::MountNamespace,
                Capability::NetworkNamespace,
                Capability::UtsNamespace,
                Capability::IpcNamespace,
                Capability::CgroupsV2,
                Capability::PrivilegedContainers,
                Capability::KernelModules,
                Capability::DeviceNodes,
                Capability::Systemd,
                Capability::NestedContainers,
                Capability::PortForwarding,
                Capability::RawSockets,
                Capability::TunTap,
            ],
        )
    }

    #[must_use]
    pub fn required_kconfig(&self) -> &BTreeSet<String> {
        &self.required_kconfig
    }

    #[must_use]
    pub fn required_capabilities(&self) -> &BTreeSet<Capability> {
        &self.required_capabilities
    }

    #[must_use]
    pub fn validate_kconfig(&self, evidence: &GuestKernelEvidence) -> KernelContractReport {
        KernelContractReport {
            missing: self
                .required_kconfig
                .difference(evidence.enabled())
                .cloned()
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum KernelEvidenceSource {
    ProcConfigGzip {
        kernel_release: String,
    },
    BootConfig {
        kernel_release: String,
    },
    BuildManifest {
        kernel_release: String,
        sha256: String,
    },
}

impl KernelEvidenceSource {
    #[must_use]
    pub fn kernel_release(&self) -> &str {
        match self {
            Self::ProcConfigGzip { kernel_release }
            | Self::BootConfig { kernel_release }
            | Self::BuildManifest { kernel_release, .. } => kernel_release,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GuestKernelEvidence {
    session_id: String,
    source: KernelEvidenceSource,
    enabled: BTreeSet<String>,
}

impl GuestKernelEvidence {
    pub fn new(
        session_id: impl Into<String>,
        source: KernelEvidenceSource,
        enabled: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, VmError> {
        let evidence = Self {
            session_id: session_id.into(),
            source,
            enabled: enabled.into_iter().map(Into::into).collect(),
        };
        evidence.validate()?;
        Ok(evidence)
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    #[must_use]
    pub fn source(&self) -> &KernelEvidenceSource {
        &self.source
    }

    #[must_use]
    pub fn enabled(&self) -> &BTreeSet<String> {
        &self.enabled
    }

    pub(crate) fn validate(&self) -> Result<(), VmError> {
        if self.session_id.is_empty() {
            return Err(VmError::InvalidKernelEvidence(
                "session ID cannot be empty".to_owned(),
            ));
        }
        validate_source(&self.source)?;
        if self.enabled.iter().any(|option| !is_kconfig_symbol(option)) {
            return Err(VmError::InvalidKernelEvidence(
                "enabled entries must be canonical CONFIG_* symbols".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn is_kconfig_symbol(value: &str) -> bool {
    value.starts_with("CONFIG_")
        && value.len() > "CONFIG_".len()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn validate_source(source: &KernelEvidenceSource) -> Result<(), VmError> {
    if source.kernel_release().is_empty() {
        return Err(VmError::InvalidKernelEvidence(
            "kernel evidence release cannot be empty".to_owned(),
        ));
    }
    if let KernelEvidenceSource::BuildManifest { sha256, .. } = source {
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(VmError::InvalidKernelEvidence(
                "build-manifest evidence requires a canonical lowercase SHA-256 digest".to_owned(),
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KernelContractReport {
    missing: BTreeSet<String>,
}

impl KernelContractReport {
    #[must_use]
    pub fn is_satisfied(&self) -> bool {
        self.missing.is_empty()
    }

    #[must_use]
    pub fn missing(&self) -> &BTreeSet<String> {
        &self.missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_manifest_digest_must_be_canonical_lowercase() {
        let error = GuestKernelEvidence::new(
            "session",
            KernelEvidenceSource::BuildManifest {
                kernel_release: "6.12-rish".to_owned(),
                sha256: "A".repeat(64),
            },
            ["CONFIG_NAMESPACES"],
        )
        .unwrap_err();
        assert!(matches!(error, VmError::InvalidKernelEvidence(_)));
    }

    #[test]
    fn deserialization_cannot_bypass_evidence_validation() {
        let evidence: GuestKernelEvidence = serde_json::from_value(serde_json::json!({
            "session_id": "session",
            "source": {
                "kind": "build_manifest",
                "kernel_release": "6.12-rish",
                "sha256": "A".repeat(64)
            },
            "enabled": ["CONFIG_NAMESPACES"]
        }))
        .unwrap();

        assert!(matches!(
            evidence.validate(),
            Err(VmError::InvalidKernelEvidence(_))
        ));
    }

    #[test]
    fn contract_report_names_missing_symbols() {
        let contract = GuestKernelContract::new(
            ["CONFIG_NAMESPACES", "CONFIG_CGROUPS"],
            [Capability::LinuxElf],
        );
        let evidence = GuestKernelEvidence::new(
            "session",
            KernelEvidenceSource::BuildManifest {
                kernel_release: "6.12-rish".to_owned(),
                sha256: "a".repeat(64),
            },
            ["CONFIG_NAMESPACES"],
        )
        .unwrap();
        let report = contract.validate_kconfig(&evidence);

        assert!(!report.is_satisfied());
        assert_eq!(
            report.missing(),
            &BTreeSet::from(["CONFIG_CGROUPS".to_owned()])
        );
    }
}
