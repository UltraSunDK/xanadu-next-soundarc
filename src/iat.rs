//! Minimal import-address-table hook for the main executable.
//!
//! Patches `XANADU.exe`'s IAT entry for `user32!FindWindowA`. By the time a
//! statically-imported DLL's `DllMain` runs the loader has already snapped the
//! executable's thunks, so the entry is present and safe to rewrite.
//!
//! The PE structures are declared here rather than pulled from `windows-sys`;
//! they are tiny, and this keeps the module off that crate's feature-gated PE
//! modules.

use std::ffi::c_void;

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};

const IMAGE_DOS_SIGNATURE: u16 = 0x5A4D; // "MZ"
const IMAGE_NT_SIGNATURE: u32 = 0x0000_4550; // "PE\0\0"
const IMAGE_DIRECTORY_ENTRY_IMPORT: usize = 1;

#[repr(C)]
struct ImageDosHeader {
    e_magic: u16,
    _pad: [u16; 29],
    e_lfanew: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ImageDataDirectory {
    virtual_address: u32,
    size: u32,
}

#[repr(C)]
struct ImageImportDescriptor {
    original_first_thunk: u32,
    time_date_stamp: u32,
    forwarder_chain: u32,
    name: u32,
    first_thunk: u32,
}

/// Replace every IAT slot in the main module whose value equals `target` with
/// `replacement`. Returns the number of slots patched.
///
/// Matching on the resolved address rather than on the import name works
/// whether the function was imported by name or by ordinal, and covers the
/// entry appearing under more than one import descriptor.
///
/// # Safety
/// `target` and `replacement` must be valid code addresses.
pub unsafe fn patch_iat(target: usize, replacement: usize) -> usize {
    // SAFETY: every dereference below is gated on the structure that precedes it:
    // the DOS magic before `e_lfanew`, the PE signature before the optional
    // header, the directory count before the import directory, and a non-zero
    // RVA before the descriptor walk. Both descriptor and thunk arrays are
    // NUL-terminated by the PE format. Slots are only written after
    // `VirtualProtect` reports success, and the original protection is restored.
    unsafe {
        let base = GetModuleHandleA(std::ptr::null()) as usize;
        if base == 0 {
            return 0;
        }

        let dos = base as *const ImageDosHeader;
        if (*dos).e_magic != IMAGE_DOS_SIGNATURE {
            return 0;
        }
        let nt = base + (*dos).e_lfanew as usize;
        if *(nt as *const u32) != IMAGE_NT_SIGNATURE {
            return 0;
        }

        // Offsets into IMAGE_NT_HEADERS32: signature(4) + FileHeader(20) = 24 to
        // OptionalHeader; the 32-bit optional header puts DataDirectory at +96.
        let opt = nt + 24;
        let num_dirs = *((opt + 92) as *const u32) as usize;
        if num_dirs <= IMAGE_DIRECTORY_ENTRY_IMPORT {
            return 0;
        }
        let dirs = (opt + 96) as *const ImageDataDirectory;
        let imp_dir = *dirs.add(IMAGE_DIRECTORY_ENTRY_IMPORT);
        if imp_dir.virtual_address == 0 {
            return 0;
        }

        let mut desc = (base + imp_dir.virtual_address as usize) as *const ImageImportDescriptor;
        let mut patched = 0usize;

        while (*desc).name != 0 && (*desc).first_thunk != 0 {
            let mut thunk = (base + (*desc).first_thunk as usize) as *mut usize;
            while *thunk != 0 {
                if *thunk == target {
                    let mut old = 0u32;
                    if VirtualProtect(
                        thunk as *const c_void,
                        size_of::<usize>(),
                        PAGE_READWRITE,
                        &mut old,
                    ) != 0
                    {
                        *thunk = replacement;
                        let mut tmp = 0u32;
                        VirtualProtect(thunk as *const c_void, size_of::<usize>(), old, &mut tmp);
                        patched += 1;
                    }
                }
                thunk = thunk.add(1);
            }
            desc = desc.add(1);
        }
        patched
    }
}
