use super::*;
use std::{ffi::c_void, sync::mpsc, time::Duration};

pub(crate) fn looping_kernel() -> Vec<u8> {
    let mut image = vec![0_u8; 4096];
    image[0x1f1] = 2;
    image[0x1fe..0x200].copy_from_slice(&0xaa55_u16.to_le_bytes());
    image[0x202..0x206].copy_from_slice(b"HdrS");
    image[0x206..0x208].copy_from_slice(&0x020f_u16.to_le_bytes());
    image[0x214..0x218].copy_from_slice(&0x0010_0000_u32.to_le_bytes());
    image[0x234] = 1;
    image[0x22c..0x230].copy_from_slice(&0x7fff_ffff_u32.to_le_bytes());
    image[0x230..0x234].copy_from_slice(&0x0020_0000_u32.to_le_bytes());
    image[0x260..0x264].copy_from_slice(&0x0100_0000_u32.to_le_bytes());
    image[0x248..0x24c].copy_from_slice(&0x600_u32.to_le_bytes());
    image[0x24c..0x250].copy_from_slice(&0x300_u32.to_le_bytes());
    image[0x236..0x238].copy_from_slice(&1_u16.to_le_bytes());
    image[0x800..0x805].copy_from_slice(&[0xe9, 0xfb, 0xff, 0xff, 0xff]);
    image
}

#[test]
fn pre_cancelled_boot_does_not_read_artifacts() {
    let cancel = Arc::new(Cancellation::default());
    cancel.request();
    let result = crate::vm_ffi::vm_boot_session_with_cancel(
        r#"{"kernel_path":"/must-not-read","initrd_path":"/must-not-read"}"#,
        cancel,
    );
    assert_eq!(result.err().as_deref(), Some(CANCELLED));
}

#[test]
fn a_token_is_claimed_once_even_after_failed_boot() {
    let cancel = Arc::new(Cancellation::default());
    let request = r#"{"kernel_path":"/missing-kernel","initrd_path":"/missing-initrd"}"#;
    assert!(crate::vm_ffi::vm_boot_session_with_cancel(request, cancel.clone()).is_err());
    assert_eq!(
        crate::vm_ffi::vm_boot_session_with_cancel(request, cancel)
            .err()
            .as_deref(),
        Some("E_VM_CANCEL_REUSED")
    );
}

#[test]
fn freeing_handle_keeps_cloned_session_ownership_alive() {
    let handle = rish_vm_cancel_new();
    // SAFETY: the freshly allocated handle is live and accessed on this thread.
    let session_owner = unsafe { &*handle.cast::<CancelHandle>() }.clone();
    let weak = Arc::downgrade(&session_owner);
    // SAFETY: no concurrent raw-pointer calls; pointer is freed exactly once.
    unsafe { rish_vm_cancel_free(handle) };
    session_owner.request();
    assert_eq!(session_owner.check().err().as_deref(), Some(CANCELLED));
    drop(session_owner);
    assert!(weak.upgrade().is_none());
}

#[test]
fn independent_c_handle_cancels_an_actual_interpreter_during_boot() {
    let directory = tempfile::tempdir().unwrap();
    let kernel = directory.path().join("bzImage");
    let initrd = directory.path().join("initrd");
    std::fs::write(&kernel, looping_kernel()).unwrap();
    std::fs::write(&initrd, [0_u8; 512]).unwrap();
    let request = serde_json::json!({"kernel_path":kernel,"initrd_path":initrd,
        "memory_mib":384,"boot_budget_units":u64::MAX})
    .to_string();
    let handle = rish_vm_cancel_new();
    let address = handle as usize;
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        // SAFETY: main retains token through completion; JSON bytes are live.
        let result = unsafe {
            crate::rish_vm_boot_session_cancellable(
                request.as_ptr().cast(),
                request.len(),
                address as *mut c_void,
            )
        };
        tx.send(result as usize).unwrap();
    });
    // SAFETY: live handle with only immutable Arc reads/request operations.
    let token = unsafe { &*handle.cast::<CancelHandle>() };
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if token.machine.lock().unwrap().upgrade().is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "interpreter never launched"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(Duration::from_millis(5));
    // SAFETY: token retained, cancellation explicitly supports another thread.
    unsafe { rish_vm_cancel_request(handle) };
    assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), 0);
    worker.join().unwrap();
    assert!(token.machine.lock().unwrap().upgrade().is_none());
    // SAFETY: worker returned; all pointer access is complete, free only once.
    unsafe { rish_vm_cancel_free(handle) };
}

#[test]
fn null_cancellation_functions_are_noops() {
    // SAFETY: all these functions explicitly accept null pointers.
    unsafe {
        rish_vm_cancel_request(std::ptr::null_mut());
        rish_vm_cancel_free(std::ptr::null_mut());
        assert!(
            crate::rish_vm_boot_session_cancellable(std::ptr::null(), 0, std::ptr::null_mut())
                .is_null()
        );
    }
}
