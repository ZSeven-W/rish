//! Deterministic FAT16 root-disk image builder and extractor for the guest
//! overlay (see docs/guest-root-disk.md). Compiled on demand by
//! build-root-disk.sh / test-root-disk.sh with the host rustc, like
//! tools/pack-newc.rs; no third-party dependencies.
//!
//! Usage:
//!   mk-root-disk build   OVERLAY_DIR OUTPUT_IMG
//!   mk-root-disk extract IMG DEST_DIR
//!
//! The builder walks OVERLAY_DIR, rejects symlinks, special files, and unsafe
//! names, and writes a fixed 4 MiB FAT16 (VFAT with long file names) image.
//! Every timestamp is the pinned 2020-01-01 constant, cluster allocation is
//! sequential after a deterministic sort, and the serial/label are constants,
//! so equal input trees produce byte-identical images. FAT stores no file
//! uids and the pinned timestamp fixes every mtime, so uid/mtime are fixed.
//! The extractor parses the image back and materialises the tree, which is
//! how tests verify content readability without mounting.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const SECTOR_BYTES: usize = 512;
const IMAGE_SECTORS: u32 = 8192; // 4 MiB
const RESERVED_SECTORS: u32 = 4;
const NUM_FATS: u32 = 2;
const ROOT_ENTRIES: u32 = 512;
const SECTORS_PER_CLUSTER: u32 = 1;
const FAT_SIZE_SECTORS: u32 = 33;
const ROOT_DIR_SECTORS: u32 = ROOT_ENTRIES * 32 / SECTOR_BYTES as u32; // 32
const DATA_START_SECTOR: u32 = RESERVED_SECTORS + NUM_FATS * FAT_SIZE_SECTORS + ROOT_DIR_SECTORS; // 102
const CLUSTER_COUNT: u32 = IMAGE_SECTORS - DATA_START_SECTOR; // 8090
const FAT1_SECTOR: u32 = RESERVED_SECTORS;
const ROOT_DIR_SECTOR: u32 = RESERVED_SECTORS + NUM_FATS * FAT_SIZE_SECTORS; // 70
const EOC: u16 = 0xFFFF;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LFN: u8 = 0x0F;
const PINNED_DATE: u16 = 0x5021; // 2020-01-01
const PINNED_TIME: u16 = 0x0000;
const VOLUME_SERIAL: u32 = 0x4849_5352; // little-endian bytes read "RISH"
const MAX_NAME_CHARS: usize = 255;
const SHORT_LEGAL: &[u8] = b"_-$%'@~!(){}^#&";

type Result<T> = io::Result<T>;

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn die(message: &str) -> ! {
    eprintln!("mk-root-disk: {message}");
    std::process::exit(1);
}

// ---------------------------------------------------------------------------
// Input tree
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Node {
    name: String,
    kind: Kind,
    short: Option<[u8; 11]>,
    needs_lfn: bool,
}

#[derive(Debug)]
enum Kind {
    File(Vec<u8>),
    Directory(Vec<Node>),
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(invalid_data(format!(
            "overlay path has an unsafe component: {name:?}"
        )));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(invalid_data(format!("file name exceeds the VFAT limit: {name}")));
    }
    for ch in name.chars() {
        let code = ch as u32;
        if code < 0x20 || matches!(ch, '"' | '*' | '/' | ':' | '<' | '>' | '?' | '\\' | '|') {
            return Err(invalid_data(format!(
                "file name contains a character VFAT cannot store: {name:?}"
            )));
        }
    }
    Ok(())
}

fn new_node(name: String, kind: Kind) -> Node {
    Node {
        name,
        kind,
        short: None,
        needs_lfn: true,
    }
}

fn read_tree(dir: &Path, into: &mut Vec<Node>) -> Result<()> {
    let mut children = fs::read_dir(dir)?
        .map(|result| result.map(|entry| entry.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    children.sort_by(|left, right| {
        use std::os::unix::ffi::OsStrExt;
        left.as_os_str().as_bytes().cmp(right.as_os_str().as_bytes())
    });
    for name_os in children {
        let name = name_os
            .to_str()
            .ok_or_else(|| invalid_data(format!("file name is not UTF-8: {name_os:?}")))?
            .to_owned();
        validate_name(&name)?;
        let path = dir.join(&name);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(invalid_data(format!(
                "symlinks are not allowed in the overlay: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            let mut sub = Vec::new();
            read_tree(&path, &mut sub)?;
            into.push(new_node(name, Kind::Directory(sub)));
        } else if metadata.is_file() {
            let bytes = fs::read(&path)?;
            into.push(new_node(name, Kind::File(bytes)));
        } else {
            return Err(invalid_data(format!(
                "only regular files and directories are allowed in the overlay: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Short names and long file names
// ---------------------------------------------------------------------------

fn short_candidate(name: &str) -> Option<[u8; 11]> {
    let bytes = name.as_bytes();
    let (base, ext) = match bytes.iter().rposition(|byte| *byte == b'.') {
        Some(index) if index > 0 && index + 1 < bytes.len() => {
            (&bytes[..index], &bytes[index + 1..])
        }
        _ if bytes.contains(&b'.') => return None, // dotfiles and trailing dots
        _ => (bytes, &b""[..]),
    };
    if base.contains(&b'.') || base.len() > 8 || ext.len() > 3 {
        return None;
    }
    let sanitize = |part: &[u8]| -> Option<Vec<u8>> {
        part.iter()
            .map(|byte| {
                let byte = byte.to_ascii_uppercase();
                if byte.is_ascii_alphanumeric() || SHORT_LEGAL.contains(&byte) {
                    Some(byte)
                } else {
                    None
                }
            })
            .collect()
    };
    let base = sanitize(base)?;
    let ext = sanitize(ext)?;
    let mut short = [b' '; 11];
    short[..base.len()].copy_from_slice(&base);
    short[8..8 + ext.len()].copy_from_slice(&ext);
    Some(short)
}

fn canonical_83(short: &[u8; 11]) -> String {
    let base = String::from_utf8_lossy(&short[..8]).trim_end().to_owned();
    let ext = String::from_utf8_lossy(&short[8..]).trim_end().to_owned();
    if ext.is_empty() { base } else { format!("{base}.{ext}") }
}

fn generic_short(counter: u32) -> [u8; 11] {
    let mut short = [b' '; 11];
    short[..8].copy_from_slice(format!("RISH{counter:04}").as_bytes());
    short
}

/// Assigns deterministic short names and LFN flags to every node. The generic
/// counter is global so generated names never collide across directories.
fn assign_shorts(children: &mut [Node], generic_counter: &mut u32) {
    let mut used = BTreeSet::new();
    for child in children.iter_mut() {
        let natural = short_candidate(&child.name);
        let (short, needs_lfn) = match natural {
            Some(short) if canonical_83(&short) == child.name && !used.contains(&short) => {
                (short, false)
            }
            _ => {
                let mut short = generic_short(*generic_counter);
                *generic_counter += 1;
                while used.contains(&short) {
                    short = generic_short(*generic_counter);
                    *generic_counter += 1;
                }
                (short, true)
            }
        };
        used.insert(short);
        child.short = Some(short);
        child.needs_lfn = needs_lfn;
    }
    for child in children.iter_mut() {
        if let Kind::Directory(sub) = &mut child.kind {
            assign_shorts(sub, generic_counter);
        }
    }
}

fn lfn_chunks(name: &str) -> Vec<[u16; 13]> {
    let utf16: Vec<u16> = name.encode_utf16().collect();
    let chunks: Vec<[u16; 13]> = utf16
        .chunks(13)
        .map(|chunk| {
            // Padding follows the convention the Linux fat driver itself
            // writes (observed in guest-created entries): the name, then one
            // 0x0000 terminator, then 0xFFFF filler. The reader converts code
            // units up to the first NUL, so plain 0xFFFF padding would
            // surface as literal '?' characters in every long name. Chunks
            // are built from the start of the name, so only the final
            // (flagged) slot can ever carry padding.
            let mut fixed = [0xFFFF_u16; 13];
            fixed[..chunk.len()].copy_from_slice(chunk);
            if chunk.len() < fixed.len() {
                fixed[chunk.len()] = 0x0000;
            }
            fixed
        })
        .collect();
    if chunks.is_empty() {
        let mut empty = [0xFFFF_u16; 13];
        empty[0] = 0x0000;
        vec![empty]
    } else {
        chunks
    }
}

/// The VFAT long-name checksum over the 8.3 short entry, per the on-disk
/// format: rotate the accumulator right (bit 0 into bit 7), then add the next
/// name byte, mod 256. This is the algorithm Linux (fs/fat/dir.c) and Windows
/// validate strictly; macOS does not validate it, which is why the previous
/// left-rotating variant passed the hdiutil read-back while every Linux vfat
/// mount silently dropped the long names.
fn lfn_checksum(short: &[u8; 11]) -> u8 {
    short
        .iter()
        .fold(0_u8, |sum, byte| sum.rotate_right(1).wrapping_add(*byte))
}

// ---------------------------------------------------------------------------
// Allocation and image assembly
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Allocated {
    first_cluster: u32,
    cluster_count: u32,
}

fn node_children(node: &Node) -> &[Node] {
    match &node.kind {
        Kind::Directory(children) => children,
        Kind::File(_) => &[],
    }
}

fn entries_for(node: &Node) -> usize {
    let lfn_entries = lfn_chunks(&node.name).len();
    match &node.kind {
        Kind::File(_) => 1 + lfn_entries,
        Kind::Directory(children) => {
            2 + children
                .iter()
                .map(|child| 1 + lfn_chunks(&child.name).len())
                .sum::<usize>()
        }
    }
}

fn plan_all(
    node: &Node,
    parent_path: &Path,
    allocations: &mut Vec<(PathBuf, Allocated)>,
    next: &mut u32,
) -> Result<()> {
    let mut path = parent_path.to_path_buf();
    if !node.name.is_empty() {
        path.push(&node.name);
    }
    let (size_bytes, is_dir) = match &node.kind {
        Kind::File(bytes) => (bytes.len(), false),
        Kind::Directory(_) => (entries_for(node) * 32, true),
    };
    let cluster_count = if size_bytes == 0 {
        0
    } else {
        (size_bytes as u32).div_ceil(SECTOR_BYTES as u32).max(if is_dir { 1 } else { 0 })
    };
    let first_cluster = *next;
    *next = first_cluster
        .checked_add(cluster_count)
        .ok_or_else(|| invalid_data("cluster counter overflow"))?;
    if *next > CLUSTER_COUNT + 2 {
        return Err(invalid_data(format!(
            "overlay content exceeds the {} MiB root disk ({CLUSTER_COUNT} clusters)",
            IMAGE_SECTORS * SECTOR_BYTES as u32 / 1024 / 1024
        )));
    }
    if let Kind::Directory(children) = &node.kind {
        for child in children {
            plan_all(child, &path, allocations, next)?;
        }
    }
    allocations.push((
        path,
        Allocated {
            first_cluster,
            cluster_count,
        },
    ));
    Ok(())
}

fn lookup<'a>(node: &'a Node, path: &Path) -> Option<&'a Node> {
    let mut current = node;
    for component in path.components() {
        let name = component.as_os_str().to_str()?;
        current = node_children(current).iter().find(|child| child.name == name)?;
    }
    Some(current)
}

fn cluster_offset(cluster: u32) -> usize { (DATA_START_SECTOR + (cluster - 2) * SECTORS_PER_CLUSTER) as usize * SECTOR_BYTES }

fn build_image(root: &Node) -> Result<Vec<u8>> {
    let mut allocations = Vec::new();
    let mut next = 2_u32;
    for child in node_children(root) {
        plan_all(child, Path::new(""), &mut allocations, &mut next)?;
    }

    let mut image = vec![0_u8; IMAGE_SECTORS as usize * SECTOR_BYTES];
    image[0..SECTOR_BYTES].copy_from_slice(&boot_sector());
    write_fats(&mut image, &allocations);

    let root_entries = dir_entries(root, 0, 0, &allocations, Path::new(""))?;
    if root_entries.len() > ROOT_DIR_SECTORS as usize * SECTOR_BYTES {
        return Err(invalid_data("overlay exceeds the FAT root directory entry budget"));
    }
    let root_offset = ROOT_DIR_SECTOR as usize * SECTOR_BYTES;
    image[root_offset..root_offset + root_entries.len()].copy_from_slice(&root_entries);

    for (path, allocated) in &allocations {
        let node = lookup(root, path)
            .ok_or_else(|| invalid_data("allocation path does not match the tree"))?;
        let parent_cluster = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .and_then(|p| allocations.iter().find(|(c, _)| *c == p))
            .map(|(_, a)| a.first_cluster)
            .unwrap_or(0);
        let content = match &node.kind {
            Kind::File(bytes) => bytes.clone(),
            Kind::Directory(_) => dir_entries(
                node,
                allocated.first_cluster,
                parent_cluster,
                &allocations,
                path,
            )?,
        };
        if allocated.cluster_count == 0 {
            continue;
        }
        let offset = cluster_offset(allocated.first_cluster);
        let end = offset + content.len();
        if end > image.len() {
            return Err(invalid_data("cluster content exceeds the image"));
        }
        image[offset..end].copy_from_slice(&content);
    }
    Ok(image)
}

fn boot_sector() -> [u8; SECTOR_BYTES] {
    let mut sector = [0_u8; SECTOR_BYTES];
    sector[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
    sector[3..11].copy_from_slice(b"RISH1.0 ");
    sector[11..13].copy_from_slice(&(SECTOR_BYTES as u16).to_le_bytes());
    sector[13] = SECTORS_PER_CLUSTER as u8;
    sector[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
    sector[16] = NUM_FATS as u8;
    sector[17..19].copy_from_slice(&(ROOT_ENTRIES as u16).to_le_bytes());
    sector[19..21].copy_from_slice(&(IMAGE_SECTORS as u16).to_le_bytes());
    sector[21] = 0xF8;
    sector[22..24].copy_from_slice(&(FAT_SIZE_SECTORS as u16).to_le_bytes());
    sector[24..26].copy_from_slice(&32_u16.to_le_bytes()); // sectors per track
    sector[26..28].copy_from_slice(&64_u16.to_le_bytes()); // heads
    sector[36] = 0x80; // drive number
    sector[38] = 0x29; // extended boot signature
    sector[39..43].copy_from_slice(&VOLUME_SERIAL.to_le_bytes());
    sector[43..54].copy_from_slice(b"RISHOVERLAY");
    sector[54..62].copy_from_slice(b"FAT16   ");
    sector[510..512].copy_from_slice(&[0x55, 0xAA]);
    sector
}

fn write_fats(image: &mut [u8], allocations: &[(PathBuf, Allocated)]) {
    let mut fat = vec![0_u8; FAT_SIZE_SECTORS as usize * SECTOR_BYTES];
    fat[0..2].copy_from_slice(&0xFFF8_u16.to_le_bytes());
    fat[2..4].copy_from_slice(&0xFFFF_u16.to_le_bytes());
    for (_, allocated) in allocations {
        for index in 0..allocated.cluster_count {
            let cluster = allocated.first_cluster + index;
            let entry = if index + 1 == allocated.cluster_count {
                EOC
            } else {
                cluster as u16 + 1
            };
            let offset = cluster as usize * 2;
            fat[offset..offset + 2].copy_from_slice(&entry.to_le_bytes());
        }
    }
    for copy in 0..NUM_FATS {
        let offset = (FAT1_SECTOR + copy * FAT_SIZE_SECTORS) as usize * SECTOR_BYTES;
        image[offset..offset + fat.len()].copy_from_slice(&fat);
    }
}

/// Serialised directory entries for one directory. The root directory omits
/// the "." / ".." entries (its parent is the fixed root region).
fn dir_entries(
    node: &Node,
    this_cluster: u32,
    parent_cluster: u32,
    allocations: &[(PathBuf, Allocated)],
    this_path: &Path,
) -> Result<Vec<u8>> {
    let children = node_children(node);
    let mut entries = Vec::with_capacity(entries_for(node) * 32);
    if !this_path.as_os_str().is_empty() {
        write_dot_entry(&mut entries, this_cluster, parent_cluster);
    }
    for child in children {
        let mut child_path = this_path.to_path_buf();
        child_path.push(&child.name);
        let allocated = allocations
            .iter()
            .find(|(path, _)| *path == child_path)
            .map(|(_, allocated)| *allocated)
            .ok_or_else(|| invalid_data("missing allocation for directory child"))?;
        let short = child
            .short
            .ok_or_else(|| invalid_data("short name was not assigned"))?;
        let cluster = if matches!(child.kind, Kind::File(_)) && allocated.cluster_count == 0 {
            0
        } else {
            allocated.first_cluster
        };
        let size = match &child.kind {
            Kind::File(bytes) => bytes.len() as u32,
            Kind::Directory(_) => 0,
        };
        let attr = match &child.kind {
            Kind::File(_) => ATTR_ARCHIVE,
            Kind::Directory(_) => ATTR_DIRECTORY,
        };
        if child.needs_lfn {
            write_lfn_entries(&mut entries, &child.name, short);
        }
        write_short_entry(&mut entries, short, attr, cluster, size);
    }
    Ok(entries)
}

fn write_dot_entry(entries: &mut Vec<u8>, this_cluster: u32, parent_cluster: u32) {
    let mut dot = [b' '; 11];
    dot[0] = b'.';
    write_short_entry(entries, dot, ATTR_DIRECTORY, this_cluster, 0);
    let mut dotdot = [b' '; 11];
    dotdot[0] = b'.';
    dotdot[1] = b'.';
    write_short_entry(entries, dotdot, ATTR_DIRECTORY, parent_cluster, 0);
}

fn write_lfn_entries(entries: &mut Vec<u8>, name: &str, short: [u8; 11]) {
    let chunks = lfn_chunks(name);
    let chunk_count = chunks.len() as u8;
    let checksum = lfn_checksum(&short);
    for (position, chunk) in chunks.iter().enumerate().rev() {
        let number = position as u8 + 1;
        let mut entry = [0_u8; 32];
        entry[0] = if number == chunk_count { number | 0x40 } else { number };
        write_utf16(&mut entry, 1, &chunk[..5]);
        entry[11] = ATTR_LFN;
        entry[13] = checksum;
        write_utf16(&mut entry, 14, &chunk[5..11]);
        write_utf16(&mut entry, 28, &chunk[11..13]);
        entries.extend_from_slice(&entry);
    }
}

fn write_short_entry(
    entries: &mut Vec<u8>,
    short: [u8; 11],
    attr: u8,
    cluster: u32,
    size: u32,
) {
    let mut entry = [0_u8; 32];
    entry[0..11].copy_from_slice(&short);
    entry[11] = attr;
    entry[14..16].copy_from_slice(&PINNED_TIME.to_le_bytes());
    entry[16..18].copy_from_slice(&PINNED_DATE.to_le_bytes());
    entry[18..20].copy_from_slice(&PINNED_DATE.to_le_bytes());
    entry[22..24].copy_from_slice(&PINNED_TIME.to_le_bytes());
    entry[24..26].copy_from_slice(&PINNED_DATE.to_le_bytes());
    entry[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    entry[28..32].copy_from_slice(&size.to_le_bytes());
    entries.extend_from_slice(&entry);
}

fn write_utf16(entry: &mut [u8; 32], offset: usize, chars: &[u16]) {
    for (index, ch) in chars.iter().enumerate() {
        entry[offset + index * 2..offset + index * 2 + 2].copy_from_slice(&ch.to_le_bytes());
    }
}

// ---------------------------------------------------------------------------
// Extraction (verification)
// ---------------------------------------------------------------------------

fn extract_tree(image: &[u8], dest: &Path) -> Result<()> {
    if image.len() != IMAGE_SECTORS as usize * SECTOR_BYTES {
        return Err(invalid_data("image size does not match the pinned 4 MiB layout"));
    }
    validate_boot_sector(image)?;
    let fat = read_fat(image);
    fs::create_dir_all(dest)?;
    let start = ROOT_DIR_SECTOR as usize * SECTOR_BYTES;
    let root_entries = image[start..start + ROOT_DIR_SECTORS as usize * SECTOR_BYTES].to_vec();
    extract_directory(image, &fat, &root_entries, dest)
}

fn validate_boot_sector(image: &[u8]) -> Result<()> {
    let sector = &image[..SECTOR_BYTES];
    if sector[510] != 0x55 || sector[511] != 0xAA {
        return Err(invalid_data("boot signature missing"));
    }
    let read_u16 = |offset: usize| u16::from_le_bytes([sector[offset], sector[offset + 1]]);
    if read_u16(11) != SECTOR_BYTES as u16
        || sector[13] != SECTORS_PER_CLUSTER as u8
        || read_u16(14) != RESERVED_SECTORS as u16
        || sector[16] != NUM_FATS as u8
        || read_u16(17) != ROOT_ENTRIES as u16
        || read_u16(19) != IMAGE_SECTORS as u16
        || sector[21] != 0xF8
        || read_u16(22) != FAT_SIZE_SECTORS as u16
    {
        return Err(invalid_data("boot sector does not match the pinned FAT16 geometry"));
    }
    Ok(())
}

fn read_fat(image: &[u8]) -> Vec<u16> {
    let start = FAT1_SECTOR as usize * SECTOR_BYTES;
    let end = start + FAT_SIZE_SECTORS as usize * SECTOR_BYTES;
    image[start..end]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

fn cluster_chain(fat: &[u16], start: u32) -> Result<Vec<u32>> {
    let mut chain = Vec::new();
    let mut cluster = start;
    while cluster != EOC as u32 {
        if cluster < 2 || cluster >= CLUSTER_COUNT + 2 {
            return Err(invalid_data(format!("cluster chain escapes the data area: {cluster}")));
        }
        if chain.len() as u32 >= CLUSTER_COUNT {
            return Err(invalid_data("cluster chain does not terminate"));
        }
        chain.push(cluster);
        let next = *fat
            .get(cluster as usize)
            .ok_or_else(|| invalid_data("cluster index exceeds the FAT"))?;
        if next == cluster as u16 {
            return Err(invalid_data("cluster chain loops on itself"));
        }
        cluster = next as u32;
    }
    Ok(chain)
}

fn read_chain(image: &[u8], fat: &[u16], start: u32, size: usize) -> Result<Vec<u8>> {
    let chain = cluster_chain(fat, start)?;
    let capacity = chain.len() * SECTOR_BYTES;
    if size > capacity {
        return Err(invalid_data("file size exceeds its cluster chain"));
    }
    let mut bytes = Vec::with_capacity(size);
    for cluster in chain {
        let offset = cluster_offset(cluster);
        bytes.extend_from_slice(&image[offset..offset + SECTOR_BYTES]);
        if bytes.len() >= size {
            break;
        }
    }
    bytes.truncate(size);
    Ok(bytes)
}

fn read_full_chain(image: &[u8], fat: &[u16], start: u32) -> Result<Vec<u8>> {
    let chain = cluster_chain(fat, start)?;
    let mut bytes = Vec::with_capacity(chain.len() * SECTOR_BYTES);
    for cluster in chain {
        let offset = cluster_offset(cluster);
        bytes.extend_from_slice(&image[offset..offset + SECTOR_BYTES]);
    }
    Ok(bytes)
}

fn read_utf16(entry: &[u8], offset: usize, out: &mut [u16]) {
    for (index, slot) in out.iter_mut().enumerate() {
        let start = offset + index * 2;
        *slot = u16::from_le_bytes([entry[start], entry[start + 1]]);
    }
}

fn short_name_text(short: &[u8; 11]) -> String {
    let base = String::from_utf8_lossy(&short[..8]).trim_end().to_owned();
    let ext = String::from_utf8_lossy(&short[8..]).trim_end().to_owned();
    if ext.is_empty() { base } else { format!("{base}.{ext}") }
}

fn extract_directory(image: &[u8], fat: &[u16], entries: &[u8], dest: &Path) -> Result<()> {
    let mut offset = 0_usize;
    let mut lfn_chunks_seen: Vec<(u8, [u16; 13])> = Vec::new();
    let mut lfn_checksum_seen: Option<u8> = None;
    let mut lfn_last_number: Option<u8> = None;
    while offset + 32 <= entries.len() {
        let entry = &entries[offset..offset + 32];
        offset += 32;
        if entry[0] == 0x00 {
            break;
        }
        if entry[0] == 0xE5 {
            lfn_chunks_seen.clear();
            lfn_checksum_seen = None;
            lfn_last_number = None;
            continue;
        }
        if entry[11] == ATTR_LFN {
            let number = entry[0] & 0x3F;
            let last = entry[0] & 0x40 != 0;
            if number == 0 {
                return Err(invalid_data("malformed LFN sequence"));
            }
            if last || lfn_chunks_seen.is_empty() {
                lfn_chunks_seen.clear();
                lfn_checksum_seen = None;
                lfn_last_number = None;
            }
            if last {
                if lfn_last_number.is_some() {
                    return Err(invalid_data("LFN sequence has two flagged entries"));
                }
                lfn_last_number = Some(number);
                lfn_chunks_seen.push((number, lfn_unicode(entry)?));
                lfn_checksum_seen = Some(entry[13]);
            } else if lfn_checksum_seen == Some(entry[13]) {
                lfn_chunks_seen.push((number, lfn_unicode(entry)?));
            } else {
                return Err(invalid_data("malformed LFN sequence"));
            }
            continue;
        }
        let short = entry[0..11].try_into().unwrap_or([b' '; 11]);
        let name = if lfn_chunks_seen.is_empty() {
            short_name_text(&short)
        } else {
            let checksum = lfn_checksum(&short);
            if lfn_checksum_seen != Some(checksum) {
                return Err(invalid_data("LFN checksum mismatch"));
            }
            let mut sorted = lfn_chunks_seen.clone();
            sorted.sort_by_key(|(number, _)| *number);
            for (index, (number, _)) in sorted.iter().enumerate() {
                if *number != index as u8 + 1 {
                    return Err(invalid_data("LFN sequence numbers are not contiguous"));
                }
            }
            if lfn_last_number != sorted.last().map(|(number, _)| *number) {
                return Err(invalid_data("LFN flagged entry is not the final chunk"));
            }
            let mut utf16 = Vec::new();
            for (_, chunk) in &sorted {
                utf16.extend(chunk.iter().take_while(|ch| **ch != 0x0000));
            }
            String::from_utf16(&utf16)
                .map_err(|_| invalid_data("LFN is not valid UTF-16"))?
        };
        lfn_chunks_seen.clear();
        lfn_checksum_seen = None;
        lfn_last_number = None;
        if name == "." || name == ".." {
            continue;
        }
        validate_name(&name)?;
        let attr = entry[11];
        let cluster = u32::from(u16::from_le_bytes([entry[26], entry[27]]));
        let size = u32::from_le_bytes([entry[28], entry[29], entry[30], entry[31]]) as usize;
        let target = dest.join(&name);
        if attr & ATTR_DIRECTORY != 0 {
            fs::create_dir_all(&target)?;
            if cluster != 0 {
                let content = read_full_chain(image, fat, cluster)?;
                extract_directory(image, fat, &content, &target)?;
            }
        } else if cluster == 0 && size == 0 {
            fs::write(&target, [])?;
        } else if cluster != 0 {
            let content = read_chain(image, fat, cluster, size)?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target, content)?;
        } else {
            return Err(invalid_data(format!(
                "file {name:?} has a size but no starting cluster"
            )));
        }
    }
    Ok(())
}

fn lfn_unicode(entry: &[u8]) -> Result<[u16; 13]> {
    // The name ends at the first 0x0000 code unit; anything after it is
    // filler (see lfn_chunks). Extraction truncates at that NUL.
    let mut chars = [0x0000_u16; 13];
    read_utf16(entry, 1, &mut chars[..5]);
    read_utf16(entry, 14, &mut chars[5..11]);
    read_utf16(entry, 28, &mut chars[11..13]);
    Ok(chars)
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

fn build(overlay: &Path, output: &Path) -> Result<()> {
    if !fs::metadata(overlay)?.is_dir() {
        return Err(invalid_data("OVERLAY_DIR must be a directory"));
    }
    if output.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("output already exists: {}", output.display()),
        ));
    }
    let mut root = new_node(String::new(), Kind::Directory(Vec::new()));
    let children = match &mut root.kind {
        Kind::Directory(c) => c,
        Kind::File(_) => unreachable!(),
    };
    read_tree(overlay, children)?;
    let mut generic_counter = 0_u32;
    assign_shorts(children, &mut generic_counter);
    let image = build_image(&root)?;
    fs::write(output, image)?;
    println!("built  {} ({IMAGE_SECTORS} sectors)", output.display());
    Ok(())
}

fn extract(image_path: &Path, dest: &Path) -> Result<()> {
    let image = fs::read(image_path)?;
    extract_tree(&image, dest)
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [command, first, second] if command == "build" => build(Path::new(first), Path::new(second)),
        [command, first, second] if command == "extract" => extract(Path::new(first), Path::new(second)),
        _ => Err(invalid_data(
            "usage: mk-root-disk build OVERLAY_DIR OUTPUT_IMG | extract IMG DEST_DIR",
        )),
    }
}

fn main() {
    if let Err(error) = run() {
        die(&error.to_string());
    }
}

#[cfg(test)]
mod tests {
    include!("mk-root-disk.tests.rs");
}
