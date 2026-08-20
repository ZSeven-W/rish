# rish-guest-importer

`rish-guest-importer` materializes a verified OCI image into a real rootfs
inside a Linux guest. It is not a mobile-host snapshotter and does not make
iOS, Android, or OpenHarmony provide Linux filesystem privileges.

## Trust boundary

The input is a `VerifiedImageRecord` plus a `DescriptorSource` that opens a
guest-visible byte stream for each descriptor. The source may be backed by a
guest block device, guest-local CAS, or a bounded host-to-guest RPC. The
importer never assumes a mobile host CAS path is visible in the guest.

For every layer it:

1. checks the declared media type, compressed size, and SHA-256;
2. spools the verified blob, then incrementally decodes gzip;
3. checks the uncompressed SHA-256 against the recorded OCI diff-id;
4. preflights tar paths, links, whiteouts, metadata, and resource limits;
5. applies the layer only to a private staging rootfs;
6. syncs and publishes with `renameat2(RENAME_NOREPLACE)`.

Any failure discards staging and leaves the destination absent or unchanged.

## Metadata policy

Accepted entries preserve numeric uid/gid, all permission and set-id bits,
mtime (including representable PAX fractions), symlinks, hardlinks,
`SCHILY.xattr.*`, and Linux V2
file capabilities. Capability xattrs are written after ownership and mode.

Device nodes and FIFOs are rejected unless the caller passes an explicit
`PrivilegedGuestPolicy`; the Linux guest must still possess the corresponding
kernel capability. The policy never grants privileges on the mobile host.

Unsupported metadata fails closed. This includes ACL/fflags encodings, global
PAX state, GNU sparse files, LIBARCHIVE xattr encoding, Linux V1/V3 file
capabilities, PAX atime/ctime, overlayfs implementation xattrs, and SCHILY
binary PAX records that Rust `tar` cannot decode faithfully (for example a
value containing an embedded line feed). No metadata is silently skipped.

The importer must run as effective uid 0 in Linux. Non-Linux builds expose the
same API but return `UnsupportedPlatform` before opening a descriptor.
