//! Bindings to `sound3d.dll` (Falcom `SoundMgV3`, 2005, MSVC 7.1).
//!
//! Every export is `cdecl`. The argument shapes were recovered from
//! `SoundArc.dll`'s dispatcher (`0x401420`) and its loader (`0x4027f0`). The
//! `add esp, N` after each call pins down the argument count and whether floats
//! travel by value or by reference.
//!
//! The asymmetry below is in the original, not a transcription slip:
//!   * `IPlaySE` / `IPlaySEB` / `ISetSEPos` take positional floats by pointer
//!     (C++ `float&`) straight into the argument register file.
//!   * `ISetListen` takes its ten floats by value.

use std::ffi::c_char;

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

use crate::util::{cstr, exe_dir, log};

type FnInit = unsafe extern "C" fn(isize) -> i32;
type FnVoid = unsafe extern "C" fn() -> i32;
type FnI1 = unsafe extern "C" fn(i32) -> i32;
type FnI2 = unsafe extern "C" fn(i32, i32) -> i32;
type FnI3 = unsafe extern "C" fn(i32, i32, i32) -> i32;
type FnStr = unsafe extern "C" fn(*const c_char) -> i32;
/// `(id, name, a, b, c, d)`
type FnPlay = unsafe extern "C" fn(i32, *const c_char, i32, i32, i32, i32) -> i32;
/// `(id, name, a, b, c, d, e)`
type FnPlayEx = unsafe extern "C" fn(i32, *const c_char, i32, i32, i32, i32, i32) -> i32;
/// `(name, &x, &y, &z, flags, id, vol)`
type FnPlaySe =
    unsafe extern "C" fn(*const c_char, *const f32, *const f32, *const f32, i32, i32, f32) -> i32;
/// Ten floats by value.
#[rustfmt::skip]
type FnSetListen =
    unsafe extern "C" fn(f32, f32, f32, f32, f32, f32, f32, f32, f32, f32) -> i32;
/// `(id, &a, &b, &c, &d, &e, &f)`
#[rustfmt::skip]
type FnSetSePos = unsafe extern "C" fn(
    i32, *const f32, *const f32, *const f32, *const f32, *const f32, *const f32,
) -> i32;

macro_rules! resolve {
    ($m:expr, $name:literal) => {{
        let n = concat!($name, "\0");
        GetProcAddress($m, n.as_ptr()).map(|f| std::mem::transmute(f))
    }};
}

/// Resolved entry points. `None` means the export was absent; the matching
/// opcode then becomes a silent no-op, as in `SoundArc.dll`.
#[allow(non_snake_case)]
pub struct Sound3d {
    pub IInit: Option<FnInit>,
    pub MixOpen: Option<FnVoid>,
    /// Resolved for parity with SoundArc, which pairs it with `MixOpen`, but
    /// never called: the mixer is torn down by process exit anyway, and closing
    /// it during a shutdown order we do not control risks a hang.
    #[allow(dead_code)]
    pub MixClose: Option<FnVoid>,

    pub IPlaySE: Option<FnPlaySe>,
    pub IPlaySEB: Option<FnPlaySe>,
    pub IPlayFade: Option<FnPlay>,
    pub IPlayFadeB: Option<FnPlay>,
    pub IPlayFadeEx: Option<FnPlayEx>,
    pub IPlayFadeBEx: Option<FnPlayEx>,
    pub IPlay: Option<FnPlay>,
    pub IPlayB: Option<FnPlay>,
    pub ILoadSE: Option<FnStr>,
    pub IFadeOut: Option<FnI1>,
    pub IFadeOutEx: Option<FnI2>,
    pub IPlayStop: Option<FnI1>,
    pub IVolFade: Option<FnI3>,
    pub IStopAllSE: Option<FnVoid>,
    pub IStopSE: Option<FnI1>,
    pub IGetPos: Option<FnI1>,
    pub ISetSEVol: Option<FnI1>,
    pub ISetBGMVol: Option<FnI1>,
    pub ISetMem: Option<FnI2>,
    pub ISetListen: Option<FnSetListen>,
    pub ISetSEPos: Option<FnSetSePos>,
}

impl Sound3d {
    /// Load `sound3d.dll` from the game directory and resolve every entry point
    /// the WM_USER dispatcher can reach.
    pub fn load() -> Option<Self> {
        let dir = exe_dir()?;
        let path = cstr(&format!("{dir}\\sound3d.dll"));
        // SAFETY: `path` is NUL-terminated and outlives the call. Reached only
        // from the server thread, never from `DllMain`, so the loader lock is
        // not held.
        let m: HMODULE = unsafe { LoadLibraryA(path.as_ptr() as *const u8) };
        if m.is_null() {
            log("[soundarc-inproc] FATAL: LoadLibrary(sound3d.dll) failed");
            return None;
        }

        // SAFETY: `m` is a live module handle, null-checked above. Each
        // `resolve!` transmutes a `GetProcAddress` result to the signature
        // recovered from SoundArc's dispatcher; see the module docs. A name that
        // does not resolve yields `None` rather than a bogus pointer.
        let s = unsafe {
            Sound3d {
                IInit: resolve!(m, "IInit"),
                MixOpen: resolve!(m, "MixOpen"),
                MixClose: resolve!(m, "MixClose"),
                IPlaySE: resolve!(m, "IPlaySE"),
                IPlaySEB: resolve!(m, "IPlaySEB"),
                IPlayFade: resolve!(m, "IPlayFade"),
                IPlayFadeB: resolve!(m, "IPlayFadeB"),
                IPlayFadeEx: resolve!(m, "IPlayFadeEx"),
                IPlayFadeBEx: resolve!(m, "IPlayFadeBEx"),
                IPlay: resolve!(m, "IPlay"),
                IPlayB: resolve!(m, "IPlayB"),
                ILoadSE: resolve!(m, "ILoadSE"),
                IFadeOut: resolve!(m, "IFadeOut"),
                IFadeOutEx: resolve!(m, "IFadeOutEx"),
                IPlayStop: resolve!(m, "IPlayStop"),
                IVolFade: resolve!(m, "IVolFade"),
                IStopAllSE: resolve!(m, "IStopAllSE"),
                IStopSE: resolve!(m, "IStopSE"),
                IGetPos: resolve!(m, "IGetPos"),
                ISetSEVol: resolve!(m, "ISetSEVol"),
                ISetBGMVol: resolve!(m, "ISetBGMVol"),
                ISetMem: resolve!(m, "ISetMem"),
                ISetListen: resolve!(m, "ISetListen"),
                ISetSEPos: resolve!(m, "ISetSEPos"),
            }
        };

        // SoundArc pops "Library version mismatch." if either of these is
        // missing. Warn instead of blocking; a partial engine still plays BGM.
        if s.ISetSEPos.is_none() || s.ILoadSE.is_none() {
            log(
                "[soundarc-inproc] WARN: sound3d.dll is missing ISetSEPos/ILoadSE (version mismatch?)",
            );
        }
        if s.IPlaySE.is_none() && s.IPlayFade.is_none() {
            log("[soundarc-inproc] FATAL: sound3d.dll exports no usable playback entry points");
            return None;
        }
        Some(s)
    }
}
