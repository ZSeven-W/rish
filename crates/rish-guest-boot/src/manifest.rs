//! Parsing and artifact verification for the guest boot manifest.
//!
//! Only the fields this harness consumes are modeled; serde ignores the rest
//! of the versioned schema.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const MANIFEST_SCHEMA: &str = "org.rish.guest.boot-manifest";

#[derive(Debug, Deserialize)]
pub struct BootManifest {
    pub schema: String,
    pub schema_version: u32,
    pub status: String,
    pub architecture: ManifestArchitecture,
    pub machine: ManifestMachine,
    pub boot: ManifestBoot,
    pub artifacts: Vec<ManifestArtifact>,
}

#[derive(Debug, Deserialize)]
pub struct ManifestMachine {
    pub minimum_memory_bytes: u64,
    pub vcpu_count: u32,
}

#[derive(Debug, Deserialize)]
pub struct ManifestArchitecture {
    pub cpu: String,
    pub oci_platform: String,
}

#[derive(Debug, Deserialize)]
pub struct ManifestBoot {
    pub kernel: String,
    pub initramfs: String,
    pub command_line: String,
}

#[derive(Debug, Deserialize)]
pub struct ManifestArtifact {
    pub size: u64,
    pub sha256: String,
}

pub struct LoadedManifest {
    pub manifest: BootManifest,
    pub kernel_path: PathBuf,
    pub initramfs_path: PathBuf,
}

impl LoadedManifest {
    pub fn load(path: &Path) -> Result<Self, String> {
        let directory = path
            .parent()
            .ok_or_else(|| "manifest path has no parent directory".to_owned())?
            .to_path_buf();
        let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
        let manifest: BootManifest =
            serde_json::from_str(&text).map_err(|error| error.to_string())?;
        if manifest.schema != MANIFEST_SCHEMA {
            return Err(format!("unsupported manifest schema {}", manifest.schema));
        }
        if !(1..=2).contains(&manifest.schema_version) {
            return Err(format!(
                "unsupported manifest schema version {}",
                manifest.schema_version
            ));
        }
        let kernel_path = directory.join(&manifest.boot.kernel);
        let initramfs_path = directory.join(&manifest.boot.initramfs);
        for (label, path) in [("kernel", &kernel_path), ("initramfs", &initramfs_path)] {
            if !path.is_file() {
                return Err(format!("{label} artifact is missing: {}", path.display()));
            }
            let digest = sha256_file(path)?;
            let pinned = manifest
                .artifacts
                .iter()
                .find(|artifact| artifact.sha256 == digest)
                .ok_or_else(|| {
                    format!(
                        "{label} {} does not match any pinned artifact digest",
                        path.display()
                    )
                })?;
            if pinned.size != fs::metadata(path).map_err(|error| error.to_string())?.len() {
                return Err(format!("{label} size mismatch for {}", path.display()));
            }
        }
        Ok(Self {
            manifest,
            kernel_path,
            initramfs_path,
        })
    }
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut reader = io::BufReader::new(file);
    let mut hasher = Sha256::new();
    io::copy(&mut reader, &mut hasher).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", hasher.finalize()))
}
