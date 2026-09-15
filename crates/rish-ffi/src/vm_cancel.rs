//! Independent, one-shot cancellation ownership for blocking C ABI calls.

use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use rish_softvm_x86_64::X86_64Machine;

pub(crate) const CANCELLED: &str = "E_VM_CANCELLED";
pub(crate) const TIMED_OUT: &str = "E_VM_TIMEOUT";

#[derive(Default)]
pub(crate) struct Cancellation {
    reason: AtomicU8,
    claimed: AtomicBool,
    machine: Mutex<Weak<X86_64Machine>>,
}

impl Cancellation {
    pub(crate) fn claim(&self) -> Result<(), String> {
        self.check()?;
        if self.claimed.swap(true, Ordering::AcqRel) {
            return Err("E_VM_CANCEL_REUSED".into());
        }
        Ok(())
    }

    pub(crate) fn attach(&self, machine: &Arc<X86_64Machine>) -> Result<(), String> {
        *self
            .machine
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Arc::downgrade(machine);
        // Covers cancellation between launch and attaching the weak reference.
        self.check()
    }

    pub(crate) fn request(&self) {
        self.request_reason(1);
    }

    pub(crate) fn timeout(&self) {
        self.request_reason(2);
    }

    fn request_reason(&self, reason: u8) {
        // Preserve the first cause, including a timeout observed while writing
        // the request before SessionIo has an opportunity to return an error.
        let _ = self
            .reason
            .compare_exchange(0, reason, Ordering::AcqRel, Ordering::Acquire);
        let machine = self
            .machine
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .upgrade();
        if let Some(machine) = machine {
            // This only stores the worker atomic; it never takes a session or
            // command lock, waits for guest execution, or aliases an &mut.
            machine.cancel();
        }
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        match self.reason.load(Ordering::Acquire) {
            0 => Ok(()),
            2 => {
                self.request_reason(2);
                Err(TIMED_OUT.into())
            }
            _ => {
                self.request();
                Err(CANCELLED.into())
            }
        }
    }
}

/// An opaque handle owns an Arc; the session clones it during synchronous boot.
pub(crate) type CancelHandle = Arc<Cancellation>;

/// Creates one cancellation handle for one guest lifetime. It starts uncancelled.
#[unsafe(no_mangle)]
pub extern "C" fn rish_vm_cancel_new() -> *mut std::ffi::c_void {
    Box::into_raw(Box::new(Arc::new(Cancellation::default()))).cast()
}

/// Requests sticky cancellation without accessing the session handle.
///
/// # Safety
/// `handle` must be null or a live handle returned by `rish_vm_cancel_new`.
/// Concurrent requests and boot are allowed; freeing this pointer concurrently
/// with any use of the pointer is not allowed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rish_vm_cancel_request(handle: *mut std::ffi::c_void) {
    if !handle.is_null() {
        // SAFETY: caller guarantees a live shared cancellation handle.
        unsafe { &*handle.cast::<CancelHandle>() }.request();
    }
}

/// Releases the caller's handle ownership; a live session keeps its own Arc.
///
/// # Safety
/// The pointer must be null or returned by `rish_vm_cancel_new`, freed exactly
/// once, after all boot/request calls using this pointer have returned. Do not
/// access this pointer after free, even if its associated session is still live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rish_vm_cancel_free(handle: *mut std::ffi::c_void) {
    if !handle.is_null() {
        // SAFETY: caller guarantees unique pointer ownership at destruction.
        drop(unsafe { Box::from_raw(handle.cast::<CancelHandle>()) });
    }
}

#[cfg(test)]
#[path = "vm_cancel_tests.rs"]
pub(crate) mod tests;
