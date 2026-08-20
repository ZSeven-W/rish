# OCI content pipeline

## Pull sequence

```text
parse reference
  → GET /v2/<repo>/manifests/<tag-or-digest>
  → authenticate and bound response size
  → verify response digest before parsing
  → choose the exact requested linux/arm64/v8 or linux/amd64 descriptor
  → fetch image manifest by digest
  → fetch config and every layer by digest
  → atomically commit verified blobs to CAS
  → acquire lease for the complete image graph
  → apply layers into an isolated staging rootfs
  → atomically publish the rootfs snapshot
```

The implementation follows the
[OCI Distribution Specification](https://github.com/opencontainers/distribution-spec/blob/main/spec.md)
and [OCI Image Specification](https://github.com/opencontainers/image-spec).

## Trust boundaries

Registry responses, manifests, image configs, tar headers, filenames, extended
attributes and layer contents are all untrusted.

The pipeline must enforce:

- a maximum manifest/index/config size before JSON parsing;
- bounded JSON nesting, value/container counts, and string allocation before
  materializing manifest or config models;
- descriptor size validation before and after download;
- digest verification over the exact downloaded bytes;
- only explicitly supported digest algorithms;
- a bounded redirect count;
- no forwarding `Authorization` to a different origin unless policy explicitly
  allows that origin;
- a maximum blob size and maximum total image size;
- a maximum layer count;
- a maximum expanded byte count and entry count;
- no absolute, parent-traversing or platform-prefix paths;
- no writes through symlinks created by an earlier tar entry;
- no device nodes, FIFOs, sockets or privileged xattrs on portable hosts;
- atomic staging so a failed pull never becomes a runnable image.

`RegistryRequest.max_response_bytes` is part of the trusted transport
contract. URLSession, OkHttp, ArkTS, or another transport must reject an
oversized `Content-Length` and stop streaming at the limit. Manifests and
configs are collected only after that bounded stream is established; layers
flow directly into the CAS. A post-download length check alone is not an
out-of-memory defense.

## Content store

Verified blobs use an OCI-compatible content-addressed layout:

```text
content/
  blobs/
    sha256/
      <64-lowercase-hex>
  pins/
    <pin-name>/
      <64-lowercase-hex>
  tmp/
    <random-upload>
```

Ingest writes to a private temporary file, calculates digest and size while
streaming, flushes it, and renames it into the final digest path. A blob only
becomes visible after all checks succeed.

Mobile defaults cap one compressed layer at 512 MiB, one pull graph at 2 GiB,
and all committed CAS blobs at 4 GiB. Unique-blob quota admission is serialized
with process-local lease and GC state; duplicate digests are charged once and a
successful GC deletion releases capacity. Existing blobs are counted on open.
Temporary ingest files and snapshots are outside this committed-byte quota, so
platform code must still monitor filesystem free space.

Every active pull owns a process-local lease containing all reachable
descriptors. Long-lived images use persistent pin markers. Garbage collection
computes:

```text
all committed blobs - union(active lease digests, persistent pins, explicit roots)
```

and removes only unreachable blobs. The current lease table is process-local;
multiple processes opening the same store require an external exclusive lock.

## Platform selection

The Full VM execution target is:

```text
os = linux
architecture = amd64
variant = absent
```

The native-offload path can also request exact `linux/arm64/v8`. Both platform
records may coexist for one multi-architecture index digest without overwriting
one another. AMD64 variants, Windows manifests, foreign layers and unsupported
compression formats are rejected rather than silently selected.

## Whiteouts

OCI layers apply in manifest order.

- `.wh.<name>` removes `<name>` from the accumulated lower rootfs.
- `.wh..wh..opq` removes existing lower entries from the containing directory
  before applying new entries from the current layer.
- Whiteout marker files are never copied into the published rootfs.

Deletion targets use already-normalized relative paths and must remain inside
the staging root. The implementation never calls a recursive deletion function
with an unresolved symlink target.

Portable snapshots intentionally strip setuid/setgid and do not materialize
Linux uid/gid, privileged xattrs, file capabilities, or device nodes on the
mobile host filesystem. They preserve ordinary permission bits and sticky
directories while normalizing directories to remain owner-readable, writable,
and searchable so later layers can be applied safely. Preflight data is
spooled beneath the app-owned snapshot directory rather than a shared global
temporary directory. A bootable VM/systemd rootfs must be unpacked inside the
Linux guest onto a Linux filesystem so those metadata have real kernel
semantics.

## Native-offload images

An image with `io.rish.offload.handler` still passes through the same digest,
size, lease and config checks. Its filesystem layers may be skipped when the
handler contract declares that they are not required, but the config and
manifest graph remain authenticated and auditable.

Unknown images are never converted into native host execution. They require a
Full VM or a successfully probed Native Linux backend.
