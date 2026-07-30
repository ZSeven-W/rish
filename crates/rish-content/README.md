# rish-content

`rish-content` is a streaming SHA-256 content-addressed store for OCI blobs.
Incoming bytes are written to a private temporary file, hashed and size-checked,
then atomically promoted to `blobs/sha256/<hex>`.

## Committed-byte quota

`StoreConfig::new` defaults to a 4 GiB global committed-blob budget. Set a
different mobile-app policy with `StoreConfig::with_max_committed_bytes`.

- Existing committed blobs are counted when the store opens.
- A unique blob is admitted while holding the process-local store state lock.
- Re-ingesting the same digest does not charge it twice.
- Successful garbage collection releases the removed blob's charge.
- Lowering the quota below existing usage does not prevent opening, reading, or
  garbage-collecting the store. New unique commits fail with
  `StoreError::CommittedBytesQuotaExceeded` until enough capacity is reclaimed.

The committed quota excludes temporary ingest files and materialized snapshots.
`max_blob_size` independently bounds one temporary ingest. Platform code should
also monitor free space and keep the store in app-owned storage.

Clones of one `ContentStore` share quota accounting and serialization.
Independently opening the same root, especially from another process, still
requires external exclusion.
