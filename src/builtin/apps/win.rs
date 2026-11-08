//! Windows: the packaged (MSIX / Store) apps of `shell:AppsFolder`, such as the new Teams,
//! Calculator or Windows Terminal. They have no Start Menu shortcut, so the `.lnk` scan misses
//! them.
//!
//! windows-sys has no COM interface types, so the few `IShellItem` and `IEnumShellItems` methods
//! used here are declared as vtable prefixes. Every `unsafe` block is a single call through such
//! a vtable (or a plain FFI call) on an object we hold a reference to; references are released
//! by [`Com`] on every return path.

use std::ffi::c_void;
use std::marker::PhantomData;

use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{
    BHID_EnumItems, SHCreateItemFromParsingName, SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
};
use windows_sys::core::{GUID, HRESULT, PWSTR};

/// The parsing name prefix of an app in the Apps folder; followed by its app id.
pub const APPS_FOLDER: &str = r"shell:AppsFolder\";

const IID_ISHELLITEM: GUID = GUID::from_u128(0x43826d1e_e718_42ee_bc55_a1e261c37bfe);
const IID_IENUMSHELLITEMS: GUID = GUID::from_u128(0x70629033_e363_4a28_a567_0db78006e6d7);

#[repr(C)]
struct UnknownVtbl {
    _query_interface: usize,
    _add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

/// The start of `IShellItem`'s vtable, up to the last method used.
#[repr(C)]
struct ShellItemVtbl {
    _base: UnknownVtbl,
    bind_to_handler:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *const GUID, *const GUID, *mut *mut c_void) -> HRESULT,
    _get_parent: usize,
    get_display_name: unsafe extern "system" fn(*mut c_void, SIGDN, *mut PWSTR) -> HRESULT,
}

/// The start of `IEnumShellItems`'s vtable.
#[repr(C)]
struct EnumVtbl {
    _base: UnknownVtbl,
    next: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void, *mut u32) -> HRESULT,
}

/// One reference to a COM object whose vtable starts like `V`; released on drop.
struct Com<V>(*mut c_void, PhantomData<V>);

impl<V> Com<V> {
    /// `None` for a null pointer.
    fn new(p: *mut c_void) -> Option<Self> {
        (!p.is_null()).then_some(Self(p, PhantomData))
    }

    fn vtbl(&self) -> &V {
        // SAFETY: a COM object starts with a pointer to its vtable, which starts like `V`
        // (checked by the IID it was asked for) and lives as long as the object.
        unsafe { &**self.0.cast::<*const V>() }
    }
}

impl<V> Drop for Com<V> {
    fn drop(&mut self) {
        // SAFETY: every vtable starts with IUnknown's; we hold one reference and drop it once.
        unsafe { ((**self.0.cast::<*const UnknownVtbl>()).release)(self.0) };
    }
}

/// The packaged apps as `(name, app id)`, in the folder's order. Empty if the folder cannot be
/// listed.
pub fn packaged_apps() -> Vec<(String, String)> {
    crate::core::icons::win::init_com();
    let mut out = vec![];
    let Some(folder) = apps_folder() else { return out };
    let mut p = std::ptr::null_mut();
    // SAFETY: `folder` is a live IShellItem; `p` receives an IEnumShellItems owned below. The
    // bind context may be null.
    let hr = unsafe {
        (folder.vtbl().bind_to_handler)(
            folder.0,
            std::ptr::null_mut(),
            &BHID_EnumItems,
            &IID_IENUMSHELLITEMS,
            &raw mut p,
        )
    };
    let Some(items) = Com::<EnumVtbl>::new(p).filter(|_| hr >= 0) else { return out };
    loop {
        let (mut p, mut fetched) = (std::ptr::null_mut(), 0u32);
        // SAFETY: `items` is a live IEnumShellItems; one item goes into `p`, owned below.
        let hr = unsafe { (items.vtbl().next)(items.0, 1, &raw mut p, &raw mut fetched) };
        // S_FALSE (1) or an error: no more items.
        if hr != 0 || fetched != 1 {
            break;
        }
        let Some(item) = Com::<ShellItemVtbl>::new(p) else { break };
        // A packaged app's id is `<package family>!<app>`; other entries are desktop apps,
        // which the shortcut scan already finds.
        let Some(id) = display_name(&item, SIGDN_PARENTRELATIVEPARSING).filter(|id| id.contains('!')) else {
            continue;
        };
        if let Some(name) = display_name(&item, SIGDN_NORMALDISPLAY).filter(|n| !n.is_empty()) {
            out.push((name, id));
        }
    }
    out
}

fn apps_folder() -> Option<Com<ShellItemVtbl>> {
    let name: Vec<u16> = "shell:AppsFolder".encode_utf16().chain([0]).collect();
    let mut p = std::ptr::null_mut();
    // SAFETY: `name` is NUL-terminated; `p` receives an IShellItem owned by the result. The bind
    // context may be null.
    let hr = unsafe { SHCreateItemFromParsingName(name.as_ptr(), std::ptr::null_mut(), &IID_ISHELLITEM, &raw mut p) };
    Com::new(p).filter(|_| hr >= 0)
}

fn display_name(item: &Com<ShellItemVtbl>, kind: SIGDN) -> Option<String> {
    let mut s: PWSTR = std::ptr::null_mut();
    // SAFETY: `item` is a live IShellItem; `s` receives a string we free below.
    let hr = unsafe { (item.vtbl().get_display_name)(item.0, kind, &raw mut s) };
    if hr < 0 || s.is_null() {
        return None;
    }
    let mut len = 0;
    // SAFETY: the shell returns a NUL-terminated string; we read up to the NUL.
    while unsafe { *s.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `s` holds `len` initialised UTF-16 units before the NUL.
    let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(s, len) });
    // SAFETY: the string was allocated by the shell with CoTaskMemAlloc and is freed once.
    unsafe { CoTaskMemFree(s.cast_const().cast()) };
    Some(text)
}
