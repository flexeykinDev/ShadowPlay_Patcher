//! Patches THIS test process's own GetWindowDisplayAffinity and Module32FirstW,
//! then calls them to prove the hooks change behavior. Same path as the real
//! patch, but nothing else on the system is touched.
//!
//! Kept as one test in its own file (= its own process), because patching
//! Module32FirstW breaks module lookups for everything that runs after it.

use std::mem::{size_of, zeroed};
use std::ptr::null;

use shadowplay_patcher::memory::{self, Process};
use shadowplay_patcher::patch::{
    GET_WINDOW_DISPLAY_AFFINITY_STUB, Hook, MODULE32_FIRST_W_STUB, PATCH_ACCESS, install_stub_hook,
    is_process_patched,
};
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, TH32CS_SNAPMODULE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetWindowDisplayAffinity, SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
    WDA_NONE, WS_OVERLAPPED,
};

fn first_byte_is_jmp(me: &Process, module: &str, function: &str) -> bool {
    let base = memory::remote_module_base(me, module).unwrap();
    let addr = memory::remote_export_address(base, module, function).unwrap();
    let mut b = [0u8];
    memory::read(me, addr, &mut b) && b[0] == 0xE9
}

fn first_module_found() -> bool {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, std::process::id());
        let mut me: MODULEENTRY32W = zeroed();
        me.dwSize = size_of::<MODULEENTRY32W>() as u32;
        let got = Module32FirstW(snap, &mut me) != 0;
        CloseHandle(snap);
        got
    }
}

#[test]
fn self_patch() {
    let me = Process::open(std::process::id(), PATCH_ACCESS).expect("OpenProcess(self)");

    // --- GetWindowDisplayAffinity
    let class = memory::wide("STATIC");
    let title = memory::wide("sp-test");
    let hwnd = unsafe {
        CreateWindowExW(0, class.as_ptr(), title.as_ptr(), WS_OVERLAPPED, 0, 0, 16, 16,
            null::<()>() as _, null::<()>() as _, null::<()>() as _, null())
    };
    assert!(!hwnd.is_null());

    // Make the window really capture-protected so "before" is meaningful.
    // If the OS refuses, the post-patch checks still prove the hook works.
    let set_ok = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } != 0;
    let mut before = 0xDEAD;
    let ret_before = unsafe { GetWindowDisplayAffinity(hwnd, &mut before) };
    if set_ok {
        assert_ne!(ret_before, 0);
        assert_eq!(before, WDA_EXCLUDEFROMCAPTURE);
    }

    let hook = install_stub_hook(&me, "USER32.dll", "GetWindowDisplayAffinity", &GET_WINDOW_DISPLAY_AFFINITY_STUB, 6);
    assert!(matches!(hook, Ok(Hook::Installed { .. })));
    assert!(first_byte_is_jmp(&me, "USER32.dll", "GetWindowDisplayAffinity"));

    // Half-applied (Module32FirstW not hooked yet) must not count as patched.
    assert!(!is_process_patched(std::process::id()));

    let mut after = 0xDEAD;
    assert_ne!(unsafe { GetWindowDisplayAffinity(hwnd, &mut after) }, 0);
    assert_eq!(after, WDA_NONE);

    // Null out-pointer returns FALSE, like the real API.
    assert_eq!(unsafe { GetWindowDisplayAffinity(hwnd, std::ptr::null_mut()) }, 0);

    // Patching again is a no-op.
    let again = install_stub_hook(&me, "USER32.dll", "GetWindowDisplayAffinity", &GET_WINDOW_DISPLAY_AFFINITY_STUB, 6);
    assert!(matches!(again, Ok(Hook::AlreadyPatched { .. })));

    unsafe { DestroyWindow(hwnd) };

    // --- Module32FirstW (must be last: it breaks module lookups)
    assert!(first_module_found());

    let base = memory::remote_module_base(&me, "KERNEL32.DLL").unwrap();
    let addr = memory::remote_export_address(base, "KERNEL32.DLL", "Module32FirstW").unwrap();

    let hook = install_stub_hook(&me, "KERNEL32.DLL", "Module32FirstW", &MODULE32_FIRST_W_STUB, 7);
    assert!(matches!(hook, Ok(Hook::Installed { .. })));

    let mut b = [0u8];
    assert!(memory::read(&me, addr, &mut b) && b[0] == 0xE9);
    assert!(!first_module_found());
}
