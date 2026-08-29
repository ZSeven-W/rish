//! Contract tests for the ExperimentalPureRust MachineProvider: the engine
//! gate, bounded quantum execution, cancellation, UART pumping, and the
//! fail-closed rejection of impossible device requests.

use std::{fs, sync::Arc};

use rish_softvm_x86_64::{
    EngineLimits, MachineProvider, MachineState, ProviderBuildInfo, ProviderKind, PureRustProvider,
    SoftVmError, X86_64SoftwareEngine, abi,
};
use rish_vm::{VmAcceleration, VmConfig, VmDevice, VmEngine, VmNetworkMode, VmProbe};
use tempfile::tempdir;

fn pure_rust_engine(limits: EngineLimits) -> (X86_64SoftwareEngine, ProviderBuildInfo) {
    let provider = PureRustProvider::new();
    let build_info = MachineProvider::build_info(&provider).clone();
    let engine = X86_64SoftwareEngine::new(Arc::new(provider), limits).unwrap();
    (engine, build_info)
}

fn config(kernel: &std::path::Path, root: &std::path::Path, memory_mib: u32) -> VmConfig {
    VmConfig {
        architecture: "x86_64".to_owned(),
        vcpus: 1,
        memory_mib,
        kernel_path: kernel.to_string_lossy().into_owned(),
        initrd_path: None,
        root_disk_path: root.to_string_lossy().into_owned(),
        acceleration: VmAcceleration::Interpreter,
        devices: vec![VmDevice::Console],
        command_line: String::new(),
    }
}

/// A minimal valid bzImage: the setup header plus a self-looping near jump
/// at the 64-bit entry point (payload + 0x200).
fn synthetic_bzimage() -> Vec<u8> {
    let mut image = vec![0_u8; 4096];
    image[0x1F1] = 2; // setup_sectors
    image[0x1F1 + 0x0D..0x1F1 + 0x0F].copy_from_slice(&0xAA55_u16.to_le_bytes());
    image[0x1F1 + 0x11..0x1F1 + 0x15].copy_from_slice(b"HdrS");
    image[0x1F1 + 0x15..0x1F1 + 0x17].copy_from_slice(&0x020F_u16.to_le_bytes());
    image[0x1F1 + 0x23..0x1F1 + 0x27].copy_from_slice(&0x0010_0000_u32.to_le_bytes());
    image[0x1F1 + 0x43] = 1; // relocatable_kernel
    image[0x1F1 + 0x3F..0x1F1 + 0x43].copy_from_slice(&0x0020_0000_u32.to_le_bytes());
    image[0x1F1 + 0x6F..0x1F1 + 0x73].copy_from_slice(&0x0100_0000_u32.to_le_bytes());
    image[0x1F1 + 0x57..0x1F1 + 0x5B].copy_from_slice(&0x600_u32.to_le_bytes());
    image[0x1F1 + 0x5B..0x1F1 + 0x5F].copy_from_slice(&0x300_u32.to_le_bytes());
    // 64-bit boot protocol marker for artifact validation.
    image[0x236..0x238].copy_from_slice(&1_u16.to_le_bytes());
    // near jump to itself at the 64-bit entry: e9 fb ff ff ff
    image[0x600 + 0x200] = 0xE9;
    image[0x600 + 0x201..0x600 + 0x205].copy_from_slice(&(-5_i32).to_le_bytes());
    image
}

#[test]
fn pure_rust_provider_passes_the_engine_gate_and_probes() {
    let (engine, build_info) = pure_rust_engine(EngineLimits::default());
    assert_eq!(build_info.kind, ProviderKind::ExperimentalPureRust);
    assert_eq!(build_info.max_vcpus, 1);
    assert_ne!(build_info.build_id, "");
    assert_ne!(build_info.source_revision, "");
    assert!(matches!(engine.probe(), VmProbe::Available { .. }));
}

#[test]
fn engine_runs_a_bounded_quantum_and_reports_state() {
    let directory = tempdir().unwrap();
    let kernel = directory.path().join("bzImage");
    let root = directory.path().join("root.img");
    fs::write(&kernel, synthetic_bzimage()).unwrap();
    fs::write(&root, vec![0x5a; 4096]).unwrap();
    let (engine, _) = pure_rust_engine(EngineLimits::default());
    let machine = engine.launch(&config(&kernel, &root, 128)).unwrap();
    assert_eq!(machine.initial_snapshot().state, MachineState::Running);
    assert!(
        !machine
            .initial_snapshot()
            .provider_source_revision
            .is_empty()
    );
    let report = machine.run_units(1000).unwrap();
    assert_eq!(report.executed_units, 1000);
    assert_eq!(report.snapshot.total_units, 1000);
    assert_eq!(report.snapshot.state, MachineState::Running);
    assert_eq!(report.console, Vec::<u8>::new());
}

#[test]
fn cancellation_stops_a_long_run() {
    let directory = tempdir().unwrap();
    let kernel = directory.path().join("bzImage");
    let root = directory.path().join("root.img");
    fs::write(&kernel, synthetic_bzimage()).unwrap();
    fs::write(&root, vec![0x5a; 4096]).unwrap();
    let (engine, _) = pure_rust_engine(EngineLimits::default());
    let machine = Arc::new(engine.launch(&config(&kernel, &root, 128)).unwrap());
    let canceller = Arc::clone(&machine);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(10));
        canceller.cancel();
    });
    let report = machine.run_units(5_000_000).unwrap();
    assert!(report.cancelled);
    assert!(report.executed_units < 5_000_000);
}

#[test]
fn engine_rejects_a_pure_rust_build_with_forbidden_features() {
    let mut build_info = PureRustProvider::build_info_static();
    build_info.compiled_features |= abi::FEATURE_JIT;
    let provider = PureRustProvider::from_build_info(build_info).unwrap();
    let error = X86_64SoftwareEngine::new(Arc::new(provider), EngineLimits::default()).unwrap_err();
    assert!(matches!(error, SoftVmError::ProviderContract(_)));
}

#[test]
fn engine_rejects_multiple_vcpus_for_the_interpreter() {
    let directory = tempdir().unwrap();
    let kernel = directory.path().join("bzImage");
    let root = directory.path().join("root.img");
    fs::write(&kernel, synthetic_bzimage()).unwrap();
    fs::write(&root, vec![0x5a; 4096]).unwrap();
    let mut config = config(&kernel, &root, 128);
    config.vcpus = 2;
    let (engine, _) = pure_rust_engine(EngineLimits::default());
    assert!(matches!(
        engine.launch(&config),
        Err(SoftVmError::InvalidConfig(_))
    ));
}

#[test]
fn engine_rejects_user_networking_the_interpreter_does_not_implement() {
    let directory = tempdir().unwrap();
    let kernel = directory.path().join("bzImage");
    let root = directory.path().join("root.img");
    fs::write(&kernel, synthetic_bzimage()).unwrap();
    fs::write(&root, vec![0x5a; 4096]).unwrap();
    let mut config = config(&kernel, &root, 128);
    config.devices.push(VmDevice::Network {
        mode: VmNetworkMode::UserNat,
    });
    let (engine, _) = pure_rust_engine(EngineLimits::default());
    assert!(matches!(
        engine.launch(&config),
        Err(SoftVmError::InvalidConfig(_))
    ));
}

#[test]
fn engine_rejects_a_non_bzimage_kernel() {
    let directory = tempdir().unwrap();
    let kernel = directory.path().join("kernel");
    let root = directory.path().join("root.img");
    // A little-endian ELF64 x86_64 header: valid kernel, wrong format for
    // the pure-Rust loader.
    let mut image = vec![0_u8; 64];
    image[..4].copy_from_slice(b"\x7fELF");
    image[4] = 2;
    image[5] = 1;
    image[6] = 1;
    image[18..20].copy_from_slice(&62_u16.to_le_bytes());
    fs::write(&kernel, &image).unwrap();
    fs::write(&root, vec![0x5a; 4096]).unwrap();
    let (engine, _) = pure_rust_engine(EngineLimits::default());
    assert!(matches!(
        engine.launch(&config(&kernel, &root, 128)),
        Err(SoftVmError::InvalidKernel(_))
    ));
}

#[test]
fn engine_rejects_a_root_disk_whose_size_is_not_sector_aligned() {
    let directory = tempdir().unwrap();
    let kernel = directory.path().join("bzImage");
    let root = directory.path().join("root.img");
    fs::write(&kernel, synthetic_bzimage()).unwrap();
    // 1000 bytes is a valid regular file but not a multiple of 512: the
    // virtio-blk backend must refuse it instead of reporting a wrong
    // capacity to the guest.
    fs::write(&root, vec![0x5a; 1000]).unwrap();
    let (engine, _) = pure_rust_engine(EngineLimits::default());
    assert!(matches!(
        engine.launch(&config(&kernel, &root, 128)),
        Err(SoftVmError::Worker(message)) if message.contains("multiple of 512")
    ));
}

#[test]
fn engine_rejects_a_command_line_that_declares_virtio_mmio_devices() {
    let directory = tempdir().unwrap();
    let kernel = directory.path().join("bzImage");
    let root = directory.path().join("root.img");
    fs::write(&kernel, synthetic_bzimage()).unwrap();
    fs::write(&root, vec![0x5a; 4096]).unwrap();
    let mut config = config(&kernel, &root, 128);
    // The provider attaches its own device; a second virtio_mmio declaration
    // would make the guest probe the same window twice, so it fails closed.
    config.command_line = "console=ttyS0 virtio_mmio.device=1K@0xfebf0000:10".to_owned();
    let (engine, _) = pure_rust_engine(EngineLimits::default());
    assert!(matches!(
        engine.launch(&config),
        Err(SoftVmError::Worker(message)) if message.contains("virtio_mmio")
    ));
}

#[test]
fn default_engine_remains_unavailable_without_a_provider() {
    assert!(matches!(
        X86_64SoftwareEngine::default().probe(),
        VmProbe::Unavailable { .. }
    ));
}
