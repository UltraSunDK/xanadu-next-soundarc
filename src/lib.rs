//! Runs Xanadu Next's audio server inside the game process.
//!
//! `XANADU.exe` looks for its sound server with
//! `FindWindowA("SOUNDARC", "Xanadu Next")` and, failing that, spawns
//! `soundarc.dll` (a PE32 executable) as a separate process. `FindWindowA`
//! searches all top-level windows on the desktop, including the caller's own,
//! so this DLL creates the window itself and reimplements the `WM_USER`
//! protocol against `sound3d.dll` loaded in-process.
//!
//! `FindWindowA` is also IAT-hooked so the result does not depend on winning a
//! startup race. If the server fails to come up the hook falls through to the
//! real `FindWindowA` and the game behaves as it does unmodded.
//!
//! Ships as a `dinput8.dll` proxy; see [`proxy`]. Protocol details are in
//! README.md.

#![allow(non_snake_case)]
// This DLL cannot avoid unsafe: it rewrites another module's import table, runs
// naked assembly thunks and calls transmuted foreign function pointers. What it
// can do is justify each one, so every unsafe block carries a SAFETY comment.
#![warn(clippy::undocumented_unsafe_blocks)]

mod iat;
mod proxy;
mod server;
mod sound3d;
mod util;

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{HMODULE, HWND};
use windows_sys::Win32::System::LibraryLoader::{
    DisableThreadLibraryCalls, GetModuleHandleA, GetProcAddress,
};
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

use util::{cstr, cstr_eq_ignore_ascii_case, log};

type FnFindWindowA = unsafe extern "system" fn(*const u8, *const u8) -> HWND;

static REAL_FIND_WINDOW_A: AtomicUsize = AtomicUsize::new(0);

/// Stand-in for `user32!FindWindowA`. Only lookups of the sound server class
/// are intercepted; everything else is passed through.
unsafe extern "system" fn hook_find_window_a(class: *const u8, title: *const u8) -> HWND {
    // SAFETY: installed in place of `FindWindowA`, so `class` and `title` are
    // whatever the caller passed that API: null, an atom, or a NUL-terminated
    // string. `cstr_eq_ignore_ascii_case` handles all three. `REAL_FIND_WINDOW_A`
    // holds the genuine `FindWindowA` address, stored before any IAT slot was
    // patched, so the transmute matches the real signature.
    unsafe {
        if cstr_eq_ignore_ascii_case(class, server::CLASS_NAME)
            && let Some(h) = server::ensure_started()
        {
            return h;
        }
        // Server unavailable: run the stock path, which spawns the original
        // soundarc.dll process.
        let real = REAL_FIND_WINDOW_A.load(Ordering::Relaxed);
        if real == 0 {
            return std::ptr::null_mut();
        }
        let f: FnFindWindowA = std::mem::transmute(real);
        f(class, title)
    }
}

unsafe fn install_hook() {
    // SAFETY: the argument is a NUL-terminated literal that outlives the call.
    let user32: HMODULE = unsafe { GetModuleHandleA(cstr("user32.dll").as_ptr() as *const u8) };
    if user32.is_null() {
        log("[soundarc-inproc] WARN: user32.dll not loaded yet; hook not installed");
        return;
    }
    let name = cstr("FindWindowA");
    // SAFETY: `user32` is a live module handle, null-checked above, and `name`
    // is NUL-terminated and outlives the call.
    let target = match unsafe { GetProcAddress(user32, name.as_ptr() as *const u8) } {
        Some(f) => f as usize,
        None => {
            log("[soundarc-inproc] WARN: GetProcAddress(FindWindowA) failed");
            return;
        }
    };
    REAL_FIND_WINDOW_A.store(target, Ordering::SeqCst);

    let replacement = hook_find_window_a as *const () as usize;
    // SAFETY: both are real code addresses. `target` came from `GetProcAddress`
    // and `replacement` is one of our own functions.
    if unsafe { iat::patch_iat(target, replacement) } == 0 {
        // Not fatal: the server thread still creates the window, and the game
        // polls FindWindowA for ~2 s, so it will most likely still find us.
        log("[soundarc-inproc] WARN: no FindWindowA IAT slot patched; relying on startup race");
    }
}

/// # Safety
/// Called by the Windows loader only, with the loader lock held.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(hinst: HMODULE, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        // SAFETY: `hinst` is this module's handle, supplied by the loader. The
        // spawned thread does not run until DllMain returns and the loader lock
        // is released, so nothing here runs work under that lock.
        unsafe {
            DisableThreadLibraryCalls(hinst);
            util::init_logging();
            install_hook();
            // Start the server now so it is usually ready before the game asks.
            // The hook above makes this an optimisation, not a dependency. The
            // thread does not run until DllMain returns and the loader lock is
            // released, so spawning here is safe.
            std::thread::spawn(|| {
                server::ensure_started();
            });
        }
    }
    1
}
