use std::{ffi::c_void, fmt, path::Path, ptr::NonNull, str};

use serde::{Deserialize, Serialize};

use crate::{
    SoftVmError, TctiSourceLock, ValidatedArtifacts,
    abi::{
        self, RishTctiApiV1, RishTctiBuildInfoV1, RishTctiConfigV1, RishTctiHostCallbacksV1,
        RishTctiRunResultV1, RishTctiSliceV1, RishTctiSnapshotV1,
    },
    config::GUEST_ARCHITECTURE,
    serial::ProviderIo,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    QemuTcti,
    ExperimentalPureRust,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderBuildInfo {
    pub kind: ProviderKind,
    pub qemu_version: String,
    pub source_revision: String,
    pub build_id: String,
    pub guest_architecture: String,
    pub host_architecture: String,
    pub target_list: String,
    pub compiled_features: u64,
    pub min_memory_mib: u32,
    pub max_memory_mib: u32,
    pub max_vcpus: u32,
}

impl ProviderBuildInfo {
    pub(crate) fn validate_production(&self) -> Result<(), SoftVmError> {
        let source = TctiSourceLock::default();
        if self.kind != ProviderKind::QemuTcti {
            return Err(contract(
                "experimental providers are not eligible for VmEngine probing",
            ));
        }
        require_equal("QEMU version", &self.qemu_version, source.version)?;
        require_equal("QEMU source revision", &self.source_revision, source.commit)?;
        require_equal(
            "guest architecture",
            &self.guest_architecture,
            GUEST_ARCHITECTURE,
        )?;
        require_equal(
            "provider host architecture",
            &self.host_architecture,
            source.host_architecture,
        )?;
        require_equal("QEMU target list", &self.target_list, source.target_list)?;
        let missing = abi::REQUIRED_FEATURES & !self.compiled_features;
        if missing != 0 {
            return Err(contract(&format!(
                "provider is missing required feature bits 0x{missing:016x}"
            )));
        }
        let forbidden = abi::FORBIDDEN_FEATURES & self.compiled_features;
        if forbidden != 0 {
            return Err(contract(&format!(
                "provider exposes forbidden JIT/hypervisor/private feature bits 0x{forbidden:016x}"
            )));
        }
        if self.build_id.is_empty() {
            return Err(contract("provider build id cannot be empty"));
        }
        if self.min_memory_mib == 0
            || self.max_memory_mib < self.min_memory_mib
            || self.max_vcpus == 0
        {
            return Err(contract("provider resource limits are invalid"));
        }
        Ok(())
    }

    #[must_use]
    pub fn supports(&self, feature: u64) -> bool {
        self.compiled_features & feature == feature
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineState {
    Running,
    Halted,
    Stopped,
    Faulted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderSnapshot {
    pub state: MachineState,
    pub pc: Option<u64>,
    pub total_units: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderRun {
    pub executed_units: u64,
    pub snapshot: ProviderSnapshot,
}

#[derive(Clone, Debug)]
pub struct ProviderRequest {
    pub memory_mib: u32,
    pub vcpus: u32,
    pub artifacts: ValidatedArtifacts,
    pub network_mode: u32,
}

/// One provider-owned VM. It is created and used exclusively on one worker.
pub trait ProviderMachine {
    fn snapshot(&mut self) -> Result<ProviderSnapshot, SoftVmError>;
    fn run_quantum(&mut self, max_units: u64) -> Result<ProviderRun, SoftVmError>;
    fn request_stop(&mut self) -> Result<(), SoftVmError>;
}

/// Pluggable x86-64 interpreter provider boundary.
///
/// The production gate currently accepts only the pinned QEMU TCTI build.
/// A pure-Rust backend can implement this trait as an experimental provider
/// without becoming an advertised Full VM backend.
pub trait MachineProvider: Send + Sync {
    fn build_info(&self) -> &ProviderBuildInfo;
    fn create(
        &self,
        request: ProviderRequest,
        io: ProviderIo,
    ) -> Result<Box<dyn ProviderMachine>, SoftVmError>;
}

/// Validated C ABI adapter for the pinned UTM QEMU TCTI build.
pub struct TctiProvider {
    api: RishTctiApiV1,
    build_info: ProviderBuildInfo,
}

impl fmt::Debug for TctiProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TctiProvider")
            .field("build_info", &self.build_info)
            .finish_non_exhaustive()
    }
}

impl TctiProvider {
    /// Constructs an adapter from a statically linked provider function table.
    ///
    /// # Safety
    ///
    /// Every non-null function pointer must use the ABI declared in
    /// `include/rish_tcti_provider.h` and remain callable for the lifetime of
    /// this value. The provider must uphold all buffer and callback lifetimes
    /// in that header.
    pub unsafe fn from_api(api: RishTctiApiV1) -> Result<Self, SoftVmError> {
        validate_api(&api)?;
        let build_info_fn = api
            .build_info
            .ok_or_else(|| contract("build_info function is missing"))?;
        let mut raw = RishTctiBuildInfoV1::default();
        // SAFETY: The caller guarantees this function table follows ABI v1,
        // and `raw` is writable for the duration of the call.
        let status = unsafe { build_info_fn(&mut raw) };
        check_status("build_info", status)?;
        let build_info = parse_build_info(&raw)?;
        build_info.validate_production()?;
        Ok(Self { api, build_info })
    }
}

impl MachineProvider for TctiProvider {
    fn build_info(&self) -> &ProviderBuildInfo {
        &self.build_info
    }

    fn create(
        &self,
        request: ProviderRequest,
        io: ProviderIo,
    ) -> Result<Box<dyn ProviderMachine>, SoftVmError> {
        let create = self
            .api
            .create
            .ok_or_else(|| contract("create function is missing"))?;
        let kernel = utf8_path(request.artifacts.kernel.path(), "kernel")?;
        let initrd = request
            .artifacts
            .initrd
            .as_ref()
            .map(|file| utf8_path(file.path(), "initrd"))
            .transpose()?;
        let root_disk = utf8_path(request.artifacts.root_disk.path(), "root disk")?;
        let config = RishTctiConfigV1 {
            struct_size: std::mem::size_of::<RishTctiConfigV1>(),
            abi_version: abi::ABI_VERSION_V1,
            memory_mib: request.memory_mib,
            vcpus: request.vcpus,
            kernel_path: borrowed_slice(kernel),
            initrd_path: initrd.map_or_else(RishTctiSliceV1::empty, borrowed_slice),
            root_disk_path: borrowed_slice(root_disk),
            network_mode: request.network_mode,
        };
        let mut io = Box::new(io);
        let callbacks = RishTctiHostCallbacksV1 {
            struct_size: std::mem::size_of::<RishTctiHostCallbacksV1>(),
            abi_version: abi::ABI_VERSION_V1,
            context: (&mut *io as *mut ProviderIo).cast::<c_void>(),
            serial_write: Some(serial_write),
            serial_read: Some(serial_read),
            should_cancel: Some(should_cancel),
        };
        let mut raw_handle = std::ptr::null_mut();
        // SAFETY: All path slices outlive this create call. The boxed callback
        // context is retained by `TctiMachine` until after provider destroy.
        let status = unsafe { create(&config, &callbacks, &mut raw_handle) };
        check_status("create", status)?;
        let handle = NonNull::new(raw_handle)
            .ok_or_else(|| contract("create succeeded with a null VM handle"))?;
        Ok(Box::new(TctiMachine {
            api: self.api,
            handle,
            _io: io,
        }))
    }
}

struct TctiMachine {
    api: RishTctiApiV1,
    handle: NonNull<c_void>,
    _io: Box<ProviderIo>,
}

impl ProviderMachine for TctiMachine {
    fn snapshot(&mut self) -> Result<ProviderSnapshot, SoftVmError> {
        let snapshot = self
            .api
            .snapshot
            .ok_or_else(|| contract("snapshot function is missing"))?;
        let mut raw = RishTctiSnapshotV1::default();
        // SAFETY: `handle` belongs to this provider instance and `raw` is
        // writable for the duration of the call.
        let status = unsafe { snapshot(self.handle.as_ptr(), &mut raw) };
        check_status("snapshot", status)?;
        parse_snapshot(&raw)
    }

    fn run_quantum(&mut self, max_units: u64) -> Result<ProviderRun, SoftVmError> {
        let run = self
            .api
            .run_quantum
            .ok_or_else(|| contract("run_quantum function is missing"))?;
        let mut raw = RishTctiRunResultV1::default();
        // SAFETY: `handle` belongs to this provider instance and `raw` is
        // writable for the duration of the bounded call.
        let status = unsafe { run(self.handle.as_ptr(), max_units, &mut raw) };
        check_status("run_quantum", status)?;
        validate_header(
            "run result",
            raw.struct_size,
            raw.abi_version,
            std::mem::size_of::<RishTctiRunResultV1>(),
        )?;
        if raw.executed_units > max_units {
            return Err(contract("provider exceeded the requested run quantum"));
        }
        Ok(ProviderRun {
            executed_units: raw.executed_units,
            snapshot: parse_snapshot(&raw.snapshot)?,
        })
    }

    fn request_stop(&mut self) -> Result<(), SoftVmError> {
        let stop = self
            .api
            .request_stop
            .ok_or_else(|| contract("request_stop function is missing"))?;
        // SAFETY: `handle` belongs to this provider instance.
        let status = unsafe { stop(self.handle.as_ptr()) };
        check_status("request_stop", status)
    }
}

impl Drop for TctiMachine {
    fn drop(&mut self) {
        if let Some(destroy) = self.api.destroy {
            // SAFETY: The handle is owned by this wrapper and is destroyed once.
            unsafe { destroy(self.handle.as_ptr()) };
        }
    }
}

fn validate_api(api: &RishTctiApiV1) -> Result<(), SoftVmError> {
    validate_header(
        "API table",
        api.struct_size,
        api.abi_version,
        std::mem::size_of::<RishTctiApiV1>(),
    )?;
    if api.build_info.is_none()
        || api.create.is_none()
        || api.snapshot.is_none()
        || api.run_quantum.is_none()
        || api.request_stop.is_none()
        || api.destroy.is_none()
    {
        return Err(contract("API table has a null required function pointer"));
    }
    Ok(())
}

fn parse_build_info(raw: &RishTctiBuildInfoV1) -> Result<ProviderBuildInfo, SoftVmError> {
    validate_header(
        "build info",
        raw.struct_size,
        raw.abi_version,
        std::mem::size_of::<RishTctiBuildInfoV1>(),
    )?;
    Ok(ProviderBuildInfo {
        kind: ProviderKind::QemuTcti,
        qemu_version: fixed_string(&raw.qemu_version, "QEMU version")?,
        source_revision: fixed_string(&raw.source_revision, "source revision")?,
        build_id: fixed_string(&raw.build_id, "build id")?,
        guest_architecture: fixed_string(&raw.guest_architecture, "guest architecture")?,
        host_architecture: fixed_string(&raw.host_architecture, "host architecture")?,
        target_list: fixed_string(&raw.target_list, "target list")?,
        compiled_features: raw.compiled_features,
        min_memory_mib: raw.min_memory_mib,
        max_memory_mib: raw.max_memory_mib,
        max_vcpus: raw.max_vcpus,
    })
}

fn parse_snapshot(raw: &RishTctiSnapshotV1) -> Result<ProviderSnapshot, SoftVmError> {
    validate_header(
        "snapshot",
        raw.struct_size,
        raw.abi_version,
        std::mem::size_of::<RishTctiSnapshotV1>(),
    )?;
    let state = match raw.state {
        abi::MACHINE_RUNNING => MachineState::Running,
        abi::MACHINE_HALTED => MachineState::Halted,
        abi::MACHINE_STOPPED => MachineState::Stopped,
        abi::MACHINE_FAULTED => MachineState::Faulted,
        other => {
            return Err(contract(&format!(
                "provider returned unknown machine state {other}"
            )));
        }
    };
    if raw.pc_valid > 1 {
        return Err(contract("snapshot pc_valid must be zero or one"));
    }
    Ok(ProviderSnapshot {
        state,
        pc: (raw.pc_valid == 1).then_some(raw.pc),
        total_units: raw.total_units,
    })
}

fn validate_header(
    label: &str,
    actual_size: usize,
    actual_version: u32,
    required_size: usize,
) -> Result<(), SoftVmError> {
    if actual_version != abi::ABI_VERSION_V1 {
        return Err(contract(&format!(
            "{label} ABI version {actual_version} is not {}",
            abi::ABI_VERSION_V1
        )));
    }
    if actual_size < required_size {
        return Err(contract(&format!(
            "{label} size {actual_size} is smaller than {required_size}"
        )));
    }
    Ok(())
}

fn fixed_string<const N: usize>(
    bytes: &[u8; N],
    label: &'static str,
) -> Result<String, SoftVmError> {
    let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(N);
    let value =
        str::from_utf8(&bytes[..end]).map_err(|_| contract(&format!("{label} is not UTF-8")))?;
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(contract(&format!("{label} is empty or contains controls")));
    }
    Ok(value.to_owned())
}

fn require_equal(label: &str, actual: &str, expected: &str) -> Result<(), SoftVmError> {
    if actual != expected {
        return Err(contract(&format!(
            "{label} {actual:?} does not match pinned {expected:?}"
        )));
    }
    Ok(())
}

fn utf8_path<'a>(path: &'a Path, label: &str) -> Result<&'a str, SoftVmError> {
    path.to_str()
        .ok_or_else(|| contract(&format!("{label} path is not valid UTF-8")))
}

fn borrowed_slice(value: &str) -> RishTctiSliceV1 {
    RishTctiSliceV1 {
        data: value.as_ptr(),
        len: value.len(),
    }
}

fn check_status(operation: &'static str, status: i32) -> Result<(), SoftVmError> {
    if status == abi::STATUS_OK {
        Ok(())
    } else {
        Err(SoftVmError::ProviderCall { operation, status })
    }
}

fn contract(message: &str) -> SoftVmError {
    SoftVmError::ProviderContract(message.to_owned())
}

unsafe extern "C" fn serial_write(context: *mut c_void, data: *const u8, len: usize) -> usize {
    if context.is_null() || (data.is_null() && len != 0) {
        return 0;
    }
    // SAFETY: The provider contract keeps both pointers valid for this call.
    let io = unsafe { &*context.cast::<ProviderIo>() };
    // SAFETY: The provider contract supplies a readable `len` byte slice.
    let bytes = unsafe { std::slice::from_raw_parts(data, len) };
    io.write_serial(bytes)
}

unsafe extern "C" fn serial_read(context: *mut c_void, data: *mut u8, capacity: usize) -> usize {
    if context.is_null() || (data.is_null() && capacity != 0) {
        return 0;
    }
    // SAFETY: The provider contract keeps both pointers valid for this call.
    let io = unsafe { &*context.cast::<ProviderIo>() };
    // SAFETY: The provider contract supplies a writable `capacity` byte slice.
    let output = unsafe { std::slice::from_raw_parts_mut(data, capacity) };
    io.read_serial(output)
}

unsafe extern "C" fn should_cancel(context: *mut c_void) -> u8 {
    if context.is_null() {
        return 1;
    }
    // SAFETY: The callback context remains alive until provider destroy.
    u8::from(unsafe { &*context.cast::<ProviderIo>() }.should_cancel())
}
