//! In-process replacement for the `SoundArc.dll` sound-server process.
//!
//! Creates a hidden top-level window with class `SOUNDARC` and title
//! `Xanadu Next` on a dedicated thread and reimplements the `WM_USER` command
//! protocol that `XANADU.exe` speaks, forwarding to `sound3d.dll`.
//!
//! The window must be top-level (`hWndParent = NULL`). A message-only window
//! (`HWND_MESSAGE`) is not returned by `FindWindowA`, which is how the game
//! locates the server.

use std::ffi::{CString, c_void};
use std::ptr::null_mut;
use std::sync::Once;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
use windows_sys::Win32::System::Threading::{
    CreateEventA, CreateThread, GetCurrentThread, SetEvent, SetThreadPriority,
    THREAD_PRIORITY_ABOVE_NORMAL, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExA, DefWindowProcA, DispatchMessageA, GetMessageA, MSG,
    RegisterClassExA, TranslateMessage, WM_COPYDATA, WM_USER, WNDCLASSEXA, WS_EX_TOOLWINDOW,
    WS_OVERLAPPED,
};

use crate::sound3d::Sound3d;
use crate::util::{log, logv};

pub const CLASS_NAME: &str = "SOUNDARC";
pub const WINDOW_TITLE: &str = "Xanadu Next";

/// Size of the argument register file. `wParam` below this bound is a register
/// write; at or above it, an opcode. Matches SoundArc's `0x427080..0x427480`.
const NUM_ARGS: usize = 0x200;
/// Filename staging buffer filled by `WM_COPYDATA` (SoundArc: `0x427480`).
const STR_BUF: usize = 0x400;

/// Server state. Created on, and thereafter touched only by, the server thread.
struct State {
    args: [u32; NUM_ARGS],
    strbuf: [u8; STR_BUF],
    /// SoundArc's `g_owner` (`0x427880`): the game HWND that claimed the server.
    owner: isize,
    initialized: bool,
    s3d: Sound3d,
}

// The accessors below take `*mut State` rather than `&State`/`&mut State` on
// purpose. `wnd_proc` is re-entrant: sound3d is static MFC over DirectSound8,
// and MFC pumps messages during modal work, so a dispatched message can land
// back here while an outer call is still inside the engine. A `&mut State` held
// across those calls would be aliased by the re-entrant one. Nothing below keeps
// a reference alive past a single field access.

/// Argument register `n`, as the integer the game wrote.
///
/// # Safety
/// `sp` must point to the live `State`, and `n` must be less than `NUM_ARGS`.
#[inline]
unsafe fn a(sp: *mut State, n: usize) -> i32 {
    // SAFETY: guaranteed by the caller; the index is bounds-checked by Rust.
    unsafe { (*sp).args[n] as i32 }
}

/// Argument register `n`, reinterpreted as the float the game bit-cast into it.
///
/// # Safety
/// As [`a`].
#[inline]
unsafe fn f(sp: *mut State, n: usize) -> f32 {
    // SAFETY: guaranteed by the caller; the index is bounds-checked by Rust.
    unsafe { f32::from_bits((*sp).args[n]) }
}

/// Pointer to argument register `n`, for the `float&` parameters.
///
/// Typed `*const` because sound3d only ever reads these. The C++ signature is a
/// non-const `float&`, so a callee that wrote back would be writing through a
/// `*const`. Nothing observed does.
///
/// # Safety
/// As [`a`]. The result stays valid for as long as the `State` box lives, which
/// is the lifetime of the process.
#[inline]
unsafe fn pf(sp: *mut State, n: usize) -> *const f32 {
    // SAFETY: guaranteed by the caller. `u32` and `f32` are both 4 bytes with no
    // padding, so element `n` of the register file is `n` floats in.
    unsafe { (&raw const (*sp).args).cast::<f32>().add(n) }
}

/// Pointer to the staged filename buffer, which is always NUL-terminated.
///
/// # Safety
/// `sp` must point to the live `State`.
#[inline]
unsafe fn s(sp: *mut State) -> *const i8 {
    // SAFETY: guaranteed by the caller. `u8` and `i8` share a layout.
    unsafe { (&raw const (*sp).strbuf).cast::<i8>() }
}

static STATE: AtomicPtr<State> = AtomicPtr::new(null_mut());
static SERVER_HWND: AtomicUsize = AtomicUsize::new(0);
/// Signalled by the server thread once startup has succeeded or failed.
static READY_EVENT: AtomicUsize = AtomicUsize::new(0);
static START: Once = Once::new();

fn signal_ready() {
    let ev = READY_EVENT.load(Ordering::SeqCst) as *mut c_void;
    if !ev.is_null() {
        // SAFETY: `ev` is a live event handle from `CreateEventA`, null-checked
        // above. `READY_EVENT` is only ever stored once, before the thread starts.
        unsafe { SetEvent(ev) };
    }
}

/// Start the server if needed and return its window handle.
///
/// Returns `None` if `sound3d.dll` could not be loaded; the caller then falls
/// back to the stock behaviour of launching the real `soundarc.dll` process.
pub fn ensure_started() -> Option<HWND> {
    // SAFETY: runs at most once. `CreateEventA`/`CreateThread` take null
    // attributes and a `'static` thread entry point, so nothing borrowed escapes.
    START.call_once(|| unsafe {
        let ev = CreateEventA(null_mut(), 1, 0, std::ptr::null());
        READY_EVENT.store(ev as usize, Ordering::SeqCst);
        let h = CreateThread(
            null_mut(),
            0,
            Some(server_thread),
            null_mut(),
            0,
            null_mut(),
        );
        if h.is_null() {
            log("[soundarc-inproc] FATAL: CreateThread failed");
            signal_ready();
        }
    });

    let ev = READY_EVENT.load(Ordering::SeqCst) as *mut c_void;
    if !ev.is_null() {
        // Far beyond a normal sound3d load; a timeout means something is badly
        // wrong and falling through to vanilla is the right answer.
        //
        // SAFETY: `ev` is the live event handle, null-checked above.
        unsafe { WaitForSingleObject(ev, 15_000) };
    }
    match SERVER_HWND.load(Ordering::SeqCst) {
        0 => None,
        h => Some(h as HWND),
    }
}

unsafe extern "system" fn server_thread(_: *mut c_void) -> u32 {
    // Load the engine up front so a failure is reported before the game commits
    // to using us.
    let s3d = match Sound3d::load() {
        Some(s) => s,
        None => {
            log("[soundarc-inproc] falling back to the external soundarc.dll process");
            signal_ready();
            return 1;
        }
    };

    // SAFETY: runs on the server thread, which owns the window class and the
    // window it registers below. Every pointer handed to Win32 is either null or
    // points to a local that outlives the call.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);

        let hinst = GetModuleHandleA(std::ptr::null());
        let class = CString::new(CLASS_NAME).unwrap();
        let title = CString::new(WINDOW_TITLE).unwrap();

        let wc = WNDCLASSEXA {
            cbSize: size_of::<WNDCLASSEXA>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr() as *const u8,
            hIconSm: null_mut(),
        };
        if RegisterClassExA(&wc) == 0 {
            log("[soundarc-inproc] FATAL: RegisterClassExA(SOUNDARC) failed");
            signal_ready();
            return 1;
        }

        let state = Box::into_raw(Box::new(State {
            args: [0; NUM_ARGS],
            strbuf: [0; STR_BUF],
            owner: 0,
            initialized: false,
            s3d,
        }));
        STATE.store(state, Ordering::SeqCst);

        // Hidden, top-level, tool window: findable by FindWindowA, absent from
        // the taskbar and alt-tab. No WS_VISIBLE, so it never paints.
        let hwnd = CreateWindowExA(
            WS_EX_TOOLWINDOW,
            class.as_ptr() as *const u8,
            title.as_ptr() as *const u8,
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            hinst,
            null_mut(),
        );
        if hwnd.is_null() {
            log("[soundarc-inproc] FATAL: CreateWindowExA failed");
            signal_ready();
            return 1;
        }

        SERVER_HWND.store(hwnd as usize, Ordering::SeqCst);
        log(
            "[soundarc-inproc] in-process SOUNDARC server ready (no child process will be spawned)",
        );
        signal_ready();

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageA(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageA(&msg);
        }
    }
    0
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: called by the window manager on the server thread. `STATE` is
    // published before `CreateWindowExA`, so it is either null (window not yet
    // created) or the leaked `State` box, which lives for the whole process.
    unsafe {
        let sp = STATE.load(Ordering::Relaxed);
        if sp.is_null() {
            return DefWindowProcA(hwnd, msg, wp, lp);
        }

        match msg {
            // Filename transfer. SoundArc memcpy's cbData bytes verbatim; we
            // clamp and force NUL-termination so a malformed send cannot run off
            // the end of the buffer.
            //
            // SoundArc also flips a `g_strHot` flag here (`0x42788c`) and clears
            // it in every string-consuming opcode, but nothing ever reads it, so
            // it is not reproduced.
            WM_COPYDATA => {
                let cds = lp as *const COPYDATASTRUCT;
                if !cds.is_null() && !(*cds).lpData.is_null() && (*cds).cbData > 0 {
                    let n = ((*cds).cbData as usize).min(STR_BUF - 1);
                    std::ptr::copy_nonoverlapping(
                        (*cds).lpData as *const u8,
                        (&raw mut (*sp).strbuf).cast::<u8>(),
                        n,
                    );
                    (*sp).strbuf[n] = 0;
                }
                DefWindowProcA(hwnd, msg, wp, lp)
            }

            WM_USER => dispatch(sp, hwnd, msg, wp, lp),

            _ => DefWindowProcA(hwnd, msg, wp, lp),
        }
    }
}

/// Execute one `WM_USER` command.
///
/// # Safety
/// `sp` must point to the live `State`.
unsafe fn dispatch(sp: *mut State, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: `sp` is the live `State`; see `wnd_proc`. Every `call!` copies the
    // function pointer out before evaluating arguments, so no borrow of the
    // state is alive once control enters sound3d.
    unsafe {
        let op = wp;

        // wParam < 0x200: argument register write. Floats arrive bit-cast in
        // lParam.
        if op < NUM_ARGS {
            (*sp).args[op] = lp as u32;
            return DefWindowProcA(hwnd, msg, wp, lp);
        }

        // A missing export makes the opcode a silent no-op, matching SoundArc.
        macro_rules! call {
            ($f:ident $(, $a:expr)* $(,)?) => {{
                let func = (*sp).s3d.$f;
                match func { Some(f) => f($($a),*) as LRESULT, None => 0 }
            }};
        }

        match op {
            // ---- meta ------------------------------------------------------
            // 0x401: force ownership.
            0x401 => {
                (*sp).owner = lp;
                DefWindowProcA(hwnd, msg, wp, lp)
            }
            // 0x400: Init + ownership handshake. First caller wins; the return
            // value tells the game whether this server belongs to it.
            0x400 => {
                if (*sp).owner == 0 {
                    (*sp).owner = lp;
                }
                if (*sp).owner == lp && !(*sp).initialized {
                    (*sp).initialized = true;
                    let (init, mix) = ((*sp).s3d.IInit, (*sp).s3d.MixOpen);
                    if let Some(f) = init {
                        f(lp);
                    }
                    if let Some(f) = mix {
                        f();
                    }
                    log(&format!(
                        "[soundarc-inproc] Init(game hwnd = {lp:#x}) complete"
                    ));
                }
                ((*sp).owner == lp) as LRESULT
            }

            // ---- BGM / streamed playback -----------------------------------
            #[rustfmt::skip]
            0x200 => {
                logv!("PlayFade({},{:?},..)", a(sp, 0), cname(sp));
                call!(IPlayFade, a(sp,0), s(sp), a(sp,1), a(sp,2), a(sp,3), a(sp,4))
            }
            #[rustfmt::skip]
            0x202 => call!(IPlayFadeB, a(sp,0), s(sp), a(sp,1), a(sp,2), a(sp,3), a(sp,4)),
            #[rustfmt::skip]
            0x207 => call!(IPlay, a(sp,0), s(sp), a(sp,1), a(sp,2), a(sp,3), a(sp,4)),
            #[rustfmt::skip]
            0x208 => call!(IPlayB, a(sp,0), s(sp), a(sp,1), a(sp,2), a(sp,3), a(sp,4)),
            #[rustfmt::skip]
            0x205 => call!(IPlayFadeEx, a(sp,0), s(sp), a(sp,1), a(sp,2), a(sp,3), a(sp,4), a(sp,5)),
            #[rustfmt::skip]
            0x206 => call!(IPlayFadeBEx, a(sp,0), s(sp), a(sp,1), a(sp,2), a(sp,3), a(sp,4), a(sp,5)),
            0x201 => call!(IFadeOut, a(sp, 0)),
            0x212 => call!(IFadeOutEx, a(sp, 0), a(sp, 1)),
            0x213 => call!(IPlayStop, a(sp, 0)),
            0x21c => call!(IVolFade, a(sp, 0), a(sp, 1), a(sp, 2)),

            // ---- SE --------------------------------------------------------
            #[rustfmt::skip]
            0x203 => {
                logv!("PlaySE({:?},{},{},{})", cname(sp), f(sp, 1), f(sp, 2), f(sp, 3));
                call!(IPlaySE, s(sp), pf(sp,1), pf(sp,2), pf(sp,3), a(sp,4), a(sp,0), f(sp,5))
            }
            #[rustfmt::skip]
            0x204 => call!(IPlaySEB, s(sp), pf(sp,1), pf(sp,2), pf(sp,3), a(sp,4), a(sp,0), f(sp,5)),
            0x20d => {
                logv!("LoadSE({:?})", cname(sp));
                call!(ILoadSE, s(sp))
            }
            0x221 => call!(IStopAllSE),

            // 0x224..0x227 read lParam directly, not the register file.
            0x224 => call!(IStopSE, lp as i32),
            0x225 => call!(IGetPos, lp as i32),
            0x226 => call!(ISetSEVol, lp as i32),
            0x227 => call!(ISetBGMVol, lp as i32),

            // ---- mixer / 3D ------------------------------------------------
            0x230 => call!(ISetMem, a(sp, 0), a(sp, 1)),
            // Ten floats by value; register 9 is the first argument.
            #[rustfmt::skip]
            0x235 => call!(
                ISetListen,
                f(sp,9), f(sp,0), f(sp,1), f(sp,2), f(sp,3),
                f(sp,4), f(sp,5), f(sp,6), f(sp,7), f(sp,8),
            ),
            #[rustfmt::skip]
            0x267 => call!(
                ISetSEPos,
                a(sp,0), pf(sp,1), pf(sp,2), pf(sp,3), pf(sp,4), pf(sp,5), pf(sp,6),
            ),

            // 0x228 is sent by XANADU.exe but has no handler in the shipped
            // SoundArc.dll: it reaches DefWindowProc and is dropped.
            _ => {
                logv!("unhandled opcode {op:#x} (lParam={lp:#x})");
                DefWindowProcA(hwnd, msg, wp, lp)
            }
        }
    }
}

/// The staged filename, for logging only.
///
/// # Safety
/// `sp` must point to the live `State`.
unsafe fn cname(sp: *mut State) -> String {
    // SAFETY: `strbuf` is `STR_BUF` bytes and every write NUL-terminates within
    // it, so the scan stops in bounds. The slice is consumed before returning.
    unsafe {
        let p = (&raw const (*sp).strbuf).cast::<u8>();
        let mut len = 0;
        while len < STR_BUF && *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf8_lossy(std::slice::from_raw_parts(p, len)).into_owned()
    }
}
