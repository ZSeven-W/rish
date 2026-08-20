use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

const CPIO_MAGIC: &[u8; 6] = b"070701";
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;

#[derive(Clone, Copy)]
enum EntryKind {
    File,
    Directory,
    Symlink,
}

struct Entry {
    host_path: PathBuf,
    archive_name: Vec<u8>,
    kind: EntryKind,
    mode: u32,
}

struct ArchiveWriter {
    output: BufWriter<File>,
    offset: u64,
}

impl ArchiveWriter {
    fn new(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o644))?;
        Ok(Self {
            output: BufWriter::new(file),
            offset: 0,
        })
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.output.write_all(bytes)?;
        self.offset = self
            .offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("archive size overflow"))?;
        Ok(())
    }

    fn pad_to(&mut self, alignment: u64) -> io::Result<()> {
        let padding = (alignment - (self.offset % alignment)) % alignment;
        if padding > 0 {
            let zeros = [0_u8; 512];
            self.write_bytes(&zeros[..padding as usize])?;
        }
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        self.pad_to(512)?;
        self.output.flush()?;
        self.output.get_ref().sync_all()
    }
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn classify(metadata: &Metadata, path: &Path) -> io::Result<EntryKind> {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        Ok(EntryKind::File)
    } else if file_type.is_dir() {
        Ok(EntryKind::Directory)
    } else if file_type.is_symlink() {
        Ok(EntryKind::Symlink)
    } else {
        Err(invalid_data(format!(
            "unsupported special file in rootfs: {}",
            path.display()
        )))
    }
}

fn archive_name(relative: &Path) -> io::Result<Vec<u8>> {
    let bytes = if relative.as_os_str().is_empty() {
        b".".to_vec()
    } else {
        relative.as_os_str().as_bytes().to_vec()
    };
    if bytes.contains(&0) {
        return Err(invalid_data("archive path contains NUL"));
    }
    if bytes.starts_with(b"/") {
        return Err(invalid_data("archive path is absolute"));
    }
    for component in bytes.split(|byte| *byte == b'/') {
        if component.is_empty() || component == b".." {
            return Err(invalid_data("archive path has an unsafe component"));
        }
    }
    Ok(bytes)
}

fn collect_entries(root: &Path, relative: &Path, entries: &mut Vec<Entry>) -> io::Result<()> {
    let host_path = root.join(relative);
    let metadata = fs::symlink_metadata(&host_path)?;
    let kind = classify(&metadata, &host_path)?;
    let type_mode = match kind {
        EntryKind::File => S_IFREG,
        EntryKind::Directory => S_IFDIR,
        EntryKind::Symlink => S_IFLNK,
    };
    entries.push(Entry {
        host_path: host_path.clone(),
        archive_name: archive_name(relative)?,
        kind,
        mode: type_mode | (metadata.mode() & 0o7777),
    });

    if matches!(kind, EntryKind::Directory) {
        let mut children = fs::read_dir(&host_path)?
            .map(|result| result.map(|child| child.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        children.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        for child in children {
            let child_relative = relative.join(Path::new(&child));
            collect_entries(root, &child_relative, entries)?;
        }
    }
    Ok(())
}

fn to_u32(value: u64, label: &str) -> io::Result<u32> {
    u32::try_from(value).map_err(|_| invalid_data(format!("{label} exceeds newc limits")))
}

fn write_header(
    archive: &mut ArchiveWriter,
    inode: u32,
    mode: u32,
    data_size: u32,
    name_size: u32,
) -> io::Result<()> {
    archive.write_bytes(CPIO_MAGIC)?;
    let fields = [
        inode, mode, 0, 0, 1, 0, data_size, 0, 0, 0, 0, name_size, 0,
    ];
    for field in fields {
        archive.write_bytes(format!("{field:08x}").as_bytes())?;
    }
    Ok(())
}

fn write_entry(archive: &mut ArchiveWriter, entry: &Entry, inode: u32) -> io::Result<()> {
    let symlink_data = match entry.kind {
        EntryKind::Symlink => Some(
            fs::read_link(&entry.host_path)?
                .as_os_str()
                .as_bytes()
                .to_vec(),
        ),
        EntryKind::File | EntryKind::Directory => None,
    };
    let data_size = match entry.kind {
        EntryKind::File => to_u32(fs::metadata(&entry.host_path)?.len(), "file size")?,
        EntryKind::Directory => 0,
        EntryKind::Symlink => to_u32(
            symlink_data
                .as_ref()
                .ok_or_else(|| invalid_data("missing symlink target"))?
                .len() as u64,
            "symlink target size",
        )?,
    };
    let name_size = to_u32(entry.archive_name.len() as u64 + 1, "path size")?;
    write_header(archive, inode, entry.mode, data_size, name_size)?;
    archive.write_bytes(&entry.archive_name)?;
    archive.write_bytes(&[0])?;
    archive.pad_to(4)?;

    match entry.kind {
        EntryKind::File => {
            let mut input = File::open(&entry.host_path)?;
            let copied = io::copy(&mut input, &mut archive.output)?;
            archive.offset = archive
                .offset
                .checked_add(copied)
                .ok_or_else(|| io::Error::other("archive size overflow"))?;
            if copied != u64::from(data_size) {
                return Err(invalid_data(format!(
                    "file changed while packing: {}",
                    entry.host_path.display()
                )));
            }
        }
        EntryKind::Directory => {}
        EntryKind::Symlink => archive.write_bytes(
            symlink_data
                .as_deref()
                .ok_or_else(|| invalid_data("missing symlink target"))?,
        )?,
    }
    archive.pad_to(4)
}

fn write_trailer(archive: &mut ArchiveWriter) -> io::Result<()> {
    let name = b"TRAILER!!!";
    write_header(archive, 0, 0, 0, (name.len() + 1) as u32)?;
    archive.write_bytes(name)?;
    archive.write_bytes(&[0])?;
    archive.pad_to(4)
}

fn parse_arguments() -> io::Result<(PathBuf, PathBuf)> {
    let mut arguments = env::args_os();
    let program = arguments
        .next()
        .unwrap_or_else(|| OsStr::new("pack-newc").to_owned());
    let root = arguments.next();
    let output = arguments.next();
    if root.is_none() || output.is_none() || arguments.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "usage: {} ROOTFS OUTPUT.cpio",
                Path::new(&program).display()
            ),
        ));
    }
    Ok((PathBuf::from(root.unwrap()), PathBuf::from(output.unwrap())))
}

fn run() -> io::Result<()> {
    let (root, output) = parse_arguments()?;
    let root = fs::canonicalize(root)?;
    if !fs::metadata(&root)?.is_dir() {
        return Err(invalid_data("ROOTFS must be a directory"));
    }
    if output.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("output already exists: {}", output.display()),
        ));
    }

    let mut entries = Vec::new();
    collect_entries(&root, Path::new(""), &mut entries)?;
    let mut archive = ArchiveWriter::new(&output)?;
    for (index, entry) in entries.iter().enumerate() {
        let inode = to_u32(index as u64 + 1, "inode number")?;
        write_entry(&mut archive, entry, inode)?;
    }
    write_trailer(&mut archive)?;
    archive.finish()
}

fn main() {
    if let Err(error) = run() {
        eprintln!("pack-newc: {error}");
        std::process::exit(1);
    }
}
