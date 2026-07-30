use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use sha2::{Digest as _, Sha256};

use crate::digest::Sha256Digest;
use crate::error::{Result, StoreError};
use crate::filesystem::{
    create_temporary_file, ensure_managed_directory, ensure_optional_managed_directory,
    ensure_regular_marker, open_regular_file_no_follow, prepare_store_root,
    remove_directory_if_empty, set_private_file_permissions, sync_directory,
};
use crate::quota::scan_committed_blobs;

pub const DEFAULT_MAX_BLOB_SIZE: u64 = 4 * 1024 * 1024 * 1024;
/// Conservative process-local CAS budget suitable for mobile app storage.
pub const DEFAULT_MAX_COMMITTED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const BUFFER_SIZE: usize = 64 * 1024;
const MAX_PIN_NAME_LEN: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreConfig {
    pub root: PathBuf,
    pub max_blob_size: u64,
    pub max_committed_bytes: u64,
}

impl StoreConfig {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_blob_size: DEFAULT_MAX_BLOB_SIZE,
            max_committed_bytes: DEFAULT_MAX_COMMITTED_BYTES,
        }
    }

    #[must_use]
    pub const fn with_max_blob_size(mut self, maximum: u64) -> Self {
        self.max_blob_size = maximum;
        self
    }

    #[must_use]
    pub const fn with_max_committed_bytes(mut self, maximum: u64) -> Self {
        self.max_committed_bytes = maximum;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlobDescriptor {
    pub digest: Sha256Digest,
    pub size: u64,
}

impl BlobDescriptor {
    #[must_use]
    pub const fn new(digest: Sha256Digest, size: u64) -> Self {
        Self { digest, size }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pin {
    pub name: String,
    pub digest: Sha256Digest,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GcOptions {
    pub dry_run: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GcReport {
    pub marked_digests: u64,
    pub kept_blobs: u64,
    pub removed_blobs: u64,
    pub reclaimed_bytes: u64,
}

/// A handle to one content store.
///
/// Clones share leases and operation serialization. Independently opening the
/// same root (especially from another process) requires external exclusion so
/// that GC cannot race an unrecorded lease.
#[derive(Clone)]
pub struct ContentStore {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    blobs: PathBuf,
    temporary: PathBuf,
    pins: PathBuf,
    max_blob_size: u64,
    max_committed_bytes: u64,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    next_lease_id: u64,
    leases: BTreeMap<u64, BTreeSet<Sha256Digest>>,
    committed_blobs: BTreeMap<Sha256Digest, u64>,
    committed_bytes: u64,
}

impl ContentStore {
    pub fn open(config: StoreConfig) -> Result<Self> {
        prepare_store_root(&config.root)?;
        let root = fs::canonicalize(&config.root)?;
        ensure_managed_directory(&root, &root.join("blobs"))?;
        let blobs = root.join("blobs").join(Sha256Digest::ALGORITHM);
        ensure_managed_directory(&root, &blobs)?;
        let temporary = root.join("tmp");
        ensure_managed_directory(&root, &temporary)?;
        let pins = root.join("pins");
        ensure_managed_directory(&root, &pins)?;
        let (committed_blobs, committed_bytes) = scan_committed_blobs(&blobs)?;

        Ok(Self {
            inner: Arc::new(Inner {
                root,
                blobs,
                temporary,
                pins,
                max_blob_size: config.max_blob_size,
                max_committed_bytes: config.max_committed_bytes,
                state: Mutex::new(State {
                    committed_blobs,
                    committed_bytes,
                    ..State::default()
                }),
            }),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    #[must_use]
    pub fn max_blob_size(&self) -> u64 {
        self.inner.max_blob_size
    }

    #[must_use]
    pub fn max_committed_bytes(&self) -> u64 {
        self.inner.max_committed_bytes
    }

    pub fn committed_bytes(&self) -> Result<u64> {
        Ok(self.lock_state()?.committed_bytes)
    }

    /// Returns the immutable path for a validated digest.
    #[must_use]
    pub fn blob_path(&self, digest: Sha256Digest) -> PathBuf {
        self.inner.blobs.join(digest.encoded())
    }

    pub fn contains(&self, digest: Sha256Digest) -> Result<bool> {
        match fs::symlink_metadata(self.blob_path(digest)) {
            Ok(metadata) if metadata.file_type().is_file() => Ok(true),
            Ok(_) => Err(StoreError::UnsafeFilesystemEntry {
                path: self.blob_path(digest),
                reason: "blob is not a regular file".to_owned(),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub fn open_blob(&self, digest: Sha256Digest) -> Result<File> {
        let _state = self.lock_state()?;
        let path = self.blob_path(digest);
        let file = match open_regular_file_no_follow(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(StoreError::BlobNotFound(digest));
            }
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() {
            return Err(StoreError::UnsafeFilesystemEntry {
                path,
                reason: "blob is not a regular file".to_owned(),
            });
        }
        Ok(file)
    }

    pub fn ingest<R: Read>(&self, reader: R) -> Result<BlobDescriptor> {
        self.ingest_inner(reader, None, None)
    }

    pub fn ingest_verified<R: Read>(
        &self,
        reader: R,
        expected: BlobDescriptor,
    ) -> Result<BlobDescriptor> {
        self.ingest_inner(reader, Some(expected), None)
    }

    pub fn ingest_bytes(&self, bytes: &[u8]) -> Result<BlobDescriptor> {
        self.ingest(bytes)
    }

    pub fn ingest_bytes_verified(
        &self,
        bytes: &[u8],
        expected: BlobDescriptor,
    ) -> Result<BlobDescriptor> {
        self.ingest_verified(bytes, expected)
    }

    pub fn verify(&self, descriptor: BlobDescriptor) -> Result<()> {
        let _state = self.lock_state()?;
        verify_blob_file(&self.blob_path(descriptor.digest), descriptor)
    }

    /// Adds a persistent GC root.
    ///
    /// A name can contain ASCII letters, digits, `.`, `_`, and `-`. Each
    /// `(name, digest)` pair is an independent pin.
    pub fn pin(&self, name: &str, digest: Sha256Digest) -> Result<()> {
        validate_pin_name(name)?;
        let _state = self.lock_state()?;
        if !blob_is_regular_file(&self.blob_path(digest))? {
            return Err(StoreError::BlobNotFound(digest));
        }

        let pin_directory = self.inner.pins.join(name);
        ensure_managed_directory(&self.inner.pins, &pin_directory)?;
        let path = pin_directory.join(digest.encoded());
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                set_private_file_permissions(&file)?;
                file.sync_all()?;
                sync_directory(&pin_directory)?;
                sync_directory(&self.inner.pins)?;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                ensure_regular_marker(&path)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn unpin(&self, name: &str, digest: Sha256Digest) -> Result<bool> {
        validate_pin_name(name)?;
        let _state = self.lock_state()?;
        let directory = self.inner.pins.join(name);
        ensure_optional_managed_directory(&self.inner.pins, &directory)?;
        let path = directory.join(digest.encoded());
        let removed = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                fs::remove_file(&path)?;
                true
            }
            Ok(_) => {
                return Err(StoreError::UnsafeFilesystemEntry {
                    path,
                    reason: "pin marker is not a regular file".to_owned(),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };

        if removed {
            remove_directory_if_empty(&directory)?;
            sync_directory(&self.inner.pins)?;
        }
        Ok(removed)
    }

    pub fn pins(&self) -> Result<Vec<Pin>> {
        let _state = self.lock_state()?;
        read_pins(&self.inner.pins)
    }

    pub fn create_lease(&self) -> Result<Lease> {
        let mut state = self.lock_state()?;
        state.next_lease_id = state.next_lease_id.wrapping_add(1);
        if state.next_lease_id == 0 {
            state.next_lease_id = 1;
        }
        let id = state.next_lease_id;
        state.leases.insert(id, BTreeSet::new());
        Ok(Lease {
            store: self.clone(),
            id,
            released: false,
        })
    }

    pub fn garbage_collect(&self, options: GcOptions) -> Result<GcReport> {
        self.garbage_collect_with_roots(std::iter::empty(), options)
    }

    pub fn garbage_collect_with_roots<I>(
        &self,
        additional_roots: I,
        options: GcOptions,
    ) -> Result<GcReport>
    where
        I: IntoIterator<Item = Sha256Digest>,
    {
        let mut state = self.lock_state()?;
        let mut marked: BTreeSet<_> = additional_roots.into_iter().collect();
        marked.extend(
            read_pins(&self.inner.pins)?
                .into_iter()
                .map(|pin| pin.digest),
        );
        for digests in state.leases.values() {
            marked.extend(digests.iter().copied());
        }

        let mut report = GcReport {
            marked_digests: u64::try_from(marked.len()).unwrap_or(u64::MAX),
            ..GcReport::default()
        };

        for entry in fs::read_dir(&self.inner.blobs)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file() {
                return Err(StoreError::UnsafeFilesystemEntry {
                    path,
                    reason: "unexpected non-file in blob directory".to_owned(),
                });
            }
            let file_name = entry.file_name();
            let encoded = file_name
                .to_str()
                .ok_or_else(|| StoreError::UnsafeFilesystemEntry {
                    path: path.clone(),
                    reason: "blob filename is not UTF-8".to_owned(),
                })?;
            let digest = Sha256Digest::from_encoded(encoded).map_err(|error| {
                StoreError::UnsafeFilesystemEntry {
                    path: path.clone(),
                    reason: format!("invalid blob filename: {error}"),
                }
            })?;

            if marked.contains(&digest) {
                report.kept_blobs = report.kept_blobs.saturating_add(1);
            } else {
                report.removed_blobs = report.removed_blobs.saturating_add(1);
                report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(metadata.len());
                if !options.dry_run {
                    fs::remove_file(path)?;
                    state.remove_committed_blob(digest);
                }
            }
        }
        if report.removed_blobs > 0 && !options.dry_run {
            sync_directory(&self.inner.blobs)?;
        }
        Ok(report)
    }

    fn ingest_inner<R: Read>(
        &self,
        reader: R,
        expected: Option<BlobDescriptor>,
        lease_id: Option<u64>,
    ) -> Result<BlobDescriptor> {
        if let Some(expected) = expected {
            if expected.size > self.inner.max_blob_size {
                return Err(StoreError::BlobTooLarge {
                    maximum: self.inner.max_blob_size,
                    observed_at_least: expected.size,
                });
            }
            self.preflight_committed_quota(expected)?;
        }

        let (temporary_path, mut temporary_file) = create_temporary_file(&self.inner.temporary)?;
        let temporary_guard = TemporaryGuard(&temporary_path);
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; BUFFER_SIZE];
        let expected_limit = expected.map_or(self.inner.max_blob_size, |value| value.size);
        let mut reader = reader.take(expected_limit.saturating_add(1));

        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            let count_u64 = u64::try_from(count).expect("buffer length fits in u64");
            total = total
                .checked_add(count_u64)
                .ok_or(StoreError::BlobTooLarge {
                    maximum: self.inner.max_blob_size,
                    observed_at_least: u64::MAX,
                })?;
            if total > self.inner.max_blob_size {
                return Err(StoreError::BlobTooLarge {
                    maximum: self.inner.max_blob_size,
                    observed_at_least: total,
                });
            }
            if let Some(expected) = expected {
                if total > expected.size {
                    return Err(StoreError::SizeMismatch {
                        expected: expected.size,
                        actual: total,
                    });
                }
            }
            hasher.update(&buffer[..count]);
            temporary_file.write_all(&buffer[..count])?;
        }

        let digest = Sha256Digest::from_bytes(hasher.finalize().into());
        let descriptor = BlobDescriptor::new(digest, total);
        if let Some(expected) = expected {
            if expected.size != total {
                return Err(StoreError::SizeMismatch {
                    expected: expected.size,
                    actual: total,
                });
            }
            if expected.digest != digest {
                return Err(StoreError::DigestMismatch {
                    expected: expected.digest,
                    actual: digest,
                });
            }
        }

        temporary_file.sync_all()?;
        drop(temporary_file);
        self.commit_temporary(&temporary_path, descriptor, lease_id)?;
        drop(temporary_guard);
        Ok(descriptor)
    }

    fn commit_temporary(
        &self,
        temporary_path: &Path,
        descriptor: BlobDescriptor,
        lease_id: Option<u64>,
    ) -> Result<()> {
        let mut state = self.lock_state()?;
        if let Some(id) = lease_id {
            if !state.leases.contains_key(&id) {
                return Err(StoreError::LeaseReleased);
            }
        }

        let final_path = self.blob_path(descriptor.digest);
        match fs::symlink_metadata(&final_path) {
            Ok(_) => {
                verify_blob_file(&final_path, descriptor)?;
                state.observe_committed_blob(descriptor);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let needs_accounting = !state.committed_blobs.contains_key(&descriptor.digest);
                if needs_accounting {
                    state.ensure_committed_capacity(
                        self.inner.max_committed_bytes,
                        descriptor.size,
                    )?;
                }
                match fs::rename(temporary_path, &final_path) {
                    Ok(()) => {
                        state.observe_committed_blob(descriptor);
                        sync_directory(&self.inner.blobs)?;
                    }
                    Err(rename_error) => {
                        if fs::symlink_metadata(&final_path).is_ok() {
                            verify_blob_file(&final_path, descriptor)?;
                            state.observe_committed_blob(descriptor);
                        } else {
                            return Err(rename_error.into());
                        }
                    }
                }
            }
            Err(error) => return Err(error.into()),
        }

        if let Some(id) = lease_id {
            state
                .leases
                .get_mut(&id)
                .ok_or(StoreError::LeaseReleased)?
                .insert(descriptor.digest);
        }
        Ok(())
    }

    fn preflight_committed_quota(&self, descriptor: BlobDescriptor) -> Result<()> {
        let state = self.lock_state()?;
        if state.committed_blobs.contains_key(&descriptor.digest) {
            Ok(())
        } else {
            state.ensure_committed_capacity(self.inner.max_committed_bytes, descriptor.size)
        }
    }

    fn add_to_lease(&self, lease_id: u64, digest: Sha256Digest) -> Result<()> {
        let mut state = self.lock_state()?;
        if !blob_is_regular_file(&self.blob_path(digest))? {
            return Err(StoreError::BlobNotFound(digest));
        }
        state
            .leases
            .get_mut(&lease_id)
            .ok_or(StoreError::LeaseReleased)?
            .insert(digest);
        Ok(())
    }

    fn remove_from_lease(&self, lease_id: u64, digest: Sha256Digest) -> Result<bool> {
        let mut state = self.lock_state()?;
        Ok(state
            .leases
            .get_mut(&lease_id)
            .ok_or(StoreError::LeaseReleased)?
            .remove(&digest))
    }

    fn release_lease(&self, lease_id: u64) -> Result<()> {
        let mut state = self.lock_state()?;
        state.leases.remove(&lease_id);
        Ok(())
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, State>> {
        self.inner
            .state
            .lock()
            .map_err(|_| StoreError::LockPoisoned)
    }
}

impl State {
    fn ensure_committed_capacity(&self, maximum: u64, attempted: u64) -> Result<()> {
        let projected = self.committed_bytes.checked_add(attempted);
        if projected.is_none_or(|projected| projected > maximum) {
            return Err(StoreError::CommittedBytesQuotaExceeded {
                maximum,
                committed: self.committed_bytes,
                attempted,
            });
        }
        Ok(())
    }

    fn observe_committed_blob(&mut self, descriptor: BlobDescriptor) {
        if self
            .committed_blobs
            .insert(descriptor.digest, descriptor.size)
            .is_none()
        {
            self.committed_bytes = self.committed_bytes.saturating_add(descriptor.size);
        }
    }

    fn remove_committed_blob(&mut self, digest: Sha256Digest) {
        if let Some(size) = self.committed_blobs.remove(&digest) {
            self.committed_bytes = self.committed_bytes.saturating_sub(size);
        }
    }
}

pub struct Lease {
    store: ContentStore,
    id: u64,
    released: bool,
}

impl Lease {
    pub fn add(&self, digest: Sha256Digest) -> Result<()> {
        if self.released {
            return Err(StoreError::LeaseReleased);
        }
        self.store.add_to_lease(self.id, digest)
    }

    pub fn remove(&self, digest: Sha256Digest) -> Result<bool> {
        if self.released {
            return Err(StoreError::LeaseReleased);
        }
        self.store.remove_from_lease(self.id, digest)
    }

    pub fn ingest<R: Read>(&self, reader: R) -> Result<BlobDescriptor> {
        if self.released {
            return Err(StoreError::LeaseReleased);
        }
        self.store.ingest_inner(reader, None, Some(self.id))
    }

    pub fn ingest_verified<R: Read>(
        &self,
        reader: R,
        expected: BlobDescriptor,
    ) -> Result<BlobDescriptor> {
        if self.released {
            return Err(StoreError::LeaseReleased);
        }
        self.store
            .ingest_inner(reader, Some(expected), Some(self.id))
    }

    pub fn release(mut self) -> Result<()> {
        if !self.released {
            self.store.release_lease(self.id)?;
            self.released = true;
        }
        Ok(())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.store.release_lease(self.id);
            self.released = true;
        }
    }
}

fn validate_pin_name(name: &str) -> Result<()> {
    let valid_length = !name.is_empty() && name.len() <= MAX_PIN_NAME_LEN;
    let valid_characters = name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !valid_length || !valid_characters || matches!(name, "." | "..") {
        return Err(StoreError::InvalidPinName(name.to_owned()));
    }
    Ok(())
}

fn read_pins(root: &Path) -> Result<Vec<Pin>> {
    let mut pins = Vec::new();
    for directory in fs::read_dir(root)? {
        let directory = directory?;
        let directory_path = directory.path();
        let metadata = fs::symlink_metadata(&directory_path)?;
        if !metadata.file_type().is_dir() {
            return Err(StoreError::CorruptPin {
                path: directory_path,
                reason: "pin name entry is not a directory".to_owned(),
            });
        }
        let name = directory
            .file_name()
            .to_str()
            .ok_or_else(|| StoreError::CorruptPin {
                path: directory.path(),
                reason: "pin name is not UTF-8".to_owned(),
            })?
            .to_owned();
        validate_pin_name(&name).map_err(|error| StoreError::CorruptPin {
            path: directory.path(),
            reason: error.to_string(),
        })?;

        for marker in fs::read_dir(&directory_path)? {
            let marker = marker?;
            let path = marker.path();
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file() {
                return Err(StoreError::CorruptPin {
                    path,
                    reason: "pin marker is not a regular file".to_owned(),
                });
            }
            let encoded = marker
                .file_name()
                .to_str()
                .ok_or_else(|| StoreError::CorruptPin {
                    path: marker.path(),
                    reason: "pin digest is not UTF-8".to_owned(),
                })?
                .to_owned();
            let digest =
                Sha256Digest::from_encoded(&encoded).map_err(|error| StoreError::CorruptPin {
                    path: marker.path(),
                    reason: error.to_string(),
                })?;
            pins.push(Pin {
                name: name.clone(),
                digest,
            });
        }
    }
    pins.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.digest.cmp(&right.digest))
    });
    Ok(pins)
}

fn verify_blob_file(path: &Path, descriptor: BlobDescriptor) -> Result<()> {
    let file = match open_regular_file_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(StoreError::BlobNotFound(descriptor.digest));
        }
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(StoreError::UnsafeFilesystemEntry {
            path: path.to_owned(),
            reason: "blob is not a regular file".to_owned(),
        });
    }
    if metadata.len() != descriptor.size {
        return Err(StoreError::CorruptBlob {
            digest: descriptor.digest,
            reason: format!(
                "expected {} bytes, found {}",
                descriptor.size,
                metadata.len()
            ),
        });
    }

    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; BUFFER_SIZE];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual = Sha256Digest::from_bytes(hasher.finalize().into());
    if actual != descriptor.digest {
        return Err(StoreError::CorruptBlob {
            digest: descriptor.digest,
            reason: format!("content hashes to {actual}"),
        });
    }
    Ok(())
}

fn blob_is_regular_file(path: &Path) -> Result<bool> {
    match open_regular_file_no_follow(path) {
        Ok(file) if file.metadata()?.file_type().is_file() => Ok(true),
        Ok(_) => Err(StoreError::UnsafeFilesystemEntry {
            path: path.to_owned(),
            reason: "blob is not a regular file".to_owned(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

struct TemporaryGuard<'a>(&'a Path);

impl Drop for TemporaryGuard<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}

#[cfg(test)]
mod tests;
