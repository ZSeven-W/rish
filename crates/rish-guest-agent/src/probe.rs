//! Evidence probes used before the guest advertises privileged capabilities.
//!
//! A file existing in the guest is not, by itself, proof that an operation is
//! usable. The probe therefore combines the running PID 1, cgroup mount,
//! namespace handles, effective UID, and an immutable executable runtime path
//! before it produces an OCI backend candidate.

use std::{
    collections::BTreeSet,
    env, fs,
    io::Read as _,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const DEFAULT_BUNDLE_ROOT: &str = "/var/lib/rish/oci/bundles";
const DEFAULT_RUNTIME_ROOT: &str = "/var/lib/rish/oci/runtime";
const DEFAULT_RUNTIME_CANDIDATES: &[(&str, &str)] = &[
    ("runc", "/usr/bin/runc"),
    ("youki", "/usr/bin/youki"),
    ("runc", "/usr/local/bin/runc"),
    ("youki", "/usr/local/bin/youki"),
];
const REQUIRED_NAMESPACES: &[&str] = &["mnt", "pid", "uts", "ipc", "net"];
const MAX_PROC_FILE_BYTES: usize = 1024 * 1024;
const RUNTIME_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// The facts used to construct the guest handshake and handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GuestProbe {
    pub(crate) kernel_release: String,
    pub(crate) init_system: String,
    pub(crate) cgroup_version: Option<u8>,
    pub(crate) namespaces_available: bool,
    pub(crate) systemd_available: bool,
    pub(crate) systemd_reason: String,
    pub(crate) installed_runtimes: Vec<String>,
    pub(crate) oci_runtime: Option<RuntimeDiscovery>,
    pub(crate) oci_reason: String,
}

/// A runtime executable that passed the path and permission checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeDiscovery {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) bundle_root: PathBuf,
    pub(crate) runtime_root: PathBuf,
}

impl GuestProbe {
    /// Probes the real guest filesystem. This is called only by the guest
    /// binary; tests and host-side constructors use [`Self::conservative`].
    #[must_use]
    pub(crate) fn detect() -> Self {
        let proc_root = Path::new("/proc");
        let sys_root = Path::new("/sys");
        let run_root = Path::new("/run");
        let candidates = configured_runtime_candidates();
        detect_at(proc_root, sys_root, run_root, &candidates)
    }

    /// Keeps the public library constructor deterministic and fail-closed when
    /// it is used outside the actual guest PID 1 process.
    #[must_use]
    pub(crate) fn conservative() -> Self {
        Self {
            kernel_release: "unprobed".to_owned(),
            init_system: "unprobed".to_owned(),
            cgroup_version: None,
            namespaces_available: false,
            systemd_available: false,
            systemd_reason: "guest capability probe was not run".to_owned(),
            installed_runtimes: Vec::new(),
            oci_runtime: None,
            oci_reason: "guest capability probe was not run".to_owned(),
        }
    }
}

fn detect_at(
    proc_root: &Path,
    sys_root: &Path,
    run_root: &Path,
    candidates: &[(String, PathBuf)],
) -> GuestProbe {
    let kernel_release = read_first_line(&proc_root.join("sys/kernel/osrelease"))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());
    let pid1 = read_first_line(&proc_root.join("1/comm"));
    let (init_system, systemd_available, systemd_reason) = detect_init_system(pid1, run_root);
    let cgroup_version = detect_cgroup_v2(proc_root, sys_root);
    let namespaces_available = has_required_namespaces(proc_root);
    let effective_uid = read_effective_uid(&proc_root.join("self/status"));
    let runtimes = discover_runtimes(candidates);
    let installed_runtimes = runtimes
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let oci_reason = if effective_uid != Some(0) {
        "guest agent is not running as uid 0".to_owned()
    } else if cgroup_version != Some(2) {
        "cgroup v2 is not mounted and readable".to_owned()
    } else if !namespaces_available {
        "required Linux namespace handles are unavailable".to_owned()
    } else if runtimes.is_empty() {
        "no secure runc or youki executable was found".to_owned()
    } else {
        String::new()
    };

    let oci_runtime = if oci_reason.is_empty() {
        runtimes.first().map(|(name, path)| RuntimeDiscovery {
            name: name.clone(),
            path: path.clone(),
            bundle_root: configured_absolute_path("RISH_OCI_BUNDLE_ROOT", DEFAULT_BUNDLE_ROOT),
            runtime_root: configured_absolute_path("RISH_OCI_RUNTIME_ROOT", DEFAULT_RUNTIME_ROOT),
        })
    } else {
        None
    };

    GuestProbe {
        kernel_release,
        init_system,
        cgroup_version,
        namespaces_available,
        systemd_available,
        systemd_reason,
        installed_runtimes,
        oci_runtime,
        oci_reason,
    }
}

fn detect_init_system(pid1: Option<String>, run_root: &Path) -> (String, bool, String) {
    if pid1.as_deref() == Some("systemd") && run_root.join("systemd/system").is_dir() {
        return (
            "systemd".to_owned(),
            true,
            "systemd is PID 1 and its runtime marker is mounted".to_owned(),
        );
    }

    let init_system = pid1
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());
    (
        init_system.clone(),
        false,
        format!("active PID 1 is {init_system}, not systemd"),
    )
}

fn detect_cgroup_v2(proc_root: &Path, sys_root: &Path) -> Option<u8> {
    let controllers = sys_root.join("fs/cgroup/cgroup.controllers");
    if !controllers.is_file() {
        return None;
    }
    let mountinfo = read_bounded(&proc_root.join("self/mountinfo"), MAX_PROC_FILE_BYTES)?;
    let mounted = mountinfo.lines().any(|line| {
        line.split_once(" - ")
            .and_then(|(_, filesystem)| filesystem.split_whitespace().next())
            == Some("cgroup2")
    });
    mounted.then_some(2)
}

fn has_required_namespaces(proc_root: &Path) -> bool {
    REQUIRED_NAMESPACES.iter().all(|namespace| {
        proc_root
            .join("self/ns")
            .join(namespace)
            .symlink_metadata()
            .is_ok()
    })
}

fn read_effective_uid(status_path: &Path) -> Option<u32> {
    let status = read_bounded(status_path, MAX_PROC_FILE_BYTES)?;
    let line = status.lines().find(|line| line.starts_with("Uid:"))?;
    line.split_whitespace().nth(2)?.parse().ok()
}

fn discover_runtimes(candidates: &[(String, PathBuf)]) -> Vec<(String, PathBuf)> {
    candidates
        .iter()
        .filter_map(|(name, path)| secure_executable(path).map(|path| (name.clone(), path)))
        .collect()
}

fn secure_executable(path: &Path) -> Option<PathBuf> {
    if !is_clean_absolute_path(path) {
        return None;
    }
    let canonical = path.canonicalize().ok()?;
    let metadata = canonical.metadata().ok()?;
    if !metadata.is_file() || !is_executable(&metadata) || is_group_or_world_writable(&metadata) {
        return None;
    }
    runtime_responds_to_version(&canonical).then_some(canonical)
}

/// Confirms that the candidate is an OCI runtime binary rather than merely an
/// executable file. Output is discarded and the process is time-bounded so a
/// malformed or hostile candidate cannot turn probing into an unbounded read
/// or a permanent wait.
fn runtime_responds_to_version(path: &Path) -> bool {
    let mut command = Command::new(path);
    command
        .arg("--version")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return false,
    };
    let deadline = Instant::now() + RUNTIME_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn configured_runtime_candidates() -> Vec<(String, PathBuf)> {
    if let Some(path) = env::var_os("RISH_OCI_RUNTIME") {
        let path = PathBuf::from(path);
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("configured-runtime")
            .to_owned();
        return vec![(name, path)];
    }
    DEFAULT_RUNTIME_CANDIDATES
        .iter()
        .map(|(name, path)| ((*name).to_owned(), PathBuf::from(path)))
        .collect()
}

fn configured_absolute_path(variable: &str, default: &str) -> PathBuf {
    let configured = env::var_os(variable).map(PathBuf::from);
    configured
        .filter(|path| is_clean_absolute_path(path))
        .unwrap_or_else(|| PathBuf::from(default))
}

fn read_first_line(path: &Path) -> Option<String> {
    read_bounded(path, 4096)?
        .lines()
        .next()
        .map(|line| line.trim().to_owned())
}

fn read_bounded(path: &Path, limit: usize) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(limit.min(4096));
    file.take(u64::try_from(limit).ok()?.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= limit).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

fn is_clean_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        })
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn roots() -> (TempDir, PathBuf, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let proc_root = root.path().join("proc");
        let sys_root = root.path().join("sys");
        let run_root = root.path().join("run");
        fs::create_dir_all(proc_root.join("1")).unwrap();
        fs::create_dir_all(proc_root.join("self/ns")).unwrap();
        fs::create_dir_all(sys_root.join("fs/cgroup")).unwrap();
        fs::create_dir_all(&run_root).unwrap();
        for namespace in REQUIRED_NAMESPACES {
            fs::write(proc_root.join("self/ns").join(namespace), b"namespace").unwrap();
        }
        (root, proc_root, sys_root, run_root)
    }

    #[test]
    fn systemd_requires_pid_one_and_the_runtime_marker() {
        let (_root, proc_root, _sys_root, run_root) = roots();
        fs::write(proc_root.join("1/comm"), b"systemd\n").unwrap();
        assert!(!detect_init_system(read_first_line(&proc_root.join("1/comm")), &run_root).1);
        fs::create_dir_all(run_root.join("systemd/system")).unwrap();
        let (init, available, _) =
            detect_init_system(read_first_line(&proc_root.join("1/comm")), &run_root);
        assert_eq!(init, "systemd");
        assert!(available);
    }

    #[test]
    fn cgroup_probe_requires_a_real_cgroup2_mount() {
        let (_root, proc_root, sys_root, _run_root) = roots();
        fs::write(
            sys_root.join("fs/cgroup/cgroup.controllers"),
            b"cpu memory\n",
        )
        .unwrap();
        fs::write(
            proc_root.join("self/mountinfo"),
            b"35 29 0:31 / /sys/fs/cgroup rw - tmpfs tmpfs rw\n",
        )
        .unwrap();
        assert_eq!(detect_cgroup_v2(&proc_root, &sys_root), None);
        fs::write(
            proc_root.join("self/mountinfo"),
            b"35 29 0:31 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n",
        )
        .unwrap();
        assert_eq!(detect_cgroup_v2(&proc_root, &sys_root), Some(2));
    }

    #[test]
    fn oci_candidate_requires_all_runtime_evidence() {
        let (_root, proc_root, sys_root, run_root) = roots();
        fs::create_dir_all(proc_root.join("sys/kernel")).unwrap();
        fs::write(proc_root.join("sys/kernel/osrelease"), b"6.18-test\n").unwrap();
        fs::create_dir_all(proc_root.join("self")).unwrap();
        fs::write(
            proc_root.join("self/status"),
            b"Name:\trish-guest-agent\nUid:\t1000\t1000\t1000\t1000\n",
        )
        .unwrap();
        fs::write(sys_root.join("fs/cgroup/cgroup.controllers"), b"cpu\n").unwrap();
        fs::write(
            proc_root.join("self/mountinfo"),
            b"35 29 0:31 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n",
        )
        .unwrap();
        let runtime = proc_root.join("runc");
        fs::write(&runtime, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let candidates = vec![("runc".to_owned(), runtime)];
        assert!(
            detect_at(&proc_root, &sys_root, &run_root, &candidates)
                .oci_runtime
                .is_none()
        );

        fs::write(
            proc_root.join("self/status"),
            b"Name:\trish-guest-agent\nUid:\t0\t0\t0\t0\n",
        )
        .unwrap();
        let probe = detect_at(&proc_root, &sys_root, &run_root, &candidates);
        assert_eq!(probe.kernel_release, "6.18-test");
        assert_eq!(probe.cgroup_version, Some(2));
        assert!(probe.namespaces_available);
        assert_eq!(probe.installed_runtimes, vec!["runc"]);
        assert!(probe.oci_runtime.is_some());
    }

    #[test]
    fn privilege_probe_uses_effective_not_real_uid() {
        let root = tempfile::tempdir().unwrap();
        let status = root.path().join("status");

        fs::write(&status, b"Uid:\t0\t1000\t0\t1000\n").unwrap();
        assert_eq!(read_effective_uid(&status), Some(1000));

        fs::write(&status, b"Uid:\t1000\t0\t1000\t0\n").unwrap();
        assert_eq!(read_effective_uid(&status), Some(0));
    }

    #[test]
    fn insecure_runtime_is_not_a_capability_candidate() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("runc");
        fs::write(&runtime, b"runtime").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&runtime, fs::Permissions::from_mode(0o777)).unwrap();
        }
        assert!(discover_runtimes(&[("runc".to_owned(), runtime)]).is_empty());
    }
}
