//! Windows: project a [`VirtualTree`] into a directory with the Projected File System
//! (ProjFS), the mechanism behind VFS for Git and EdenFS on Windows. Files appear at once as
//! placeholders; Windows asks for a file's contents on first read and keeps them on disk.
//!
//! ProjFS is an optional Windows feature, so its library is loaded at run time: `gd`
//! starts on machines without it, and `gd mount` explains how to enable it. Every
//! callback runs inside `catch_unwind`, because a panic must never cross into Windows.
//! Symlinks are projected as small files holding the link target, as git does on Windows
//! without symlink rights. Files are marked read-only; local edits are not tracked.

use std::collections::HashMap;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use grovedock_core::object::Mode;
use windows_sys::Win32::Storage::ProjectedFileSystem::{
    PRJ_CALLBACK_DATA, PRJ_CALLBACKS, PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN, PRJ_CB_DATA_FLAG_ENUM_RETURN_SINGLE_ENTRY,
    PRJ_DIR_ENTRY_BUFFER_HANDLE, PRJ_FILE_BASIC_INFO, PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, PRJ_PLACEHOLDER_INFO,
    PRJ_PLACEHOLDER_VERSION_INFO, PRJ_STARTVIRTUALIZING_OPTIONS,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::core::{GUID, HRESULT, PCWSTR};

use super::{Attr, ROOT, VirtualTree};

const S_OK: HRESULT = 0;
const E_FAIL: HRESULT = 0x8000_4005_u32 as i32;
const E_OUTOFMEMORY: HRESULT = 0x8007_000E_u32 as i32;
const E_FILE_NOT_FOUND: HRESULT = 0x8007_0002_u32 as i32;
const E_INSUFFICIENT_BUFFER: HRESULT = 0x8007_007A_u32 as i32;
const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
/// Largest piece written per `PrjWriteFileData` call.
const WRITE_CHUNK: u64 = 4 << 20;

type MarkFn = unsafe extern "system" fn(PCWSTR, PCWSTR, *const PRJ_PLACEHOLDER_VERSION_INFO, *const GUID) -> HRESULT;
type StartFn = unsafe extern "system" fn(
    PCWSTR,
    *const PRJ_CALLBACKS,
    *const c_void,
    *const PRJ_STARTVIRTUALIZING_OPTIONS,
    *mut PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
) -> HRESULT;
type StopFn = unsafe extern "system" fn(PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT);
type FillFn = unsafe extern "system" fn(PCWSTR, *const PRJ_FILE_BASIC_INFO, PRJ_DIR_ENTRY_BUFFER_HANDLE) -> HRESULT;
type PlaceholderFn = unsafe extern "system" fn(
    PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
    PCWSTR,
    *const PRJ_PLACEHOLDER_INFO,
    u32,
) -> HRESULT;
type AllocFn = unsafe extern "system" fn(PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, usize) -> *mut c_void;
type FreeFn = unsafe extern "system" fn(*const c_void);
type WriteFn =
    unsafe extern "system" fn(PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, *const GUID, *const c_void, u64, u32) -> HRESULT;
type CompareFn = unsafe extern "system" fn(PCWSTR, PCWSTR) -> i32;
type MatchFn = unsafe extern "system" fn(PCWSTR, PCWSTR) -> bool;

/// ProjectedFSLib.dll, resolved at run time.
struct Api {
    mark: MarkFn,
    start: StartFn,
    stop: StopFn,
    fill: FillFn,
    placeholder: PlaceholderFn,
    alloc: AllocFn,
    free: FreeFn,
    write: WriteFn,
    compare: CompareFn,
    matches: MatchFn,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn from_wide(p: PCWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

impl Api {
    fn load() -> Result<Api> {
        let lib = unsafe { LoadLibraryW(wide("ProjectedFSLib.dll").as_ptr()) };
        if lib.is_null() {
            bail!(
                "Windows Projected File System is not enabled. Enable it from an administrator \
                 PowerShell with `Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS`"
            );
        }
        macro_rules! sym {
            ($name:literal) => {{
                let f = unsafe { GetProcAddress(lib, concat!($name, "\0").as_ptr()) }
                    .with_context(|| format!("ProjectedFSLib.dll has no {}", $name))?;
                // SAFETY: the symbol is the documented ProjFS function with this signature.
                unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, _>(f) }
            }};
        }
        Ok(Api {
            mark: sym!("PrjMarkDirectoryAsPlaceholder"),
            start: sym!("PrjStartVirtualizing"),
            stop: sym!("PrjStopVirtualizing"),
            fill: sym!("PrjFillDirEntryBuffer"),
            placeholder: sym!("PrjWritePlaceholderInfo"),
            alloc: sym!("PrjAllocateAlignedBuffer"),
            free: sym!("PrjFreeAlignedBuffer"),
            write: sym!("PrjWriteFileData"),
            compare: sym!("PrjFileNameCompare"),
            matches: sym!("PrjFileNameMatch"),
        })
    }
}

/// One directory listing in progress: entries in ProjFS order, how far ProjFS has read, and
/// the search pattern it asked for.
struct Enumeration {
    entries: Vec<(Vec<u16>, Attr)>,
    next: usize,
    pattern: Option<Vec<u16>>,
}

struct Projection {
    tree: Arc<VirtualTree>,
    api: Api,
    callbacks: PRJ_CALLBACKS,
    enumerations: Mutex<HashMap<u128, Enumeration>>,
    /// FILETIME of the mount, used for every timestamp.
    time: i64,
}

fn key(g: &GUID) -> u128 {
    (g.data1 as u128) << 96 | (g.data2 as u128) << 80 | (g.data3 as u128) << 64 | u64::from_be_bytes(g.data4) as u128
}

impl Projection {
    /// Resolve a path relative to the mount root (backslashes, any case) to its canonical
    /// form and attributes.
    fn resolve(&self, rel: &str) -> Result<Option<(String, Attr)>> {
        let mut ino = ROOT;
        let mut canonical = Vec::new();
        for part in rel.split(['\\', '/']).filter(|p| !p.is_empty()) {
            let want = part.to_lowercase();
            match self.tree.readdir(ino)?.into_iter().find(|(_, name, _)| name.to_lowercase() == want) {
                Some((child, name, _)) => {
                    ino = child;
                    canonical.push(name);
                }
                None => return Ok(None),
            }
        }
        Ok(Some((canonical.join("\\"), self.tree.attr(ino)?)))
    }

    fn basic(&self, a: &Attr) -> PRJ_FILE_BASIC_INFO {
        let dir = a.mode == Mode::Dir;
        PRJ_FILE_BASIC_INFO {
            IsDirectory: dir,
            FileSize: if dir { 0 } else { a.size as i64 },
            CreationTime: self.time,
            LastAccessTime: self.time,
            LastWriteTime: self.time,
            ChangeTime: self.time,
            FileAttributes: if dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_READONLY },
        }
    }

    fn start_enumeration(&self, data: &PRJ_CALLBACK_DATA, id: &GUID) -> Result<HRESULT> {
        let rel = unsafe { from_wide(data.FilePathName) };
        let Some((_, dir)) = self.resolve(&rel)? else { return Ok(E_FILE_NOT_FOUND) };
        let mut entries = Vec::new();
        for (ino, name, _) in self.tree.readdir(dir.ino)? {
            entries.push((wide(&name), self.tree.attr(ino)?));
        }
        // ProjFS requires entries in its own collation order.
        entries.sort_by(|a, b| unsafe { (self.api.compare)(a.0.as_ptr(), b.0.as_ptr()) }.cmp(&0));
        self.enumerations
            .lock()
            .expect("enumerations")
            .insert(key(id), Enumeration { entries, next: 0, pattern: None });
        Ok(S_OK)
    }

    fn get_enumeration(
        &self,
        data: &PRJ_CALLBACK_DATA,
        id: &GUID,
        search: PCWSTR,
        buffer: PRJ_DIR_ENTRY_BUFFER_HANDLE,
    ) -> Result<HRESULT> {
        let mut all = self.enumerations.lock().expect("enumerations");
        let Some(e) = all.get_mut(&key(id)) else { return Ok(E_FAIL) };
        if data.Flags & PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN != 0 {
            e.next = 0;
            e.pattern = None;
        }
        if e.pattern.is_none() && !search.is_null() {
            let p = unsafe { from_wide(search) };
            e.pattern = Some(wide(if p.is_empty() { "*" } else { &p }));
        }
        let mut added = 0;
        while e.next < e.entries.len() {
            let (name, attr) = &e.entries[e.next];
            if let Some(p) = &e.pattern
                && !unsafe { (self.api.matches)(name.as_ptr(), p.as_ptr()) }
            {
                e.next += 1;
                continue;
            }
            let info = self.basic(attr);
            let hr = unsafe { (self.api.fill)(name.as_ptr(), &info, buffer) };
            if hr == E_INSUFFICIENT_BUFFER {
                // The buffer is full; ProjFS calls again for the rest.
                return Ok(if added == 0 { hr } else { S_OK });
            }
            if hr < 0 {
                return Ok(hr);
            }
            e.next += 1;
            added += 1;
            if data.Flags & PRJ_CB_DATA_FLAG_ENUM_RETURN_SINGLE_ENTRY != 0 {
                break;
            }
        }
        Ok(S_OK)
    }

    fn placeholder(&self, data: &PRJ_CALLBACK_DATA) -> Result<HRESULT> {
        let rel = unsafe { from_wide(data.FilePathName) };
        let Some((canonical, attr)) = self.resolve(&rel)? else { return Ok(E_FILE_NOT_FOUND) };
        let info = PRJ_PLACEHOLDER_INFO { FileBasicInfo: self.basic(&attr), ..Default::default() };
        let name = wide(&canonical);
        Ok(unsafe {
            (self.api.placeholder)(
                data.NamespaceVirtualizationContext,
                name.as_ptr(),
                &info,
                size_of::<PRJ_PLACEHOLDER_INFO>() as u32,
            )
        })
    }

    fn file_data(&self, data: &PRJ_CALLBACK_DATA, offset: u64, length: u32) -> Result<HRESULT> {
        let rel = unsafe { from_wide(data.FilePathName) };
        let Some((_, attr)) = self.resolve(&rel)? else { return Ok(E_FILE_NOT_FOUND) };
        let ctx = data.NamespaceVirtualizationContext;
        let end = offset + length as u64;
        let mut pos = offset;
        while pos < end {
            let n = (end - pos).min(WRITE_CHUNK) as u32;
            let bytes = self.tree.read(attr.ino, pos, n)?;
            if bytes.is_empty() {
                break;
            }
            let buf = unsafe { (self.api.alloc)(ctx, bytes.len()) };
            if buf.is_null() {
                return Ok(E_OUTOFMEMORY);
            }
            let hr = unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast(), bytes.len());
                let hr = (self.api.write)(ctx, &data.DataStreamId, buf, pos, bytes.len() as u32);
                (self.api.free)(buf);
                hr
            };
            if hr < 0 {
                return Ok(hr);
            }
            pos += bytes.len() as u64;
        }
        Ok(S_OK)
    }
}

/// Run a callback body: errors become E_FAIL and panics never cross into Windows.
fn guarded(f: impl FnOnce() -> Result<HRESULT>) -> HRESULT {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(hr)) => hr,
        Ok(Err(e)) => {
            eprintln!("gd mount: {e:#}");
            E_FAIL
        }
        Err(_) => E_FAIL,
    }
}

unsafe fn projection<'a>(data: *const PRJ_CALLBACK_DATA) -> (&'a Projection, &'a PRJ_CALLBACK_DATA) {
    let data = unsafe { &*data };
    (unsafe { &*(data.InstanceContext as *const Projection) }, data)
}

unsafe extern "system" fn on_start_enumeration(data: *const PRJ_CALLBACK_DATA, id: *const GUID) -> HRESULT {
    guarded(|| {
        let (p, d) = unsafe { projection(data) };
        p.start_enumeration(d, unsafe { &*id })
    })
}

unsafe extern "system" fn on_end_enumeration(data: *const PRJ_CALLBACK_DATA, id: *const GUID) -> HRESULT {
    guarded(|| {
        let (p, _) = unsafe { projection(data) };
        p.enumerations.lock().expect("enumerations").remove(&key(unsafe { &*id }));
        Ok(S_OK)
    })
}

unsafe extern "system" fn on_get_enumeration(
    data: *const PRJ_CALLBACK_DATA,
    id: *const GUID,
    search: PCWSTR,
    buffer: PRJ_DIR_ENTRY_BUFFER_HANDLE,
) -> HRESULT {
    guarded(|| {
        let (p, d) = unsafe { projection(data) };
        p.get_enumeration(d, unsafe { &*id }, search, buffer)
    })
}

unsafe extern "system" fn on_placeholder(data: *const PRJ_CALLBACK_DATA) -> HRESULT {
    guarded(|| {
        let (p, d) = unsafe { projection(data) };
        p.placeholder(d)
    })
}

unsafe extern "system" fn on_file_data(data: *const PRJ_CALLBACK_DATA, offset: u64, length: u32) -> HRESULT {
    guarded(|| {
        let (p, d) = unsafe { projection(data) };
        p.file_data(d, offset, length)
    })
}

/// A running projection; dropping it stops virtualization. Files already read stay on disk.
pub struct Session {
    ctx: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
    projection: *mut Projection,
}

// SAFETY: the context is an opaque handle ProjFS allows using from any thread, and the
// projection is only freed after virtualization stops.
unsafe impl Send for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            ((*self.projection).api.stop)(self.ctx);
            drop(Box::from_raw(self.projection));
        }
    }
}

/// Project `tree` into `dir`, which must be an empty directory never used for a mount.
pub fn mount(tree: Arc<VirtualTree>, dir: &Path) -> Result<Session> {
    let api = Api::load()?;
    std::fs::create_dir_all(dir)?;
    anyhow::ensure!(std::fs::read_dir(dir)?.next().is_none(), "{} must be empty", dir.display());
    let root = wide(&dir.canonicalize()?.to_string_lossy());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    // FILETIME: 100 ns ticks since 1601-01-01.
    let time = (now.as_nanos() / 100) as i64 + 116_444_736_000_000_000;
    let seed =
        grovedock_core::Id::hash(0, format!("{}{}{}", dir.display(), now.as_nanos(), std::process::id()).as_bytes());
    let b = &seed.0;
    let instance = GUID {
        data1: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        data2: u16::from_le_bytes([b[4], b[5]]),
        data3: u16::from_le_bytes([b[6], b[7]]),
        data4: [b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]],
    };
    let hr = unsafe { (api.mark)(root.as_ptr(), std::ptr::null(), std::ptr::null(), &instance) };
    if hr < 0 {
        bail!("could not prepare {} for projection (HRESULT {hr:#x}); use a new, empty directory", dir.display());
    }
    let callbacks = PRJ_CALLBACKS {
        StartDirectoryEnumerationCallback: Some(on_start_enumeration),
        EndDirectoryEnumerationCallback: Some(on_end_enumeration),
        GetDirectoryEnumerationCallback: Some(on_get_enumeration),
        GetPlaceholderInfoCallback: Some(on_placeholder),
        GetFileDataCallback: Some(on_file_data),
        QueryFileNameCallback: None,
        NotificationCallback: None,
        CancelCommandCallback: None,
    };
    let projection = Box::into_raw(Box::new(Projection { tree, api, callbacks, enumerations: Mutex::default(), time }));
    let mut ctx: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT = std::ptr::null_mut();
    let hr = unsafe {
        ((*projection).api.start)(
            root.as_ptr(),
            &(*projection).callbacks,
            projection.cast(),
            std::ptr::null(),
            &mut ctx,
        )
    };
    if hr < 0 {
        drop(unsafe { Box::from_raw(projection) });
        bail!("could not start the projection (HRESULT {hr:#x})");
    }
    Ok(Session { ctx, projection })
}
