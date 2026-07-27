# xanadu-next-soundarc

Runs **Xanadu Next**'s audio server inside the game process. Vanilla, the game launches
`soundarc.dll` as a second process at high priority and talks to it over window messages.
With the mod there is only `XANADU.exe`, and the audio work happens on a thread inside it.

## Install

1. Grab `dinput8.dll` from the latest release, or build it yourself (see below).
2. Drop it next to `XANADU.exe`.
3. Launch the game. `soundarc_inproc.log` appears next to the exe and says whether the
   server came up.

To uninstall, delete `dinput8.dll` from the game folder. No game file is patched.

## The problem

`SoundArc.dll` is not a DLL. It is a PE32 GUI executable: `type EXEC`, image base
`0x400000`, zero exports, a tray icon and a context menu. `XANADU.exe` starts it with
`CreateProcessA(..., HIGH_PRIORITY_CLASS)` and drives it entirely through window messages:

```text
hwnd = FindWindowA("SOUNDARC", "Xanadu Next")    // else CreateProcess + poll 100x20ms
SendMessageA(hwnd, WM_USER,     n,  value)       // n < 0x200  -> argument register n
SendMessageA(hwnd, WM_COPYDATA, 0,  &cds)        // filename staging buffer
SendMessageA(hwnd, WM_USER,     op, arg)         // op >= 0x200 -> execute opcode
```

There is no shared memory and there are no pipes. SoundArc's only IPC related imports are
`CreateMutexA`, `OpenMutexA` and `ReleaseMutex`, which together form a single instance
guard. SoundArc contains no audio code of its own. Every opcode is a thin forward to a
`GetProcAddress`'d export of `sound3d.dll` (Falcom `SoundMgV3`, 2005: DirectSound8, WINMM
and MSACM, static MFC).

## The fix

`FindWindowA` enumerates top level windows across the whole desktop, including windows
owned by the calling process. So the mod creates a hidden class-`SOUNDARC` window on its
own thread inside `XANADU.exe` and reimplements the protocol against `sound3d.dll` loaded
in-process. The game's own unpatched `FindWindowA` then succeeds on the first attempt and
`CreateProcessA` is never reached.

`FindWindowA` is also hooked in the exe's import table, so the result does not depend on
winning a startup race.

The window is created with `hWndParent = NULL`. That part matters. A message only window
(`HWND_MESSAGE`) is not discoverable by `FindWindowA`.

If `sound3d.dll` cannot be loaded, the hook falls through to the real `FindWindowA` and the
game behaves exactly as it does unmodded, spawning the original process.

The DLL loads as a `dinput8.dll` proxy. `XANADU.exe` statically imports
`DirectInput8Create`, so the DLL is in memory before `WinMain`. All six exports of the real
32-bit `dinput8.dll` are forwarded to `SysWOW64\dinput8.dll` at the same ordinals.

## Protocol notes

These came out of `SoundArc.dll`'s dispatcher at `0x401420` and matter for correctness:

- Floats are bit cast into `lParam` by the game and stored raw in the argument register
  file.
- `IPlaySE`, `IPlaySEB` and `ISetSEPos` take positional floats by pointer (C++ `float&`)
  straight into that register file. `ISetListen` takes its ten floats by value. The
  asymmetry is real, and the `add esp, N` after each call pins it down.
- `ISetListen`'s first argument is register 9, followed by registers 0-8.
- Opcodes `0x224` to `0x227` read `lParam` directly, not the register file.
- Opcode `0x228` is sent by the game but has no handler in the shipped `SoundArc.dll`. It
  reaches `DefWindowProc` and is dropped. Reproduced as is.
- Every opcode is guarded by a null check on its function pointer, so a missing export
  makes that opcode a silent no-op, matching SoundArc.
- Init order is `LoadLibrary(<gamedir>\sound3d.dll)`, resolve, `IInit(gameHwnd)`,
  `MixOpen()`, driven by the `0x400` opcode.

## Logging

`soundarc_inproc.log` is written next to the game exe on every launch. A working start
looks like this:

```text
[soundarc-inproc] in-process SOUNDARC server ready (no child process will be spawned)
[soundarc-inproc] Init(game hwnd = 0x...) complete
```

Set `SOUNDARC_INPROC_LOG=1` for per command tracing (`PlaySE`, `LoadSE`, `PlayFade`, and
any unhandled opcode).

To confirm the mod is doing its job, open Task Manager and check that no second process
appears alongside `XANADU.exe`. Unmodded, `soundarc.dll` shows up as its own process.

## Building

You need rustup and the MSVC build tools. Everything else is pinned in the repo:
`rust-toolchain.toml` fixes the compiler version and pulls the 32-bit
`i686-pc-windows-msvc` target (the game is 32-bit), and `.cargo/config.toml` selects that
target and links the CRT statically.

```sh
cargo build --release
```

The first build downloads the pinned toolchain if you do not already have it.

The DLL ends up in `target/i686-pc-windows-msvc/release/dinput8.dll`. CI builds every push
and attaches the DLL to tagged releases (`v*`).

## Code layout

| Files | Concern |
|---|---|
| `src/lib.rs` | DLL entry point, `FindWindowA` hook install |
| `src/server.rs` | `SOUNDARC` window, message pump, `WM_USER` opcode dispatch |
| `src/sound3d.rs` | `sound3d.dll` bindings and export resolution |
| `src/proxy.rs` | `dinput8` export forwarding to the system DLL, `.def` with pinned ordinals |
| `src/iat.rs` | import table slot swapping |
| `src/util.rs` | logging, path and string helpers |

## License

MIT, see [LICENSE](LICENSE).
