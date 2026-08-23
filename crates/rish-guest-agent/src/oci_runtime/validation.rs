use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rish_guest_protocol::{ErrorCode, OciPrepareRequest, RemoteError};
use serde::Deserialize;
use serde_json::Value;

use super::{MAX_CONTAINER_ID_BYTES, MAX_IMAGE_REFERENCE_BYTES, invalid};

static NEXT_SPEC_ID: AtomicU64 = AtomicU64::new(1);
const ROOTFS_RECORD_NAME: &str = ".rish-verified-rootfs.json";
const ROOTFS_RECORD_PROTOCOL: &str = "dev.rish.verified-rootfs";
const ROOTFS_RECORD_VERSION: u16 = 1;
const MAX_ROOTFS_RECORD_BYTES: u64 = 16 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifiedRootfsRecord {
    protocol: String,
    version: u16,
    image_digest: String,
}

pub(super) fn validate_runtime_path(path: &Path) -> Result<PathBuf, RemoteError> {
    if !is_clean_absolute_path(path) {
        return Err(invalid("runtime_path must be a clean absolute path"));
    }
    let canonical = path.canonicalize().map_err(|error| {
        RemoteError::new(
            ErrorCode::Io,
            format!("failed to resolve OCI runtime executable: {error}"),
        )
    })?;
    let metadata = canonical.metadata().map_err(|error| {
        RemoteError::new(
            ErrorCode::Io,
            format!("failed to inspect OCI runtime executable: {error}"),
        )
    })?;
    if !metadata.is_file() || !is_executable(&metadata) {
        return Err(RemoteError::new(
            ErrorCode::PermissionDenied,
            "OCI runtime path must identify an executable regular file",
        ));
    }
    if is_group_or_world_writable(&metadata) {
        return Err(RemoteError::new(
            ErrorCode::PermissionDenied,
            "OCI runtime executable must not be group or world writable",
        ));
    }
    Ok(canonical)
}

pub(super) fn prepare_directory(path: &Path, label: &str) -> Result<PathBuf, RemoteError> {
    if !is_clean_absolute_path(path) {
        return Err(invalid(format!("{label} must be a clean absolute path")));
    }
    fs::create_dir_all(path).map_err(|error| {
        RemoteError::new(
            ErrorCode::Io,
            format!("failed to create OCI {label}: {error}"),
        )
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        RemoteError::new(
            ErrorCode::Io,
            format!("failed to inspect OCI {label}: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid(format!(
            "OCI {label} must be a non-symlink directory"
        )));
    }
    path.canonicalize().map_err(|error| {
        RemoteError::new(
            ErrorCode::Io,
            format!("failed to resolve OCI {label}: {error}"),
        )
    })
}

pub(super) fn validate_spec(
    spec: &Value,
    bundle: &Path,
    limit: usize,
) -> Result<Vec<u8>, RemoteError> {
    let object = spec
        .as_object()
        .ok_or_else(|| invalid("oci_spec must be a JSON object"))?;
    let version = object
        .get("ociVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("oci_spec.ociVersion must be a string"))?;
    if version.is_empty() || version.len() > 64 {
        return Err(invalid("oci_spec.ociVersion has an invalid length"));
    }
    let process = object
        .get("process")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("oci_spec.process must be an object"))?;
    if process
        .get("terminal")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(RemoteError::new(
            ErrorCode::UnsupportedOperation,
            "terminal OCI specs require a negotiated console socket",
        ));
    }
    let args = process
        .get("args")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("oci_spec.process.args must be an array"))?;
    if args.is_empty()
        || args.iter().any(|argument| argument.as_str().is_none())
        || args[0].as_str().is_none_or(str::is_empty)
    {
        return Err(invalid(
            "oci_spec.process.args must contain a non-empty program",
        ));
    }
    let root_path = object
        .get("root")
        .and_then(Value::as_object)
        .and_then(|root| root.get("path"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("oci_spec.root.path must be a string"))?;
    let root_path = Path::new(root_path);
    if !is_clean_relative_path(root_path) {
        return Err(invalid(
            "oci_spec.root.path must be a clean relative path inside the bundle",
        ));
    }
    let rootfs = bundle.join(root_path);
    let metadata = fs::symlink_metadata(&rootfs).map_err(|error| {
        RemoteError::new(
            ErrorCode::Io,
            format!("failed to inspect guest rootfs: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid("guest rootfs must be a non-symlink directory"));
    }
    let canonical_rootfs = rootfs.canonicalize().map_err(|error| {
        RemoteError::new(
            ErrorCode::Io,
            format!("failed to resolve guest rootfs: {error}"),
        )
    })?;
    if canonical_rootfs == bundle || !canonical_rootfs.starts_with(bundle) {
        return Err(RemoteError::new(
            ErrorCode::PermissionDenied,
            "guest rootfs escaped its OCI bundle",
        ));
    }

    let bytes = serde_json::to_vec(spec).map_err(|error| {
        RemoteError::new(
            ErrorCode::InvalidRequest,
            format!("failed to encode oci_spec: {error}"),
        )
    })?;
    if bytes.len() > limit {
        return Err(RemoteError::new(
            ErrorCode::ResourceExhausted,
            format!("oci_spec exceeds the configured {limit} byte limit"),
        ));
    }
    Ok(bytes)
}

pub(super) fn validate_image_identity(request: &OciPrepareRequest) -> Result<String, RemoteError> {
    let reference = request.image_reference.as_str();
    if reference.is_empty()
        || reference.len() > MAX_IMAGE_REFERENCE_BYTES
        || reference.trim() != reference
        || reference.chars().any(char::is_control)
    {
        return Err(invalid("image_reference is invalid"));
    }
    let reference_digest = reference.rsplit_once('@').map(|(_, digest)| digest);
    let digest = request
        .expected_digest
        .as_deref()
        .or(reference_digest)
        .ok_or_else(|| invalid("expected_digest or a digest-pinned image_reference is required"))?;
    validate_sha256_digest(digest)?;
    if let Some(reference_digest) = reference_digest {
        validate_sha256_digest(reference_digest)?;
        if reference_digest != digest {
            return Err(invalid(
                "expected_digest does not match the digest-pinned image_reference",
            ));
        }
    }
    Ok(digest.to_owned())
}

pub(super) fn validate_rootfs_record(
    bundle: &Path,
    expected_digest: &str,
) -> Result<(), RemoteError> {
    let path = bundle.join(ROOTFS_RECORD_NAME);
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        let message = if error.kind() == io::ErrorKind::NotFound {
            "guest rootfs has no verified import record".to_owned()
        } else {
            format!("failed to inspect guest rootfs import record: {error}")
        };
        RemoteError::new(ErrorCode::Oci, message)
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_ROOTFS_RECORD_BYTES
    {
        return Err(RemoteError::new(
            ErrorCode::Oci,
            "guest rootfs import record is not a bounded regular file",
        ));
    }
    if is_group_or_world_writable(&metadata) {
        return Err(RemoteError::new(
            ErrorCode::PermissionDenied,
            "guest rootfs import record must not be group or world writable",
        ));
    }
    let bytes = fs::read(&path).map_err(|error| {
        RemoteError::new(
            ErrorCode::Io,
            format!("failed to read guest rootfs import record: {error}"),
        )
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_ROOTFS_RECORD_BYTES {
        return Err(RemoteError::new(
            ErrorCode::ResourceExhausted,
            "guest rootfs import record exceeded its size limit",
        ));
    }
    let record: VerifiedRootfsRecord = serde_json::from_slice(&bytes).map_err(|error| {
        RemoteError::new(
            ErrorCode::Oci,
            format!("guest rootfs import record is invalid: {error}"),
        )
    })?;
    if record.protocol != ROOTFS_RECORD_PROTOCOL || record.version != ROOTFS_RECORD_VERSION {
        return Err(RemoteError::new(
            ErrorCode::VersionMismatch,
            "guest rootfs import record uses an unsupported protocol version",
        ));
    }
    validate_sha256_digest(&record.image_digest)?;
    if record.image_digest != expected_digest {
        return Err(RemoteError::new(
            ErrorCode::PermissionDenied,
            "guest rootfs import record digest does not match the requested image",
        ));
    }
    Ok(())
}

fn validate_sha256_digest(digest: &str) -> Result<(), RemoteError> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(invalid("image digest must use sha256"));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid(
            "image digest must contain 64 lowercase hexadecimal characters",
        ));
    }
    Ok(())
}

pub(super) fn validate_container_id(container_id: &str) -> Result<(), RemoteError> {
    let bytes = container_id.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_CONTAINER_ID_BYTES
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.+-".contains(byte))
    {
        return Err(invalid(
            "container_id must be 1-128 ASCII bytes starting with an alphanumeric character",
        ));
    }
    Ok(())
}

pub(super) fn normalize_signal(signal: Option<&str>) -> Result<&'static str, RemoteError> {
    match signal.unwrap_or("SIGTERM").to_ascii_uppercase().as_str() {
        "HUP" | "SIGHUP" => Ok("SIGHUP"),
        "INT" | "SIGINT" => Ok("SIGINT"),
        "QUIT" | "SIGQUIT" => Ok("SIGQUIT"),
        "KILL" | "SIGKILL" => Ok("SIGKILL"),
        "TERM" | "SIGTERM" => Ok("SIGTERM"),
        _ => Err(invalid(
            "OCI stop signal must be HUP, INT, QUIT, KILL, or TERM",
        )),
    }
}

pub(super) fn write_spec_atomically(bundle: &Path, bytes: &[u8]) -> Result<(), RemoteError> {
    let id = NEXT_SPEC_ID.fetch_add(1, Ordering::Relaxed);
    let temporary = bundle.join(format!(".config.json.rish-{}-{id}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    configure_private_file(&mut options);
    let mut file = options.open(&temporary).map_err(|error| {
        RemoteError::new(
            ErrorCode::Io,
            format!("failed to create temporary OCI config: {error}"),
        )
    })?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, bundle.join("config.json"))?;
        File::open(bundle)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error: io::Error| {
        RemoteError::new(
            ErrorCode::Io,
            format!("failed to publish OCI config: {error}"),
        )
    })
}

pub(super) fn validate_duration(
    value: Duration,
    maximum: Duration,
    name: &str,
) -> Result<(), RemoteError> {
    if value.is_zero() || value > maximum {
        return Err(invalid(format!(
            "{name} must be between 1 and {} ms",
            maximum.as_millis()
        )));
    }
    Ok(())
}

pub(super) fn is_clean_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        })
}

fn is_clean_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn is_group_or_world_writable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o022 != 0
}

#[cfg(not(unix))]
fn is_group_or_world_writable(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn configure_private_file(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn configure_private_file(_options: &mut OpenOptions) {}
