//! Small helpers: logging and path handling.

use std::ffi::CString;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::MAX_PATH;
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameA;

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Verbose per-command logging is opt-in via `SOUNDARC_INPROC_LOG=1`.
/// Lifecycle events (init, failure) are always logged.
pub fn init_logging() {
    let on = std::env::var("SOUNDARC_INPROC_LOG")
        .map(|v| v != "0" && !v.is_empty())
        .unwrap_or(false);
    VERBOSE.store(on, Ordering::Relaxed);
}

pub fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

static LOG_PATH: OnceLock<Option<String>> = OnceLock::new();

/// Append a line to `soundarc_inproc.log` next to the game executable.
///
/// The file is reopened per line so the log survives a hard crash, but the path
/// is resolved once: with `SOUNDARC_INPROC_LOG=1` this runs on the audio command
/// path, and re-deriving it per line would cost a syscall and two allocations.
pub fn log(msg: &str) {
    let path = LOG_PATH.get_or_init(|| exe_dir().map(|d| format!("{d}\\soundarc_inproc.log")));
    let Some(path) = path else { return };
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{msg}");
    }
}

macro_rules! logv {
    ($($arg:tt)*) => {
        if $crate::util::verbose() { $crate::util::log(&format!($($arg)*)); }
    };
}
pub(crate) use logv;

/// Directory containing the running executable (XANADU.exe), no trailing slash.
///
/// Mirrors `SoundArc.dll`, which resolves `sound3d.dll` relative to the module
/// path rather than trusting the current working directory.
pub fn exe_dir() -> Option<String> {
    let mut buf = [0u8; MAX_PATH as usize];
    // SAFETY: a null module handle asks for the running executable, and the
    // buffer length passed is the real capacity of `buf`.
    let n = unsafe { GetModuleFileNameA(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) };
    // n == buf.len() means the path was truncated. Returning None fails cleanly
    // into the vanilla fallback rather than resolving against a wrong directory.
    if n == 0 || n as usize >= buf.len() {
        return None;
    }
    let full = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
    let cut = full.rfind(['\\', '/'])?;
    Some(full[..cut].to_string())
}

/// Build a NUL-terminated `CString`, tolerating interior NULs by truncating.
pub fn cstr(s: &str) -> CString {
    match CString::new(s) {
        Ok(c) => c,
        Err(e) => {
            let pos = e.nul_position();
            let bytes = e.into_vec();
            CString::new(&bytes[..pos]).unwrap_or_default()
        }
    }
}

/// Case-insensitive compare of a NUL-terminated C string against an ASCII literal.
///
/// # Safety
/// `p` must be a valid NUL-terminated string, or null.
pub unsafe fn cstr_eq_ignore_ascii_case(p: *const u8, want: &str) -> bool {
    if p.is_null() {
        return false;
    }
    // Reject `MAKEINTATOM`-style pseudo-pointers (high word zero) before deref.
    if (p as usize) >> 16 == 0 {
        return false;
    }
    // SAFETY: `p` is a non-null, non-atom pointer per the checks above, and the
    // caller guarantees it is NUL-terminated. The loop stops at the first NUL, so
    // the trailing read at `want.len()` is only reached when at least that many
    // non-NUL bytes were already seen, keeping it inside the allocation.
    unsafe {
        for (i, wb) in want.as_bytes().iter().enumerate() {
            let c = *p.add(i);
            if c == 0 || !c.eq_ignore_ascii_case(wb) {
                return false;
            }
        }
        *p.add(want.len()) == 0
    }
}
