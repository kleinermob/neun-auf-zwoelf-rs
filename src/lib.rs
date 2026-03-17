//! d3d9.dll — D3D9on12 shim
//!
//! ===========================================================================
//! What this does
//! ===========================================================================
//!
//! This DLL acts as a drop-in replacement for d3d9.dll. It intercepts
//! Direct3DCreate9 and Direct3DCreate9Ex and redirects them to
//! Direct3DCreate9On12 / Direct3DCreate9On12Ex in the *real* system d3d9.dll
//! with Enable9On12 = TRUE. This forces any D3D9 game to use the D3D9on12
//! translation layer, which renders via D3D12 internally.
//!
//! The practical result: games that only support D3D9 (e.g. CS:GO Legacy)
//! gain a proper DXGI swap chain, which allows DXGI-level hooks such as
//! FPS overlays and vsync-off proxies to work on top of them.
//!
//! All other d3d9.dll exports are forwarded transparently to the real DLL.
//!
//! ===========================================================================
//! Ordinal reference (must match real system d3d9.dll exactly)
//! ===========================================================================
//!
//! Ordinals are used by some loaders and engines instead of names.
//! Source engine loads d3d9 exports by name, but we keep ordinals correct
//! for maximum compatibility.
//!
//!   1  Direct3DCreate9
//!   2  Direct3DCreate9Ex
//!  15  Direct3DShaderValidatorCreate9
//!  20  Direct3DCreate9On12
//!  21  Direct3DCreate9On12Ex
//!  25  PSGPError
//!  26  PSGPSampleTexture
//!  27  D3DPERF_BeginEvent
//!  28  D3DPERF_EndEvent
//!  29  D3DPERF_GetStatus
//!  30  D3DPERF_QueryRepeatFrame
//!  31  D3DPERF_SetMarker
//!  32  D3DPERF_SetOptions
//!  33  D3DPERF_SetRegion
//!  34  DebugSetLevel
//!  35  DebugSetMute
//!  36  Direct3D9EnableMaximizedWindowedModeShim

#![allow(non_snake_case, non_camel_case_types)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows::Win32::Foundation::{BOOL, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::core::{HRESULT, PCSTR};

// ---------------------------------------------------------------------------
// D3D9 / D3D9on12 raw types
// ---------------------------------------------------------------------------
//
// We define only what we need as raw pointers — we never look inside these
// structures, just pass them through to the real DLL.

type IDirect3D9Ptr = *mut core::ffi::c_void;
type IDirect3D9ExPtr = *mut core::ffi::c_void;
type D3DCOLOR = u32;

/// D3D9ON12_ARGS — matches the SDK struct layout exactly.
/// We only need Enable9On12; all other fields are zeroed.
#[repr(C)]
struct D3D9ON12_ARGS {
    Enable9On12: BOOL,
    pD3D12Device: *mut core::ffi::c_void,
    ppD3D12Queues: *mut *mut core::ffi::c_void,
    NumQueues: u32,
    NodeMask: u32,
}

impl D3D9ON12_ARGS {
    fn enabled() -> Self {
        Self {
            Enable9On12: BOOL(1),
            pD3D12Device: std::ptr::null_mut(),
            ppD3D12Queues: std::ptr::null_mut(),
            NumQueues: 0,
            NodeMask: 0,
        }
    }
}

// Opaque forward-declared types — we only ever pass pointers to these.
#[repr(C)] struct D3DFE_PROCESSVERTICES { _opaque: u8 }
#[repr(C)] struct IDirect3DShaderValidator9 { _opaque: u8 }

// ---------------------------------------------------------------------------
// Real d3d9.dll proc types
// ---------------------------------------------------------------------------

type FnDirect3DCreate9On12 = unsafe extern "system" fn(
    sdk_version: u32,
    p_override_list: *mut D3D9ON12_ARGS,
    num_override_entries: u32,
) -> IDirect3D9Ptr;

type FnDirect3DCreate9On12Ex = unsafe extern "system" fn(
    sdk_version: u32,
    p_override_list: *mut D3D9ON12_ARGS,
    num_override_entries: u32,
    pp_direct3d9ex: *mut IDirect3D9ExPtr,
) -> HRESULT;

type FnDirect3DCreate9 = unsafe extern "system" fn(sdk_version: u32) -> IDirect3D9Ptr;
type FnDirect3DCreate9Ex = unsafe extern "system" fn(sdk_version: u32, pp: *mut IDirect3D9ExPtr) -> HRESULT;

type FnD3DPERF_BeginEvent = unsafe extern "system" fn(col: D3DCOLOR, name: *const u16) -> i32;
type FnD3DPERF_EndEvent = unsafe extern "system" fn() -> i32;
type FnD3DPERF_SetMarker = unsafe extern "system" fn(col: D3DCOLOR, name: *const u16);
type FnD3DPERF_SetRegion = unsafe extern "system" fn(col: D3DCOLOR, name: *const u16);
type FnD3DPERF_QueryRepeatFrame = unsafe extern "system" fn() -> BOOL;
type FnD3DPERF_SetOptions = unsafe extern "system" fn(options: u32);
type FnD3DPERF_GetStatus = unsafe extern "system" fn() -> u32;
type FnDebugSetMute = unsafe extern "system" fn();
type FnDebugSetLevel = unsafe extern "system" fn() -> i32;
type FnPSGPError = unsafe extern "system" fn(
    a: *mut D3DFE_PROCESSVERTICES, b: u32, c: u32);
type FnPSGPSampleTexture = unsafe extern "system" fn(
    a: *mut D3DFE_PROCESSVERTICES,
    b: u32,
    c: *const [f32; 4],
    d: u32,
    e: *const [f32; 4]);
type FnDirect3DShaderValidatorCreate9 =
    unsafe extern "system" fn() -> *mut IDirect3DShaderValidator9;
type FnDirect3D9EnableMaximizedWindowedModeShim =
    unsafe extern "system" fn(a: u32) -> i32;

// ---------------------------------------------------------------------------
// Real DLL handle + proc cache
// ---------------------------------------------------------------------------

static REAL_D3D9: AtomicUsize = AtomicUsize::new(0);

/// Load the real system d3d9.dll once and cache the handle.
unsafe fn real_d3d9() -> Option<HMODULE> {
    let raw = REAL_D3D9.load(Ordering::SeqCst);
    if raw != 0 {
        return Some(HMODULE(raw as *mut core::ffi::c_void));
    }
    None
}

/// Resolve a named export from the real d3d9.dll.
/// Returns None if the DLL isn't loaded or the export doesn't exist.
/// Panics in debug builds, returns None in release so we fail gracefully.
unsafe fn resolve<T>(name: &[u8]) -> Option<T> {
    let h = real_d3d9()?;
    let proc = GetProcAddress(h, PCSTR(name.as_ptr()))?;
    Some(std::mem::transmute_copy(&proc))
}

/// Resolve a proc, cache it in a OnceLock, and call it.
/// Usage: call_proc!(b"ProcName\0", FnType, arg1, arg2, ...)
macro_rules! call_proc {
    ($name:expr, $fn_type:ty $(, $arg:expr)*) => {{
        static PROC: OnceLock<$fn_type> = OnceLock::new();
        let f = PROC.get_or_init(|| unsafe {
            resolve::<$fn_type>($name)
                .unwrap_or_else(|| panic!("d3d9on12 shim: {} not found in real d3d9.dll",
                    std::str::from_utf8($name).unwrap_or("?")))
        });
        unsafe { f($($arg),*) }
    }};
}

// ---------------------------------------------------------------------------
// Exported functions
// ---------------------------------------------------------------------------

/// Direct3DCreate9 — redirected to Direct3DCreate9On12 with Enable9On12=TRUE.
///
/// DEVNOTE: This is the key interception point. The game calls Direct3DCreate9
/// expecting a plain D3D9 device. We silently upgrade it to a D3D9on12 device
/// which internally creates a D3D12 device and a proper DXGI swap chain.
/// This allows DXGI-level hooks (present hooks, overlay DLLs) to fire on top.
#[no_mangle]
pub unsafe extern "system" fn Direct3DCreate9(sdk_version: u32) -> IDirect3D9Ptr {
    let mut arg = D3D9ON12_ARGS::enabled();
    call_proc!(
        b"Direct3DCreate9On12\0",
        FnDirect3DCreate9On12,
        sdk_version,
        &mut arg as *mut D3D9ON12_ARGS,
        1
    )
}

/// Direct3DCreate9Ex — redirected to Direct3DCreate9On12Ex with Enable9On12=TRUE.
#[no_mangle]
pub unsafe extern "system" fn Direct3DCreate9Ex(
    sdk_version: u32,
    pp: *mut IDirect3D9ExPtr,
) -> HRESULT {
    let mut arg = D3D9ON12_ARGS::enabled();
    call_proc!(
        b"Direct3DCreate9On12Ex\0",
        FnDirect3DCreate9On12Ex,
        sdk_version,
        &mut arg as *mut D3D9ON12_ARGS,
        1,
        pp
    )
}

/// Direct3DCreate9On12 — forwarded directly, no interception needed.
#[no_mangle]
pub unsafe extern "system" fn Direct3DCreate9On12(
    sdk_version: u32,
    p_override_list: *mut D3D9ON12_ARGS,
    num_override_entries: u32,
) -> IDirect3D9Ptr {
    call_proc!(
        b"Direct3DCreate9On12\0",
        FnDirect3DCreate9On12,
        sdk_version,
        p_override_list,
        num_override_entries
    )
}

/// Direct3DCreate9On12Ex — forwarded directly.
#[no_mangle]
pub unsafe extern "system" fn Direct3DCreate9On12Ex(
    sdk_version: u32,
    p_override_list: *mut D3D9ON12_ARGS,
    num_override_entries: u32,
    pp: *mut IDirect3D9ExPtr,
) -> HRESULT {
    call_proc!(
        b"Direct3DCreate9On12Ex\0",
        FnDirect3DCreate9On12Ex,
        sdk_version,
        p_override_list,
        num_override_entries,
        pp
    )
}

// ---------------------------------------------------------------------------
// D3DPERF forwarding
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "system" fn D3DPERF_BeginEvent(col: D3DCOLOR, name: *const u16) -> i32 {
    call_proc!(b"D3DPERF_BeginEvent\0", FnD3DPERF_BeginEvent, col, name)
}

#[no_mangle]
pub unsafe extern "system" fn D3DPERF_EndEvent() -> i32 {
    call_proc!(b"D3DPERF_EndEvent\0", FnD3DPERF_EndEvent)
}

#[no_mangle]
pub unsafe extern "system" fn D3DPERF_SetMarker(col: D3DCOLOR, name: *const u16) {
    call_proc!(b"D3DPERF_SetMarker\0", FnD3DPERF_SetMarker, col, name)
}

#[no_mangle]
pub unsafe extern "system" fn D3DPERF_SetRegion(col: D3DCOLOR, name: *const u16) {
    call_proc!(b"D3DPERF_SetRegion\0", FnD3DPERF_SetRegion, col, name)
}

#[no_mangle]
pub unsafe extern "system" fn D3DPERF_QueryRepeatFrame() -> BOOL {
    call_proc!(b"D3DPERF_QueryRepeatFrame\0", FnD3DPERF_QueryRepeatFrame)
}

#[no_mangle]
pub unsafe extern "system" fn D3DPERF_SetOptions(options: u32) {
    call_proc!(b"D3DPERF_SetOptions\0", FnD3DPERF_SetOptions, options)
}

/// DEVNOTE: Fixed from original — was incorrectly calling "D3DPERF_SetOptions"
#[no_mangle]
pub unsafe extern "system" fn D3DPERF_GetStatus() -> u32 {
    call_proc!(b"D3DPERF_GetStatus\0", FnD3DPERF_GetStatus)
}

// ---------------------------------------------------------------------------
// Debug / internal forwarding
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "system" fn DebugSetMute() {
    call_proc!(b"DebugSetMute\0", FnDebugSetMute)
}

#[no_mangle]
pub unsafe extern "system" fn DebugSetLevel() -> i32 {
    call_proc!(b"DebugSetLevel\0", FnDebugSetLevel)
}

#[no_mangle]
pub unsafe extern "system" fn PSGPError(
    a: *mut D3DFE_PROCESSVERTICES, b: u32, c: u32,
) {
    call_proc!(b"PSGPError\0", FnPSGPError, a, b, c)
}

/// DEVNOTE: Fixed from original — was incorrectly calling "PSGPError"
#[no_mangle]
pub unsafe extern "system" fn PSGPSampleTexture(
    a: *mut D3DFE_PROCESSVERTICES,
    b: u32,
    c: *const [f32; 4],
    d: u32,
    e: *const [f32; 4],
) {
    call_proc!(b"PSGPSampleTexture\0", FnPSGPSampleTexture, a, b, c, d, e)
}

#[no_mangle]
pub unsafe extern "system" fn Direct3DShaderValidatorCreate9(
) -> *mut IDirect3DShaderValidator9 {
    call_proc!(
        b"Direct3DShaderValidatorCreate9\0",
        FnDirect3DShaderValidatorCreate9
    )
}

#[no_mangle]
pub unsafe extern "system" fn Direct3D9EnableMaximizedWindowedModeShim(a: u32) -> i32 {
    call_proc!(
        b"Direct3D9EnableMaximizedWindowedModeShim\0",
        FnDirect3D9EnableMaximizedWindowedModeShim,
        a
    )
}

// ---------------------------------------------------------------------------
// DllMain
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "system" fn DllMain(
    _hinst: HMODULE,
    reason: u32,
    _reserved: *mut core::ffi::c_void,
) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        // Load the real system d3d9.dll.
        // We must load it from System32 explicitly — if we just call
        // LoadLibraryW("d3d9.dll") Windows will find *us* first and recurse.
        let mut buf = [0u16; 512];
        let len = GetSystemDirectoryW(Some(&mut buf)) as usize;
        if len == 0 {
            return BOOL(0);
        }

        // Append \d3d9.dll
        let suffix: Vec<u16> = OsStr::new("\\d3d9.dll")
            .encode_wide()
            .chain(std::iter::once(0u16))
            .collect();

        if len + suffix.len() > buf.len() {
            return BOOL(0);
        }

        buf[len..len + suffix.len()].copy_from_slice(&suffix);

        let h = match LoadLibraryW(windows::core::PCWSTR(buf.as_ptr())) {
            Ok(h) => h,
            Err(_) => return BOOL(0),
        };

        REAL_D3D9.store(h.0 as usize, Ordering::SeqCst);
    }

    BOOL(1)
}
