use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use rish_vm::VmConfig;
use serde::{Deserialize, Serialize};

use crate::{EngineLimits, SoftVmError};

const LINUX_BOOT_HEADER_END: usize = 0x238;
const ELF_MACHINE_X86_64: u16 = 62;
const XLF_KERNEL_64: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelFormat {
    LinuxBzImage,
    Elf64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactFile {
    path: PathBuf,
    bytes: u64,
}

impl ArtifactFile {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedArtifacts {
    pub kernel: ArtifactFile,
    pub kernel_format: KernelFormat,
    pub initrd: Option<ArtifactFile>,
    pub root_disk: ArtifactFile,
}

impl ValidatedArtifacts {
    pub(crate) fn load(config: &VmConfig, limits: &EngineLimits) -> Result<Self, SoftVmError> {
        let kernel = regular_file(
            Path::new(&config.kernel_path),
            "kernel",
            limits.max_kernel_bytes,
        )?;
        let kernel_format = validate_kernel(kernel.path())?;
        let initrd = config
            .initrd_path
            .as_deref()
            .map(|path| regular_file(Path::new(path), "initrd", limits.max_initrd_bytes))
            .transpose()?;
        let root_disk = regular_file(
            Path::new(&config.root_disk_path),
            "root disk",
            limits.max_root_disk_bytes,
        )?;
        Ok(Self {
            kernel,
            kernel_format,
            initrd,
            root_disk,
        })
    }
}

fn regular_file(path: &Path, kind: &'static str, limit: u64) -> Result<ArtifactFile, SoftVmError> {
    let metadata = fs::metadata(path).map_err(|source| SoftVmError::ArtifactRead {
        kind,
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(SoftVmError::InvalidConfig(format!(
            "{kind} path must name a regular file"
        )));
    }
    if metadata.len() == 0 {
        return Err(SoftVmError::InvalidConfig(format!(
            "{kind} artifact cannot be empty"
        )));
    }
    if metadata.len() > limit {
        return Err(SoftVmError::ArtifactTooLarge {
            kind,
            actual: metadata.len(),
            limit,
        });
    }
    Ok(ArtifactFile {
        path: path.to_path_buf(),
        bytes: metadata.len(),
    })
}

fn validate_kernel(path: &Path) -> Result<KernelFormat, SoftVmError> {
    let mut file = File::open(path).map_err(|source| SoftVmError::ArtifactRead {
        kind: "kernel",
        path: path.to_path_buf(),
        source,
    })?;
    let mut header = vec![0; LINUX_BOOT_HEADER_END];
    let read = file
        .read(&mut header)
        .map_err(|source| SoftVmError::ArtifactRead {
            kind: "kernel",
            path: path.to_path_buf(),
            source,
        })?;
    header.truncate(read);

    if header.starts_with(b"\x7fELF") {
        if header.len() < 20 || header[4] != 2 || header[5] != 1 || header[6] != 1 {
            return Err(invalid_kernel(
                "ELF kernel must be a little-endian ELF64 image",
            ));
        }
        let machine = u16::from_le_bytes([header[18], header[19]]);
        if machine != ELF_MACHINE_X86_64 {
            return Err(invalid_kernel("ELF machine is not AMD64"));
        }
        return Ok(KernelFormat::Elf64);
    }

    if header.len() < LINUX_BOOT_HEADER_END {
        return Err(invalid_kernel(
            "kernel is neither AMD64 ELF64 nor a complete Linux boot-protocol header",
        ));
    }
    if header[0x1fe..0x200] != [0x55, 0xaa] || &header[0x202..0x206] != b"HdrS" {
        return Err(invalid_kernel(
            "Linux boot flag or HdrS signature is missing",
        ));
    }
    let xloadflags = u16::from_le_bytes([header[0x236], header[0x237]]);
    if xloadflags & XLF_KERNEL_64 == 0 {
        return Err(invalid_kernel(
            "Linux boot image does not advertise a 64-bit kernel",
        ));
    }
    Ok(KernelFormat::LinuxBzImage)
}

fn invalid_kernel(message: &str) -> SoftVmError {
    SoftVmError::InvalidKernel(message.to_owned())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn rejects_a_32_bit_linux_boot_header() {
        let mut file = NamedTempFile::new().unwrap();
        let mut image = vec![0; LINUX_BOOT_HEADER_END];
        image[0x1fe..0x200].copy_from_slice(&[0x55, 0xaa]);
        image[0x202..0x206].copy_from_slice(b"HdrS");
        file.write_all(&image).unwrap();

        assert!(matches!(
            validate_kernel(file.path()),
            Err(SoftVmError::InvalidKernel(_))
        ));
    }

    #[test]
    fn accepts_an_amd64_elf_header() {
        let mut file = NamedTempFile::new().unwrap();
        let mut image = vec![0; 64];
        image[..4].copy_from_slice(b"\x7fELF");
        image[4] = 2;
        image[5] = 1;
        image[6] = 1;
        image[18..20].copy_from_slice(&ELF_MACHINE_X86_64.to_le_bytes());
        file.write_all(&image).unwrap();

        assert_eq!(validate_kernel(file.path()).unwrap(), KernelFormat::Elf64);
    }
}
