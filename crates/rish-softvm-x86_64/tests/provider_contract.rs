use std::{ffi::c_void, fs, sync::Arc};

use rish_softvm_x86_64::{
    EngineLimits, MachineState, SoftVmError, TctiProvider, X86_64SoftwareEngine,
    abi::{
        self, RishTctiApiV1, RishTctiBuildInfoV1, RishTctiConfigV1, RishTctiHostCallbacksV1,
        RishTctiRunResultV1, RishTctiSnapshotV1,
    },
};
use rish_vm::{VmAcceleration, VmConfig, VmDevice, VmEngine, VmProbe};
use tempfile::tempdir;

struct FakeMachine {
    callbacks: RishTctiHostCallbacksV1,
    total_units: u64,
    emitted_marker: bool,
    stopped: bool,
}

unsafe extern "C" fn build_info(output: *mut RishTctiBuildInfoV1) -> i32 {
    if output.is_null() {
        return -1;
    }
    let mut value = RishTctiBuildInfoV1::default();
    put_string(&mut value.qemu_version, b"10.0.2");
    put_string(
        &mut value.source_revision,
        b"37ba092d59aff24900dfd0d5e01d4ed68441ba07",
    );
    put_string(&mut value.build_id, b"adapter-contract-test");
    put_string(&mut value.guest_architecture, b"x86_64");
    put_string(&mut value.host_architecture, b"aarch64");
    put_string(&mut value.target_list, b"x86_64-softmmu");
    value.compiled_features =
        abi::REQUIRED_FEATURES | abi::FEATURE_INITRD | abi::FEATURE_USER_NETWORK;
    value.min_memory_mib = 128;
    value.max_memory_mib = 2048;
    value.max_vcpus = 1;
    // SAFETY: Null was rejected and the caller supplies writable ABI storage.
    unsafe { *output = value };
    abi::STATUS_OK
}

unsafe extern "C" fn build_info_with_jit(output: *mut RishTctiBuildInfoV1) -> i32 {
    // SAFETY: Forwarding the same ABI pointer to the base test function.
    let status = unsafe { build_info(output) };
    if status == abi::STATUS_OK {
        // SAFETY: The base function already validated and initialized output.
        unsafe { (*output).compiled_features |= abi::FEATURE_JIT };
    }
    status
}

unsafe extern "C" fn create(
    config: *const RishTctiConfigV1,
    callbacks: *const RishTctiHostCallbacksV1,
    output: *mut *mut c_void,
) -> i32 {
    if config.is_null() || callbacks.is_null() || output.is_null() {
        return -1;
    }
    // SAFETY: All pointers were checked and are valid for this create call.
    let config = unsafe { &*config };
    if config.abi_version != abi::ABI_VERSION_V1 || config.memory_mib != 128 || config.vcpus != 1 {
        return -2;
    }
    // SAFETY: The callback table is copied while valid; its context outlives
    // the resulting handle under the provider contract.
    let callbacks = unsafe { *callbacks };
    let machine = Box::new(FakeMachine {
        callbacks,
        total_units: 0,
        emitted_marker: false,
        stopped: false,
    });
    // SAFETY: `output` is writable and ownership transfers to destroy().
    unsafe { *output = Box::into_raw(machine).cast::<c_void>() };
    abi::STATUS_OK
}

unsafe extern "C" fn snapshot(handle: *mut c_void, output: *mut RishTctiSnapshotV1) -> i32 {
    if handle.is_null() || output.is_null() {
        return -1;
    }
    // SAFETY: The handle was allocated by create and remains live.
    let machine = unsafe { &*handle.cast::<FakeMachine>() };
    let value = RishTctiSnapshotV1 {
        state: if machine.stopped {
            abi::MACHINE_STOPPED
        } else {
            abi::MACHINE_RUNNING
        },
        pc_valid: 1,
        pc: 0x100000 + machine.total_units,
        total_units: machine.total_units,
        ..RishTctiSnapshotV1::default()
    };
    // SAFETY: Null was rejected and output is writable ABI storage.
    unsafe { *output = value };
    abi::STATUS_OK
}

unsafe extern "C" fn run_quantum(
    handle: *mut c_void,
    max_units: u64,
    output: *mut RishTctiRunResultV1,
) -> i32 {
    if handle.is_null() || output.is_null() {
        return -1;
    }
    // SAFETY: The handle was allocated by create and is worker-confined.
    let machine = unsafe { &mut *handle.cast::<FakeMachine>() };
    let cancelled = machine
        .callbacks
        .should_cancel
        .map(|callback| {
            // SAFETY: The adapter owns a live callback context.
            unsafe { callback(machine.callbacks.context) != 0 }
        })
        .unwrap_or(true);
    let executed = if cancelled || machine.stopped {
        0
    } else {
        max_units
    };
    machine.total_units += executed;

    if !machine.emitted_marker {
        write_serial(&machine.callbacks, b"TCTI ABI\n");
        machine.emitted_marker = true;
    }
    let mut input = [0_u8; 32];
    let read = machine
        .callbacks
        .serial_read
        .map(|callback| {
            // SAFETY: The adapter owns a live callback context and output.
            unsafe { callback(machine.callbacks.context, input.as_mut_ptr(), input.len()) }
        })
        .unwrap_or(0);
    write_serial(&machine.callbacks, &input[..read.min(input.len())]);

    let result = RishTctiRunResultV1 {
        executed_units: executed,
        snapshot: RishTctiSnapshotV1 {
            state: if machine.stopped {
                abi::MACHINE_STOPPED
            } else {
                abi::MACHINE_RUNNING
            },
            pc_valid: 1,
            pc: 0x100000 + machine.total_units,
            total_units: machine.total_units,
            ..RishTctiSnapshotV1::default()
        },
        ..RishTctiRunResultV1::default()
    };
    // SAFETY: Null was rejected and output is writable ABI storage.
    unsafe { *output = result };
    abi::STATUS_OK
}

unsafe extern "C" fn request_stop(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        return -1;
    }
    // SAFETY: The handle was allocated by create and is worker-confined.
    unsafe { (*handle.cast::<FakeMachine>()).stopped = true };
    abi::STATUS_OK
}

unsafe extern "C" fn destroy(handle: *mut c_void) {
    if !handle.is_null() {
        // SAFETY: The adapter calls destroy exactly once for an owned handle.
        drop(unsafe { Box::from_raw(handle.cast::<FakeMachine>()) });
    }
}

#[test]
fn c_abi_adapter_routes_bounded_step_and_uart() {
    let directory = tempdir().unwrap();
    let kernel = directory.path().join("bzImage");
    let root = directory.path().join("root.img");
    fs::write(&kernel, linux_bzimage_header()).unwrap();
    fs::write(&root, vec![0x5a; 4096]).unwrap();

    // SAFETY: The static test function table implements ABI v1.
    let provider = unsafe { TctiProvider::from_api(api(build_info)) }.unwrap();
    let engine = X86_64SoftwareEngine::new(Arc::new(provider), EngineLimits::default()).unwrap();
    assert!(matches!(engine.probe(), VmProbe::Available { .. }));

    let machine = engine.launch(&config(&kernel, &root)).unwrap();
    assert_eq!(machine.initial_snapshot().state, MachineState::Running);
    assert_eq!(machine.write_console(b"echo").unwrap(), 4);
    let report = machine.run_units(100).unwrap();
    assert_eq!(report.executed_units, 100);
    assert_eq!(report.console, b"TCTI ABI\necho");
    assert_eq!(report.dropped_console_bytes, 0);
    assert_eq!(report.snapshot.total_units, 100);
}

#[test]
fn default_engine_and_jit_provider_fail_closed() {
    assert!(matches!(
        X86_64SoftwareEngine::default().probe(),
        VmProbe::Unavailable { .. }
    ));

    // SAFETY: The test table has valid ABI functions; its build feature report
    // is deliberately forbidden and must be rejected by the Rust gate.
    let error = unsafe { TctiProvider::from_api(api(build_info_with_jit)) }.unwrap_err();
    assert!(matches!(error, SoftVmError::ProviderContract(_)));
}

fn api(build_info: abi::BuildInfoFn) -> RishTctiApiV1 {
    RishTctiApiV1 {
        struct_size: std::mem::size_of::<RishTctiApiV1>(),
        abi_version: abi::ABI_VERSION_V1,
        build_info: Some(build_info),
        create: Some(create),
        snapshot: Some(snapshot),
        run_quantum: Some(run_quantum),
        request_stop: Some(request_stop),
        destroy: Some(destroy),
    }
}

fn config(kernel: &std::path::Path, root: &std::path::Path) -> VmConfig {
    VmConfig {
        architecture: "x86_64".to_owned(),
        vcpus: 1,
        memory_mib: 128,
        kernel_path: kernel.to_string_lossy().into_owned(),
        initrd_path: None,
        root_disk_path: root.to_string_lossy().into_owned(),
        acceleration: VmAcceleration::Interpreter,
        devices: vec![VmDevice::Console],
    }
}

fn linux_bzimage_header() -> Vec<u8> {
    let mut image = vec![0; 0x238];
    image[0x1fe..0x200].copy_from_slice(&[0x55, 0xaa]);
    image[0x202..0x206].copy_from_slice(b"HdrS");
    image[0x236..0x238].copy_from_slice(&1_u16.to_le_bytes());
    image
}

fn put_string<const N: usize>(target: &mut [u8; N], value: &[u8]) {
    let length = value.len().min(N.saturating_sub(1));
    target[..length].copy_from_slice(&value[..length]);
}

fn write_serial(callbacks: &RishTctiHostCallbacksV1, bytes: &[u8]) {
    if let Some(callback) = callbacks.serial_write {
        // SAFETY: The adapter owns a live callback context and bytes.
        let _ = unsafe { callback(callbacks.context, bytes.as_ptr(), bytes.len()) };
    }
}
