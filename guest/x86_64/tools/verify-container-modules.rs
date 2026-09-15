//! Verify boot-critical modules in the final uncompressed newc archive.
//! Uses only Rust's standard library so it runs on macOS and Linux builders.
use std::collections::{BTreeMap, BTreeSet};

const KERNEL: &str = "6.18.35-0-virt";
const MODULES: &[&str] = &[
    "kernel/drivers/block/virtio_blk.ko",
    "kernel/fs/fat/fat.ko",
    "kernel/fs/fat/vfat.ko",
    "kernel/fs/nls/nls_cp437.ko",
    "kernel/fs/nls/nls_ascii.ko",
    "kernel/fs/nls/nls_utf8.ko",
    "kernel/drivers/net/virtio_net.ko",
    "kernel/drivers/net/net_failover.ko",
    "kernel/net/core/failover.ko",
    "kernel/lib/crc/crc16.ko",
    "kernel/fs/mbcache.ko",
    "kernel/fs/jbd2/jbd2.ko",
    "kernel/fs/ext4/ext4.ko",
];

type Result<T> = std::result::Result<T, String>;

fn range(bytes: &[u8], offset: usize, size: usize) -> Result<&[u8]> {
    bytes
        .get(offset..offset.checked_add(size).ok_or("range overflow")?)
        .ok_or_else(|| "truncated archive or ELF section".into())
}

fn align4(value: usize) -> Result<usize> {
    value
        .checked_add(3)
        .map(|v| v & !3)
        .ok_or("alignment overflow".into())
}

fn hex(bytes: &[u8]) -> Result<usize> {
    usize::from_str_radix(
        std::str::from_utf8(bytes).map_err(|_| "invalid newc field")?,
        16,
    )
    .map_err(|_| "invalid newc hex field".into())
}

fn unpack(bytes: &[u8]) -> Result<BTreeMap<&str, &[u8]>> {
    let mut result = BTreeMap::new();
    let mut offset = 0;
    loop {
        let header = range(bytes, offset, 110)?;
        if &header[..6] != b"070701" {
            return Err("expected uncompressed newc archive".into());
        }
        let size = hex(&header[54..62])?;
        let name_size = hex(&header[94..102])?;
        let name = range(bytes, offset + 110, name_size)?;
        if name.last() != Some(&0) || name[..name.len() - 1].contains(&0) {
            return Err("invalid newc name".into());
        }
        let name = std::str::from_utf8(&name[..name.len() - 1]).map_err(|_| "invalid newc path")?;
        let data_start = align4(offset + 110 + name_size)?;
        let data = range(bytes, data_start, size)?;
        offset = align4(data_start + size)?;
        if name == "TRAILER!!!" {
            return Ok(result);
        }
        if result.insert(name, data).is_some() {
            return Err(format!("duplicate archive path: {name}"));
        }
    }
}

fn u16le(bytes: &[u8], offset: usize) -> Result<usize> {
    Ok(u16::from_le_bytes(range(bytes, offset, 2)?.try_into().unwrap()) as usize)
}

fn u32le(bytes: &[u8], offset: usize) -> Result<usize> {
    usize::try_from(u32::from_le_bytes(
        range(bytes, offset, 4)?.try_into().unwrap(),
    ))
    .map_err(|_| "ELF integer overflow".into())
}

fn u64le(bytes: &[u8], offset: usize) -> Result<usize> {
    usize::try_from(u64::from_le_bytes(
        range(bytes, offset, 8)?.try_into().unwrap(),
    ))
    .map_err(|_| "ELF integer overflow".into())
}

fn cstr(bytes: &[u8], offset: usize) -> Result<&str> {
    let tail = bytes.get(offset..).ok_or("invalid ELF string offset")?;
    let end = tail
        .iter()
        .position(|b| *b == 0)
        .ok_or("unterminated ELF string")?;
    std::str::from_utf8(&tail[..end]).map_err(|_| "invalid ELF string".into())
}

fn module_info(bytes: &[u8]) -> Result<BTreeMap<&str, &str>> {
    if range(bytes, 0, 6)? != b"\x7fELF\x02\x01"
        || u16le(bytes, 16)? != 1
        || u16le(bytes, 18)? != 62
    {
        return Err("module must be a little-endian x86_64 ELF relocatable".into());
    }
    let table_start = u64le(bytes, 40)?;
    let entry_size = u16le(bytes, 58)?;
    let count = u16le(bytes, 60)?;
    let strings_index = u16le(bytes, 62)?;
    if entry_size != 64 || strings_index >= count {
        return Err("unsupported ELF section table".into());
    }
    let table = range(bytes, table_start, entry_size * count)?;
    let strings_header = range(table, strings_index * entry_size, entry_size)?;
    let strings = range(
        bytes,
        u64le(strings_header, 24)?,
        u64le(strings_header, 32)?,
    )?;
    for header in table.chunks_exact(entry_size) {
        if cstr(strings, u32le(header, 0)?)? != ".modinfo" {
            continue;
        }
        let info = range(bytes, u64le(header, 24)?, u64le(header, 32)?)?;
        let mut fields = BTreeMap::new();
        for field in info.split(|b| *b == 0).filter(|field| !field.is_empty()) {
            let field = std::str::from_utf8(field).map_err(|_| "invalid module metadata")?;
            if let Some((key, value)) = field.split_once('=') {
                // Multiple aliases are normal. The fields used for validation must be unique.
                if matches!(key, "depends" | "vermagic") && fields.insert(key, value).is_some() {
                    return Err(format!("duplicate module field: {key}"));
                }
            }
        }
        return Ok(fields);
    }
    Err("ELF module has no .modinfo section".into())
}

fn verify_load_order(init: &str, dependencies: &BTreeMap<&str, Vec<&str>>) -> Result<()> {
    let mut loaded = BTreeSet::new();
    for line in init.lines() {
        let Some(tail) = line.strip_prefix("insmod \"$MODULES/") else {
            continue;
        };
        let path = tail.split('"').next().ok_or("invalid insmod path")?;
        if !MODULES.contains(&path) {
            return Err(format!("unexpected module load: {path}"));
        }
        let name = path.rsplit('/').next().unwrap().trim_end_matches(".ko");
        for dependency in dependencies
            .get(name)
            .ok_or_else(|| format!("missing module {name}"))?
        {
            if !loaded.contains(dependency) {
                return Err(format!("{name} loads before dependency {dependency}"));
            }
        }
        if !loaded.insert(name) {
            return Err(format!("duplicate module load: {name}"));
        }
    }
    if loaded.len() != MODULES.len() {
        return Err("PID 1 does not load every required module".into());
    }
    let boot = init
        .find("echo RISH_X86_64_BOOT_OK")
        .ok_or("missing boot-ready marker")?;
    let agent = init
        .find("exec /usr/bin/rish-guest-agent")
        .ok_or("missing guest agent exec")?;
    if boot >= agent || init[boot..].contains("insmod ") {
        return Err("modules must load before boot-ready marker and agent exec".into());
    }
    Ok(())
}

fn verify(bytes: &[u8]) -> Result<()> {
    let files = unpack(bytes)?;
    let init = std::str::from_utf8(files.get("init").ok_or("missing init")?)
        .map_err(|_| "invalid init")?;
    if !init
        .lines()
        .any(|line| line == format!("MODULES=/lib/modules/{KERNEL}"))
    {
        return Err("PID 1 kernel module version differs from pinned kernel".into());
    }
    let mut dependencies = BTreeMap::new();
    for path in MODULES {
        let full_path = format!("lib/modules/{KERNEL}/{path}");
        let info = module_info(
            files
                .get(full_path.as_str())
                .ok_or_else(|| format!("missing {full_path}"))?,
        )
        .map_err(|error| format!("{path}: {error}"))?;
        let vermagic = info.get("vermagic").ok_or("missing module vermagic")?;
        if vermagic.split_whitespace().next() != Some(KERNEL) {
            return Err(format!("{path}: module built for another kernel"));
        }
        let depends = info.get("depends").ok_or("missing module dependencies")?;
        dependencies.insert(
            path.rsplit('/').next().unwrap().trim_end_matches(".ko"),
            depends.split(',').filter(|name| !name.is_empty()).collect(),
        );
    }
    verify_load_order(init, &dependencies)
}

fn main() {
    let result = std::env::args_os()
        .nth(1)
        .ok_or("usage: verify-container-modules ARCHIVE".into())
        .and_then(|path| std::fs::read(path).map_err(|error| error.to_string()))
        .and_then(|bytes| verify(&bytes));
    match result {
        Ok(()) => println!(
            "verified {} container modules, kernel ABI, and PID 1 dependency order",
            MODULES.len()
        ),
        Err(error) => {
            eprintln!("verify-container-modules: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dependencies() -> BTreeMap<&'static str, Vec<&'static str>> {
        let mut result: BTreeMap<_, _> = MODULES
            .iter()
            .map(|path| {
                (
                    path.rsplit('/').next().unwrap().trim_end_matches(".ko"),
                    vec![],
                )
            })
            .collect();
        result.insert("ext4", vec!["jbd2", "crc16", "mbcache"]);
        result.insert("vfat", vec!["fat"]);
        result.insert("virtio_net", vec!["net_failover"]);
        result.insert("net_failover", vec!["failover"]);
        result
    }

    #[test]
    fn pid_one_loads_required_modules_in_dependency_order() {
        verify_load_order(include_str!("../container-overlay/init"), &dependencies()).unwrap();
    }

    #[test]
    fn rejects_missing_and_late_dependencies() {
        let init = include_str!("../container-overlay/init");
        for name in ["crc16", "mbcache", "jbd2", "ext4", "fat", "failover"] {
            let broken = init
                .lines()
                .filter(|line| !line.contains(&format!("/{name}.ko\"")))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                verify_load_order(&broken, &dependencies()).is_err(),
                "accepted missing {name}"
            );
        }
        let early = format!("insmod \"$MODULES/kernel/fs/ext4/ext4.ko\"\n{init}");
        assert!(verify_load_order(&early, &dependencies())
            .unwrap_err()
            .contains("before dependency"));
    }

    #[test]
    fn rejects_missing_or_early_boot_marker() {
        let init = include_str!("../container-overlay/init");
        assert!(verify_load_order(
            &init.replace("echo RISH_X86_64_BOOT_OK", ""),
            &dependencies()
        )
        .is_err());
        assert!(verify_load_order(
            &format!("echo RISH_X86_64_BOOT_OK\n{init}"),
            &dependencies()
        )
        .is_err());
    }

    #[test]
    fn rejects_non_modules_and_truncated_inputs() {
        assert!(module_info(b"not a module").is_err());
        assert!(module_info(b"\x7fELF\x02\x01").is_err());
        assert!(unpack(b"070701").is_err());
        let mut malformed = b"070701".to_vec();
        malformed.extend_from_slice(&[b'0'; 104]);
        assert!(unpack(&malformed).is_err());
    }
}
