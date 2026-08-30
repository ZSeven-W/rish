use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
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

#[derive(Clone, Debug)]
pub struct ArtifactFile {
    path: PathBuf,
    bytes: u64,
    /// The open handle the validation was performed on. Consumers that need
    /// the file contents must read through this handle, never re-open the
    /// path: a path swap between validation and use (TOCTOU) would otherwise
    /// substitute a file that never passed the checks. Shared through an Arc
    /// because the worker keeps a cloned artifact set for its snapshots.
    file: Option<std::sync::Arc<File>>,
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

    /// The validated open handle. Always `Some` for artifacts loaded through
    /// [`ValidatedArtifacts::load`].
    #[must_use]
    pub fn file(&self) -> Option<&File> {
        self.file.as_deref()
    }

    /// The validated open handle by value. When another clone of this
    /// artifact still shares the handle, the descriptor is duplicated; the
    /// duplicate refers to the same open file, so the inode stays the one
    /// that was validated.
    #[must_use]
    pub fn into_file(self) -> Option<File> {
        let shared = self.file?;
        match std::sync::Arc::try_unwrap(shared) {
            Ok(file) => Some(file),
            Err(shared) => shared.try_clone().ok(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ValidatedArtifacts {
    pub kernel: ArtifactFile,
    pub kernel_format: KernelFormat,
    pub initrd: Option<ArtifactFile>,
    pub root_disk: ArtifactFile,
}

impl ValidatedArtifacts {
    pub(crate) fn load(config: &VmConfig, limits: &EngineLimits) -> Result<Self, SoftVmError> {
        let mut kernel = regular_file(
            Path::new(&config.kernel_path),
            "kernel",
            limits.max_kernel_bytes,
            false,
        )?;
        // The kernel header check reads through the same handle the
        // metadata validation used, so the two checks can never observe
        // different files.
        let kernel_path = kernel.path().to_path_buf();
        let kernel_format = validate_kernel(
            std::sync::Arc::get_mut(
                kernel
                    .file
                    .as_mut()
                    .expect("regular_file always keeps the open handle"),
            )
            .expect("the fresh artifact handle is not yet shared"),
            &kernel_path,
        )?;
        let initrd = config
            .initrd_path
            .as_deref()
            .map(|path| regular_file(Path::new(path), "initrd", limits.max_initrd_bytes, false))
            .transpose()?;
        let root_disk = regular_file(
            Path::new(&config.root_disk_path),
            "root disk",
            limits.max_root_disk_bytes,
            true,
        )?;
        Ok(Self {
            kernel,
            kernel_format,
            initrd,
            root_disk,
        })
    }
}

/// Validates a path, then opens it once and re-validates the opened inode,
/// keeping the handle. Everything the engine reads afterwards goes through
/// this handle, so a path swap after validation (TOCTOU) cannot substitute
/// a file that skipped the regular-file, non-empty, and size-limit checks.
fn regular_file(
    path: &Path,
    kind: &'static str,
    limit: u64,
    writable: bool,
) -> Result<ArtifactFile, SoftVmError> {
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
    let mut options = fs::OpenOptions::new();
    options.read(true);
    if writable {
        options.write(true);
    }
    let file = options
        .open(path)
        .map_err(|source| SoftVmError::ArtifactRead {
            kind,
            path: path.to_path_buf(),
            source,
        })?;
    // Re-check the opened inode: the path-level metadata above is only a
    // fast fail for config errors; these checks bind to what was opened.
    let opened = file
        .metadata()
        .map_err(|source| SoftVmError::ArtifactRead {
            kind,
            path: path.to_path_buf(),
            source,
        })?;
    if !opened.is_file() {
        return Err(SoftVmError::InvalidConfig(format!(
            "{kind} path must name a regular file"
        )));
    }
    if opened.len() == 0 {
        return Err(SoftVmError::InvalidConfig(format!(
            "{kind} artifact cannot be empty"
        )));
    }
    if opened.len() > limit {
        return Err(SoftVmError::ArtifactTooLarge {
            kind,
            actual: opened.len(),
            limit,
        });
    }
    Ok(ArtifactFile {
        path: path.to_path_buf(),
        bytes: opened.len(),
        file: Some(std::sync::Arc::new(file)),
    })
}

fn validate_kernel(file: &mut File, path: &Path) -> Result<KernelFormat, SoftVmError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| SoftVmError::ArtifactRead {
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
        let path = file.path().to_path_buf();

        assert!(matches!(
            validate_kernel(file.as_file_mut(), &path),
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
        let path = file.path().to_path_buf();

        assert_eq!(
            validate_kernel(file.as_file_mut(), &path).unwrap(),
            KernelFormat::Elf64
        );
    }

    #[test]
    fn the_root_disk_binds_to_the_validated_inode_not_the_path() {
        use rish_vm::{VmAcceleration, VmDevice};

        let directory = tempfile::tempdir().unwrap();
        let kernel_path = directory.path().join("vmlinuz");
        let disk_path = directory.path().join("root.img");
        // A minimal valid 64-bit bzImage header so the kernel passes
        // validate_kernel.
        let mut image = vec![0_u8; LINUX_BOOT_HEADER_END];
        image[0x1fe..0x200].copy_from_slice(&[0x55, 0xaa]);
        image[0x202..0x206].copy_from_slice(b"HdrS");
        image[0x236..0x238].copy_from_slice(&XLF_KERNEL_64.to_le_bytes());
        std::fs::write(&kernel_path, &image).unwrap();
        std::fs::write(&disk_path, vec![0x11; 4096]).unwrap();

        let config = VmConfig {
            architecture: "x86_64".to_owned(),
            vcpus: 1,
            memory_mib: 128,
            kernel_path: kernel_path.to_string_lossy().into_owned(),
            initrd_path: None,
            root_disk_path: disk_path.to_string_lossy().into_owned(),
            acceleration: VmAcceleration::Interpreter,
            devices: vec![VmDevice::Console],
            command_line: String::new(),
        };
        let artifacts = ValidatedArtifacts::load(&config, &EngineLimits::default()).unwrap();

        // After validation, a hostile process swaps the path for a sparse
        // file far beyond the engine limit.
        std::fs::remove_file(&disk_path).unwrap();
        let swapped = File::create(&disk_path).unwrap();
        swapped.set_len(32 * 1024 * 1024 * 1024).unwrap();

        assert_eq!(artifacts.root_disk.bytes(), 4096);
        // The backend binds to the validated handle: the original 4096-byte
        // image, not the 32 GiB replacement.
        let mut backend = rish_softvm_core::virtio::FileBlockBackend::from_file(
            artifacts.root_disk.into_file().unwrap(),
        )
        .unwrap();
        assert_eq!(
            rish_softvm_core::virtio::BlockBackend::length(&backend),
            4096
        );
        let mut first = [0_u8; 4];
        rish_softvm_core::virtio::BlockBackend::read_at(&mut backend, 0, &mut first).unwrap();
        assert_eq!(first, [0x11; 4]);
    }
}
