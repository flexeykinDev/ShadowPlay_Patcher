//! The two ShadowPlay patches and the high-level helpers shared by the CLI and GUI.

use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
use windows_sys::Win32::System::Memory::{PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE};
use windows_sys::Win32::System::Threading::{
    PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
};

use crate::memory::{self, Process};

/// Rights needed to patch: read/write/alloc + query. Not PROCESS_ALL_ACCESS.
pub const PATCH_ACCESS: u32 =
    PROCESS_VM_OPERATION | PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_QUERY_INFORMATION;

/// `GetWindowDisplayAffinity(HWND, DWORD* pdwAffinity)`:
/// `if (pdwAffinity) { *pdwAffinity = WDA_NONE; return TRUE; } return FALSE;`
pub const GET_WINDOW_DISPLAY_AFFINITY_STUB: [u8; 20] = [
    0x48, 0x85, 0xD2, //                 test rdx, rdx
    0x74, 0x0C, //                       jz   set_false
    0xC7, 0x02, 0x00, 0x00, 0x00, 0x00, // mov  dword [rdx], 0
    0xB8, 0x01, 0x00, 0x00, 0x00, //     mov  eax, 1
    0xC3, //                             ret
    0x31, 0xC0, //                       set_false: xor eax, eax
    0xC3, //                             ret
];

/// `Module32FirstW(...)`: return FALSE, so the scan never finds widevinecdm.dll.
pub const MODULE32_FIRST_W_STUB: [u8; 4] = [
    0x48, 0x31, 0xC0, // xor rax, rax
    0xC3, //             ret
];

/// One function the patch redirects.
pub struct HookSpec {
    pub module: &'static str,
    pub function: &'static str,
    pub stub: &'static [u8],
    /// Bytes of the entry to overwrite: the 5-byte JMP plus NOPs up to an
    /// instruction boundary.
    pub patch_len: usize,
    /// Which NVIDIA check this hook defeats, for messages.
    pub check: &'static str,
}

/// Everything the patch hooks. Both are needed for the "patched" status.
pub const HOOKS: [HookSpec; 2] = [
    HookSpec {
        module: "USER32.dll",
        function: "GetWindowDisplayAffinity",
        stub: &GET_WINDOW_DISPLAY_AFFINITY_STUB,
        patch_len: 6,
        check: "Hidden-window",
    },
    HookSpec {
        module: "KERNEL32.DLL",
        function: "Module32FirstW",
        stub: &MODULE32_FIRST_W_STUB,
        patch_len: 7,
        check: "Protected-content",
    },
];

pub enum Hook {
    Installed { at: usize, jmp: [u8; 5] },
    AlreadyPatched { at: usize },
}

/// Redirect an exported function to `stub`, written into fresh RX memory near it.
///
/// The JMP is only written once the stub is in place, so a failure can never
/// redirect the function into empty memory. Idempotent: an entry that already
/// starts with a JMP is left alone.
pub fn install_stub_hook(process: &Process, module: &str, function: &str, stub: &[u8], patch_len: usize) -> Result<Hook, String> {
    if patch_len < 5 {
        return Err("patch_len must be at least 5 bytes for a near JMP".into());
    }

    let base = memory::remote_module_base(process, module)
        .ok_or_else(|| format!("Could not find {module} in the NVIDIA process"))?;
    let target = memory::remote_export_address(base, module, function)?;

    // Already starts with our JMP: re-patching would only leak another page.
    if memory::starts_with_jmp(process, target) {
        return Ok(Hook::AlreadyPatched { at: target });
    }

    let stub_mem = memory::allocate_near(process, target, 0x1000, PAGE_EXECUTE_READWRITE)
        .ok_or_else(|| format!("Could not allocate memory near {function}"))?;

    let result = (|| {
        memory::write_protected(process, stub_mem, stub)
            .map_err(|e| format!("Could not write the stub for {function} ({e})"))?;

        // The stub is in place, so drop write access (RWX -> RX). Not fatal.
        let _ = memory::protect(process, stub_mem, stub.len(), PAGE_EXECUTE_READ);

        let jmp = memory::assemble_near_jmp(target, stub_mem)
            .ok_or_else(|| format!("The stub is too far from {function} for a near JMP"))?;
        let mut patch = vec![0x90; patch_len];
        patch[..5].copy_from_slice(&jmp);

        memory::write_protected(process, target, &patch)
            .map_err(|e| format!("Could not write the JMP over {function} ({e})"))?;
        Ok(Hook::Installed { at: target, jmp })
    })();

    // Free the stub only if nothing jumps to it. A failed write may still have
    // landed partly, and freeing a page the function jumps into crashes the target.
    if result.is_err() && !memory::starts_with_jmp(process, target) {
        memory::free(process, stub_mem);
    }
    result
}

/// Apply every hook to an open process. `log` gets one line per step. The
/// error names the check that could not be patched and why.
pub fn apply_shadowplay_patches(process: &Process, log: &mut dyn FnMut(&str)) -> Result<(), String> {
    for h in &HOOKS {
        match install_stub_hook(process, h.module, h.function, h.stub, h.patch_len) {
            Ok(Hook::Installed { at, jmp }) => {
                log(&format!("Info: {} hooked at {at:#x} ({})", h.function, hex(&jmp)));
            }
            Ok(Hook::AlreadyPatched { at }) => {
                log(&format!("Info: {} at {at:#x} is already patched; skipping.", h.function));
            }
            Err(e) => return Err(format!("{} patch failed: {e}.", h.check)),
        }
    }
    Ok(())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

// ------------------------------------------------------------------ high level

/// The SPUser nvcontainer.exe (the one with nvd3dumx.dll loaded). `None` if it
/// isn't running or there is more than one candidate.
pub fn find_shadowplay_pid() -> Option<u32> {
    let candidates: Vec<u32> = memory::processes_by_name("nvcontainer.exe")
        .into_iter()
        .filter(|&pid| memory::is_module_loaded(pid, "nvd3dumx.dll"))
        .collect();
    match candidates[..] {
        [pid] => Some(pid),
        _ => None,
    }
}

/// Best-effort: does every hooked function in that process start with a JMP?
/// Checking only one would report "Active" after a half-applied patch, and
/// auto re-apply would then never fix the missing hook.
pub fn is_process_patched(pid: u32) -> bool {
    let Ok(process) = Process::open(pid, PROCESS_VM_READ | PROCESS_QUERY_INFORMATION) else { return false };
    HOOKS.iter().all(|h| {
        memory::remote_module_base(&process, h.module)
            .and_then(|base| memory::remote_export_address(base, h.module, h.function).ok())
            .is_some_and(|addr| memory::starts_with_jmp(&process, addr))
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PatchStatus {
    /// No single SPUser nvcontainer.exe found.
    NvidiaNotFound,
    NotPatched,
    Patched,
    /// Windows refused access to the NVIDIA process.
    NeedsAdmin,
    Error,
}

#[derive(Clone, Debug)]
pub struct PatchInfo {
    pub status: PatchStatus,
    /// Short and human-readable, safe to show in a UI.
    pub message: String,
}

const MSG_NOT_FOUND: &str = "NVIDIA ShadowPlay process not found. Is the NVIDIA App running with Instant Replay on?";
const MSG_NOT_PATCHED: &str = "NVIDIA found, but the patch is not applied yet.";
const MSG_ACTIVE: &str = "Instant Replay will keep recording.";
const MSG_NEEDS_ADMIN: &str = "Windows blocked access to the NVIDIA process. Run this app as administrator.";

fn info(status: PatchStatus, message: impl Into<String>) -> PatchInfo {
    PatchInfo { status, message: message.into() }
}

/// Current state, without changing anything.
pub fn query_status() -> PatchInfo {
    match find_shadowplay_pid() {
        None => info(PatchStatus::NvidiaNotFound, MSG_NOT_FOUND),
        Some(pid) if is_process_patched(pid) => info(PatchStatus::Patched, MSG_ACTIVE),
        Some(_) => info(PatchStatus::NotPatched, MSG_NOT_PATCHED),
    }
}

/// Find, open and patch. Safe to call repeatedly.
pub fn apply_patch() -> PatchInfo {
    let Some(pid) = find_shadowplay_pid() else { return info(PatchStatus::NvidiaNotFound, MSG_NOT_FOUND) };

    let process = match Process::open(pid, PATCH_ACCESS) {
        Ok(p) => p,
        Err(ERROR_ACCESS_DENIED) => return info(PatchStatus::NeedsAdmin, MSG_NEEDS_ADMIN),
        Err(err) => return info(PatchStatus::Error, format!("Could not open the NVIDIA process (Windows error {err}).")),
    };

    match apply_shadowplay_patches(&process, &mut |_| {}) {
        Ok(()) => info(PatchStatus::Patched, MSG_ACTIVE),
        Err(e) => info(PatchStatus::Error, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gwda_stub_branch_lands_on_return_false() {
        let s = GET_WINDOW_DISPLAY_AFFINITY_STUB;
        assert_eq!(s[3], 0x74); // jz rel8
        let target = 5 + s[4] as usize;
        assert_eq!(&s[target..], &[0x31, 0xC0, 0xC3]); // xor eax,eax; ret
    }

    #[test]
    fn module32_stub_returns_false() {
        assert_eq!(MODULE32_FIRST_W_STUB, [0x48, 0x31, 0xC0, 0xC3]);
    }

    #[test]
    fn hooks_overwrite_at_least_a_jmp() {
        assert!(HOOKS.iter().all(|h| h.patch_len >= 5));
    }

    #[test]
    fn hex_format() {
        assert_eq!(hex(&[0x0f, 0xe9, 0x00]), "0f e9 00");
    }

    #[test]
    fn short_patch_is_rejected() {
        let me = Process::open(std::process::id(), PATCH_ACCESS).unwrap();
        assert!(install_stub_hook(&me, "USER32.dll", "GetWindowDisplayAffinity", &MODULE32_FIRST_W_STUB, 4).is_err());
    }
}
