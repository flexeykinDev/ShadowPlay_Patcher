//! Thin, safe-ish wrappers over the Win32 calls used to read, write and
//! allocate memory in another process.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};

use windows_sys::Win32::Foundation::{CloseHandle, FreeLibrary, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, PROCESSENTRY32W,
    Process32FirstW, Process32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE,
    PAGE_PROTECTION_FLAGS, VirtualAllocEx, VirtualFreeEx, VirtualProtectEx, VirtualQuery,
};
use windows_sys::Win32::System::Threading::{GetProcessId, OpenProcess, PROCESS_ACCESS_RIGHTS};

/// UTF-16, NUL-terminated copy of `s` for passing to `W` APIs.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// An owned process handle, closed on drop.
pub struct Process(HANDLE);

impl Process {
    /// Open `pid` with `access`. On failure returns the Win32 error code.
    pub fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Result<Self, u32> {
        let h = unsafe { OpenProcess(access, 0, pid) };
        if h.is_null() { Err(unsafe { GetLastError() }) } else { Ok(Self(h)) }
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }

    pub fn pid(&self) -> u32 {
        unsafe { GetProcessId(self.0) }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

struct Snapshot(HANDLE);

impl Snapshot {
    fn new(flags: u32, pid: u32) -> Option<Self> {
        let h = unsafe { CreateToolhelp32Snapshot(flags, pid) };
        if h == INVALID_HANDLE_VALUE { None } else { Some(Self(h)) }
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// PIDs of all running processes whose exe name matches `name` (case-insensitive).
pub fn processes_by_name(name: &str) -> Vec<u32> {
    let mut pids = Vec::new();
    let Some(snap) = Snapshot::new(TH32CS_SNAPPROCESS, 0) else { return pids };

    let target = name.to_lowercase();
    let mut pe: PROCESSENTRY32W = unsafe { zeroed() };
    pe.dwSize = size_of::<PROCESSENTRY32W>() as u32;

    let mut ok = unsafe { Process32FirstW(snap.0, &mut pe) } != 0;
    while ok {
        if from_wide(&pe.szExeFile).to_lowercase() == target {
            pids.push(pe.th32ProcessID);
        }
        ok = unsafe { Process32NextW(snap.0, &mut pe) } != 0;
    }
    pids
}

pub struct Module {
    pub name: String,
    pub path: String,
    pub base: usize,
}

/// Modules loaded in `pid`. Empty if the process can't be inspected.
pub fn modules(pid: u32) -> Vec<Module> {
    let mut out = Vec::new();
    let Some(snap) = Snapshot::new(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) else { return out };

    let mut me: MODULEENTRY32W = unsafe { zeroed() };
    me.dwSize = size_of::<MODULEENTRY32W>() as u32;

    let mut ok = unsafe { Module32FirstW(snap.0, &mut me) } != 0;
    while ok {
        out.push(Module {
            name: from_wide(&me.szModule),
            path: from_wide(&me.szExePath),
            base: me.modBaseAddr as usize,
        });
        ok = unsafe { Module32NextW(snap.0, &mut me) } != 0;
    }
    out
}

/// Is a module called `name` (file name or full path) loaded in `pid`?
pub fn is_module_loaded(pid: u32, name: &str) -> bool {
    let target = name.to_lowercase();
    modules(pid)
        .iter()
        .any(|m| m.name.to_lowercase() == target || m.path.to_lowercase() == target)
}

/// Base address of `module` inside the process behind `process`.
pub fn remote_module_base(process: &Process, module: &str) -> Option<usize> {
    modules(process.pid())
        .into_iter()
        .find(|m| m.name.eq_ignore_ascii_case(module))
        .map(|m| m.base)
}

/// Address of `function` in a remote copy of `module` loaded at `remote_base`.
///
/// System DLLs sit at the same offset in every process, so we resolve the
/// export locally and apply its offset to the remote base.
pub fn remote_export_address(remote_base: usize, module: &str, function: &str) -> Result<usize, String> {
    let module_w = wide(module);
    let mut function_a = function.as_bytes().to_vec();
    function_a.push(0);

    let local = unsafe { LoadLibraryW(module_w.as_ptr()) };
    if local.is_null() {
        return Err(format!("Could not load {module} locally"));
    }

    let result = (|| {
        let proc = unsafe { GetProcAddress(local, function_a.as_ptr()) }
            .ok_or_else(|| format!("{function} is not exported by {module}"))?;
        let proc = proc as usize;

        // A forwarded export (e.g. KERNEL32 -> kernelbase) resolves into a
        // different DLL, which would make the offset meaningless. Refuse rather
        // than hook a random address in the target.
        let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { zeroed() };
        let got = unsafe { VirtualQuery(proc as *const c_void, &mut mbi, size_of::<MEMORY_BASIC_INFORMATION>()) };
        if got == 0 || mbi.AllocationBase != local {
            return Err(format!("{function} is forwarded to another module; refusing to compute a bogus offset"));
        }
        Ok(remote_base + (proc - local as usize))
    })();

    unsafe { FreeLibrary(local) };
    result
}

/// Commit `size` bytes as close to `desired` as possible, within near-JMP range.
pub fn allocate_near(process: &Process, desired: usize, size: usize, protection: PAGE_PROTECTION_FLAGS) -> Option<usize> {
    const STEP: usize = 0x1000;
    const RANGE: usize = 0x2000_0000 - 0x2000;

    let start = desired.saturating_sub(RANGE) & !(STEP - 1);
    let end = desired.saturating_add(RANGE);
    (start..end).step_by(STEP).find_map(|addr| {
        let p = unsafe { VirtualAllocEx(process.raw(), addr as *const c_void, size, MEM_RESERVE | MEM_COMMIT, protection) };
        (!p.is_null()).then_some(p as usize)
    })
}

pub fn free(process: &Process, address: usize) {
    unsafe { VirtualFreeEx(process.raw(), address as *mut c_void, 0, MEM_RELEASE) };
}

/// `E9 rel32` jumping from `source` to `target`, or `None` if out of range.
pub fn assemble_near_jmp(source: usize, target: usize) -> Option<[u8; 5]> {
    let offset = target as i64 - (source as i64 + 5);
    let rel = i32::try_from(offset).ok()?;
    let mut jmp = [0xE9, 0, 0, 0, 0];
    jmp[1..].copy_from_slice(&rel.to_le_bytes());
    Some(jmp)
}

pub fn read(process: &Process, address: usize, buf: &mut [u8]) -> bool {
    let mut done = 0usize;
    let ok = unsafe {
        ReadProcessMemory(process.raw(), address as *const c_void, buf.as_mut_ptr().cast(), buf.len(), &mut done)
    };
    ok != 0 && done == buf.len()
}

pub fn write(process: &Process, address: usize, data: &[u8]) -> bool {
    let mut done = 0usize;
    let ok = unsafe {
        WriteProcessMemory(process.raw(), address as *const c_void, data.as_ptr().cast(), data.len(), &mut done)
    };
    ok != 0 && done == data.len()
}

/// Change protection of a remote range, returning the old protection.
pub fn protect(process: &Process, address: usize, size: usize, protection: PAGE_PROTECTION_FLAGS) -> Option<PAGE_PROTECTION_FLAGS> {
    let mut old = 0;
    let ok = unsafe { VirtualProtectEx(process.raw(), address as *const c_void, size, protection, &mut old) };
    (ok != 0).then_some(old)
}

/// Make the range writable, write `data`, then restore the old protection.
///
/// Only a failed write is an error. If the write went through but the old
/// protection can't be restored, the data is still in place, and reporting a
/// failure would make the caller free memory that is already in use.
pub fn write_protected(process: &Process, address: usize, data: &[u8]) -> Result<(), String> {
    let old = protect(process, address, data.len(), PAGE_EXECUTE_READWRITE)
        .ok_or("Could not change memory protection")?;
    let written = write(process, address, data);
    let _ = protect(process, address, data.len(), old);
    if written { Ok(()) } else { Err("Could not write memory".into()) }
}

/// Does the code at `address` start with a near JMP (`E9`)?
pub fn starts_with_jmp(process: &Process, address: usize) -> bool {
    let mut first = [0u8];
    read(process, address, &mut first) && first[0] == 0xE9
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jmp_forward() {
        let jmp = assemble_near_jmp(0x1_4000_0000, 0x1_4000_0100).unwrap();
        assert_eq!(jmp[0], 0xE9);
        assert_eq!(i32::from_le_bytes(jmp[1..].try_into().unwrap()), 0xFB);
    }

    #[test]
    fn jmp_backward() {
        let jmp = assemble_near_jmp(0x1_4000_0100, 0x1_4000_0000).unwrap();
        assert_eq!(i32::from_le_bytes(jmp[1..].try_into().unwrap()), -0x105);
    }

    #[test]
    fn jmp_out_of_range() {
        assert!(assemble_near_jmp(0x1000_0000, 0xA000_0000).is_none());
    }

    #[test]
    fn wide_roundtrip() {
        let w = wide("AbC");
        assert_eq!(w, [b'A' as u16, b'b' as u16, b'C' as u16, 0]);
        assert_eq!(from_wide(&w), "AbC");
    }
}
