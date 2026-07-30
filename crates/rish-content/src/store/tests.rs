use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use super::*;

static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("rish-content-test-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn store(maximum: u64) -> (TestDirectory, ContentStore) {
    let directory = TestDirectory::new();
    let store =
        ContentStore::open(StoreConfig::new(&directory.0).with_max_blob_size(maximum)).unwrap();
    (directory, store)
}

fn store_with_quota(maximum: u64, quota: u64) -> (TestDirectory, ContentStore) {
    let directory = TestDirectory::new();
    let store = ContentStore::open(
        StoreConfig::new(&directory.0)
            .with_max_blob_size(maximum)
            .with_max_committed_bytes(quota),
    )
    .unwrap();
    (directory, store)
}

#[test]
fn default_mobile_quota_matches_the_public_constant() {
    let config = StoreConfig::new("unused");
    assert_eq!(config.max_committed_bytes, 4 * 1024 * 1024 * 1024);
    assert_eq!(config.max_committed_bytes, DEFAULT_MAX_COMMITTED_BYTES);
    assert!(config.max_blob_size <= config.max_committed_bytes);
}

#[test]
fn streams_blob_to_digest_path() {
    let (_directory, store) = store(1024);
    let descriptor = store
        .ingest(Cursor::new(
            [b"hello ".as_slice(), b"world".as_slice()].concat(),
        ))
        .unwrap();

    assert_eq!(descriptor.digest, Sha256Digest::calculate(b"hello world"));
    assert_eq!(descriptor.size, 11);
    assert_eq!(
        store.blob_path(descriptor.digest),
        store
            .root()
            .join("blobs")
            .join("sha256")
            .join(descriptor.digest.encoded())
    );
    let mut contents = Vec::new();
    store
        .open_blob(descriptor.digest)
        .unwrap()
        .read_to_end(&mut contents)
        .unwrap();
    assert_eq!(contents, b"hello world");
    store.verify(descriptor).unwrap();
}

#[test]
fn rejects_digest_mismatch_and_cleans_temporary_file() {
    let (_directory, store) = store(1024);
    let expected = BlobDescriptor::new(Sha256Digest::calculate(b"different"), 7);
    let error = store
        .ingest_bytes_verified(b"content", expected)
        .unwrap_err();

    assert!(matches!(error, StoreError::DigestMismatch { .. }));
    assert!(!store.contains(expected.digest).unwrap());
    assert_eq!(fs::read_dir(&store.inner.temporary).unwrap().count(), 0);
}

#[test]
fn rejects_size_mismatch() {
    let (_directory, store) = store(1024);
    let expected = BlobDescriptor::new(Sha256Digest::calculate(b"content"), 8);

    assert!(matches!(
        store.ingest_bytes_verified(b"content", expected),
        Err(StoreError::SizeMismatch {
            expected: 8,
            actual: 7
        })
    ));
}

#[test]
fn verified_ingest_stops_after_expected_size_plus_one() {
    let (_directory, store) = store(1024);
    let expected = BlobDescriptor::new(Sha256Digest::calculate(b"xx"), 2);

    assert!(matches!(
        store.ingest_verified(std::io::repeat(b'x'), expected),
        Err(StoreError::SizeMismatch {
            expected: 2,
            actual: 3
        })
    ));
    assert_eq!(fs::read_dir(&store.inner.temporary).unwrap().count(), 0);
}

#[test]
fn rejects_an_expected_size_above_the_store_limit_before_ingest() {
    let (_directory, store) = store(4);
    let expected = BlobDescriptor::new(Sha256Digest::calculate(b""), 5);

    assert!(matches!(
        store.ingest_verified(std::io::empty(), expected),
        Err(StoreError::BlobTooLarge {
            maximum: 4,
            observed_at_least: 5
        })
    ));
    assert_eq!(fs::read_dir(&store.inner.temporary).unwrap().count(), 0);
}

#[test]
fn enforces_maximum_size_while_streaming() {
    let (_directory, store) = store(4);
    let error = store.ingest_bytes(b"12345").unwrap_err();

    assert!(matches!(
        error,
        StoreError::BlobTooLarge {
            maximum: 4,
            observed_at_least: 5
        }
    ));
    assert_eq!(fs::read_dir(&store.inner.temporary).unwrap().count(), 0);
}

#[test]
fn pins_and_leases_are_gc_roots() {
    let (_directory, store) = store(1024);
    let pinned = store.ingest_bytes(b"pinned").unwrap();
    let leased = store.ingest_bytes(b"leased").unwrap();
    let garbage = store.ingest_bytes(b"garbage").unwrap();
    store.pin("image-v1", pinned.digest).unwrap();
    let lease = store.create_lease().unwrap();
    lease.add(leased.digest).unwrap();

    let report = store.garbage_collect(GcOptions::default()).unwrap();
    assert_eq!(report.kept_blobs, 2);
    assert_eq!(report.removed_blobs, 1);
    assert!(store.contains(pinned.digest).unwrap());
    assert!(store.contains(leased.digest).unwrap());
    assert!(!store.contains(garbage.digest).unwrap());

    drop(lease);
    assert!(store.unpin("image-v1", pinned.digest).unwrap());
    let report = store.garbage_collect(GcOptions::default()).unwrap();
    assert_eq!(report.removed_blobs, 2);
    assert!(!store.contains(pinned.digest).unwrap());
    assert!(!store.contains(leased.digest).unwrap());
}

#[test]
fn lease_ingest_marks_before_gc_can_observe_blob() {
    let (_directory, store) = store(1024);
    let lease = store.create_lease().unwrap();
    let descriptor = lease.ingest(b"protected".as_slice()).unwrap();

    let report = store.garbage_collect(GcOptions::default()).unwrap();
    assert_eq!(report.kept_blobs, 1);
    assert!(store.contains(descriptor.digest).unwrap());
}

#[test]
fn dry_run_reports_without_removing() {
    let (_directory, store) = store(1024);
    let descriptor = store.ingest_bytes(b"garbage").unwrap();

    let report = store.garbage_collect(GcOptions { dry_run: true }).unwrap();
    assert_eq!(report.removed_blobs, 1);
    assert!(store.contains(descriptor.digest).unwrap());
}

#[test]
fn rejects_traversal_in_pin_names() {
    let (_directory, store) = store(1024);
    let descriptor = store.ingest_bytes(b"content").unwrap();

    assert!(matches!(
        store.pin("../escape", descriptor.digest),
        Err(StoreError::InvalidPinName(_))
    ));
    assert!(matches!(
        store.pin("nested/name", descriptor.digest),
        Err(StoreError::InvalidPinName(_))
    ));
}

#[test]
fn repeated_ingest_reuses_verified_blob() {
    let (_directory, store) = store(1024);
    let first = store.ingest_bytes(b"same").unwrap();
    let second = store.ingest_bytes(b"same").unwrap();

    assert_eq!(first, second);
    assert_eq!(fs::read_dir(&store.inner.blobs).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&store.inner.temporary).unwrap().count(), 0);
}

#[test]
fn committed_quota_counts_unique_blobs_and_not_duplicate_digests() {
    let (_directory, store) = store_with_quota(1024, 4);
    let first = store.ingest_bytes(b"same").unwrap();
    let repeated = store.ingest_bytes(b"same").unwrap();

    assert_eq!(first, repeated);
    assert_eq!(store.max_committed_bytes(), 4);
    assert_eq!(store.committed_bytes().unwrap(), 4);
    assert!(matches!(
        store.ingest_bytes(b"x"),
        Err(StoreError::CommittedBytesQuotaExceeded {
            maximum: 4,
            committed: 4,
            attempted: 1
        })
    ));
    assert_eq!(store.committed_bytes().unwrap(), 4);
    assert_eq!(fs::read_dir(&store.inner.blobs).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&store.inner.temporary).unwrap().count(), 0);
}

#[test]
fn verified_ingest_rejects_an_unavailable_quota_before_reading() {
    struct PanicReader;

    impl Read for PanicReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            panic!("quota preflight must run before reading a verified blob")
        }
    }

    let (_directory, store) = store_with_quota(1024, 4);
    store.ingest_bytes(b"same").unwrap();
    let expected = BlobDescriptor::new(Sha256Digest::calculate(b"x"), 1);
    assert!(matches!(
        store.ingest_verified(PanicReader, expected),
        Err(StoreError::CommittedBytesQuotaExceeded {
            maximum: 4,
            committed: 4,
            attempted: 1
        })
    ));
    assert_eq!(fs::read_dir(&store.inner.temporary).unwrap().count(), 0);
}

#[test]
fn reopen_counts_existing_blobs_and_gc_releases_capacity() {
    let directory = TestDirectory::new();
    let descriptor = {
        let store = ContentStore::open(
            StoreConfig::new(&directory.0)
                .with_max_blob_size(1024)
                .with_max_committed_bytes(16),
        )
        .unwrap();
        store.ingest_bytes(b"existing").unwrap()
    };

    let store = ContentStore::open(
        StoreConfig::new(&directory.0)
            .with_max_blob_size(1024)
            .with_max_committed_bytes(4),
    )
    .unwrap();
    assert_eq!(store.committed_bytes().unwrap(), descriptor.size);
    assert_eq!(
        store
            .ingest_bytes_verified(b"existing", descriptor)
            .unwrap(),
        descriptor
    );
    assert!(matches!(
        store.ingest_bytes(b"x"),
        Err(StoreError::CommittedBytesQuotaExceeded {
            maximum: 4,
            committed: 8,
            attempted: 1
        })
    ));

    let report = store.garbage_collect(GcOptions::default()).unwrap();
    assert_eq!(report.removed_blobs, 1);
    assert_eq!(store.committed_bytes().unwrap(), 0);
    assert_eq!(store.ingest_bytes(b"next").unwrap().size, 4);
    assert_eq!(store.committed_bytes().unwrap(), 4);
}

#[test]
fn concurrent_unique_commits_cannot_oversubscribe_the_quota() {
    let (_directory, store) = store_with_quota(1024, 4);
    let barrier = Arc::new(Barrier::new(3));
    let handles = [b"aaaa".as_slice(), b"bbbb".as_slice()].map(|payload| {
        let store = store.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            store.ingest_bytes(payload)
        })
    });
    barrier.wait();

    let results = handles.map(|handle| handle.join().unwrap());
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(StoreError::CommittedBytesQuotaExceeded { .. })))
            .count(),
        1
    );
    assert_eq!(store.committed_bytes().unwrap(), 4);
    assert_eq!(fs::read_dir(&store.inner.blobs).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&store.inner.temporary).unwrap().count(), 0);
}

#[test]
fn concurrent_duplicate_commits_are_charged_once() {
    const WORKERS: usize = 8;
    let (_directory, store) = store_with_quota(1024, 4);
    let barrier = Arc::new(Barrier::new(WORKERS + 1));
    let handles = (0..WORKERS)
        .map(|_| {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                store.ingest_bytes(b"same")
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();

    let expected = BlobDescriptor::new(Sha256Digest::calculate(b"same"), 4);
    for handle in handles {
        assert_eq!(handle.join().unwrap().unwrap(), expected);
    }
    assert_eq!(store.committed_bytes().unwrap(), 4);
    assert_eq!(fs::read_dir(&store.inner.blobs).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&store.inner.temporary).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn rejects_a_symlink_as_the_store_root() {
    use std::os::unix::fs::symlink;

    let sandbox = TestDirectory::new();
    let real = sandbox.0.join("real");
    let link = sandbox.0.join("link");
    fs::create_dir(&real).unwrap();
    symlink(&real, &link).unwrap();

    assert!(matches!(
        ContentStore::open(StoreConfig::new(link)),
        Err(StoreError::UnsafeFilesystemEntry { .. })
    ));
}

#[cfg(unix)]
#[test]
fn refuses_to_open_a_blob_replaced_by_a_symlink() {
    use std::os::unix::fs::symlink;

    let (directory, store) = store(1024);
    let descriptor = store.ingest_bytes(b"trusted").unwrap();
    let outside = directory.0.join("outside");
    fs::write(&outside, b"forged").unwrap();
    let blob = store.blob_path(descriptor.digest);
    fs::remove_file(&blob).unwrap();
    symlink(&outside, &blob).unwrap();

    assert!(store.open_blob(descriptor.digest).is_err());
    assert!(store.verify(descriptor).is_err());
    assert!(matches!(
        store.contains(descriptor.digest),
        Err(StoreError::UnsafeFilesystemEntry { .. })
    ));
}
