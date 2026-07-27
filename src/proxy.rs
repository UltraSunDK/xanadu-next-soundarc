//! Transparent `dinput8.dll` proxy.
//!
//! The export list mirrors the real 32-bit `dinput8.dll`
//! (`C:\Windows\SysWOW64\dinput8.dll`): all six named exports, ordinals 1-6.
//!
//! Each export is a naked `jmp` thunk rather than a typed wrapper, which makes
//! the forwarding ABI-agnostic: stdcall callees still clean their own
//! arguments, cdecl callees still leave that to the caller, and there is no
//! argument count or return type to get wrong. The caller's stack frame is left
//! untouched; only `eax`/`ecx`/`edx` are clobbered, and those are scratch under
//! every calling convention in play here.
//!
//! Resolution is lazy: nothing is loaded from `DllMain`, so `LoadLibrary` is
//! never called under the loader lock.

use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryA;

use crate::util::{cstr, log};

/// Order matters: it is the index baked into each thunk below.
const EXPORTS: [&str; 6] = [
    "DirectInput8Create",
    "DllCanUnloadNow",
    "DllGetClassObject",
    "DllRegisterServer",
    "DllUnregisterServer",
    "GetdfDIJoystick",
];

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicUsize = AtomicUsize::new(0);
static REAL: [AtomicUsize; EXPORTS.len()] = [ZERO; EXPORTS.len()];
static REAL_MODULE: AtomicUsize = AtomicUsize::new(0);
static LOAD_ONCE: Once = Once::new();

/// Load the genuine `dinput8.dll` by absolute system path.
///
/// The absolute path matters: this DLL *is* `dinput8.dll` as far as the normal
/// search order is concerned, so a bare name would resolve back to us. In a
/// 32-bit process on 64-bit Windows `GetSystemDirectoryA` yields
/// `C:\Windows\SysWOW64`, which holds the correct 32-bit copy.
fn real_module() -> HMODULE {
    // SAFETY: runs at most once. The buffer handed to `GetSystemDirectoryA` is
    // a local of known size, and the path passed to `LoadLibraryA` is
    // NUL-terminated and outlives the call. Never reached from `DllMain`, so
    // this never runs under the loader lock.
    LOAD_ONCE.call_once(|| unsafe {
        let mut buf = [0u8; 260];
        let n = GetSystemDirectoryA(buf.as_mut_ptr(), buf.len() as u32);
        if n == 0 || n as usize >= buf.len() {
            log("[soundarc-inproc] FATAL: GetSystemDirectoryA failed");
            return;
        }
        let dir = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
        let path = cstr(&format!("{dir}\\dinput8.dll"));
        let m = LoadLibraryA(path.as_ptr() as *const u8);
        if m.is_null() {
            log(&format!(
                "[soundarc-inproc] FATAL: cannot load {dir}\\dinput8.dll"
            ));
        } else {
            REAL_MODULE.store(m as usize, Ordering::SeqCst);
        }
    });
    REAL_MODULE.load(Ordering::SeqCst) as HMODULE
}

/// Called from the naked thunks. Returns the address of the real export.
///
/// Aborting on failure is the only option left: the thunk has already been
/// jumped to with the caller's arguments on the stack, and there is no way to
/// synthesize a return value for a signature we do not know.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn soundarc_resolve_export(idx: usize) -> usize {
    let cached = REAL[idx].load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let m = real_module();
    let addr = if m.is_null() {
        0
    } else {
        let name = cstr(EXPORTS[idx]);
        // SAFETY: `m` is a live module handle, null-checked above, and `name`
        // is NUL-terminated and outlives the call.
        unsafe { GetProcAddress(m, name.as_ptr() as *const u8) }.map_or(0, |f| f as usize)
    };
    if addr == 0 {
        log(&format!(
            "[soundarc-inproc] FATAL: cannot resolve dinput8.{}",
            EXPORTS[idx]
        ));
        std::process::abort();
    }
    REAL[idx].store(addr, Ordering::Relaxed);
    addr
}

/// Define a naked forwarding thunk for export `idx`.
macro_rules! forward {
    ($idx:expr, $name:ident) => {
        #[unsafe(no_mangle)]
        #[unsafe(naked)]
        pub unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                "push {i}",
                "call {r}",
                "add esp, 4",
                "jmp eax",
                i = const $idx,
                r = sym soundarc_resolve_export,
            )
        }
    };
}

forward!(0, DirectInput8Create);
forward!(1, DllCanUnloadNow);
forward!(2, DllGetClassObject);
forward!(3, DllRegisterServer);
forward!(4, DllUnregisterServer);
forward!(5, GetdfDIJoystick);
