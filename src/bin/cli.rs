use std::process::ExitCode;

use shadowplay_patcher::memory::Process;
use shadowplay_patcher::patch::{PATCH_ACCESS, PatchStatus, apply_shadowplay_patches, find_shadowplay_pid, query_status};
use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;

const HELP: &str = "\
ShadowPlay Patcher (console)

Usage: ShadowPlay_Patcher.exe [option]

  (no option)              patch the NVIDIA process, then wait for a key
  --no-wait-for-keypress   patch and exit right away
  --status                 only show the current state, change nothing
  --help                   show this text

Exit codes:
  0  patched (or already patched)
  1  NVIDIA found, not patched (--status only)
  2  NVIDIA ShadowPlay process not found
  3  error: patching failed, access denied, or unknown option";

unsafe extern "C" {
    fn _getch() -> i32;
}

fn exit_code(status: PatchStatus) -> u8 {
    match status {
        PatchStatus::Patched => 0,
        PatchStatus::NotPatched => 1,
        PatchStatus::NvidiaNotFound => 2,
        PatchStatus::NeedsAdmin | PatchStatus::Error => 3,
    }
}

fn patch() -> Result<(), (PatchStatus, String)> {
    let pid = find_shadowplay_pid().ok_or((
        PatchStatus::NvidiaNotFound,
        "Expected exactly one nvcontainer.exe with nvd3dumx.dll loaded (found none or several).".to_string(),
    ))?;
    println!("Info: Found the NVIDIA ShadowPlay process. PID: {pid}");

    let process = Process::open(pid, PATCH_ACCESS).map_err(|e| match e {
        ERROR_ACCESS_DENIED => (PatchStatus::NeedsAdmin, "Access denied. Run this as administrator.".to_string()),
        e => (PatchStatus::Error, format!("Could not open the process (Windows error {e}).")),
    })?;

    apply_shadowplay_patches(&process, &mut |line| println!("{line}")).map_err(|e| (PatchStatus::Error, e))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let known = ["--no-wait-for-keypress", "--status", "--help", "-h", "/?"];
    if let Some(bad) = args.iter().find(|a| !known.contains(&a.as_str())) {
        eprintln!("Unknown option: {bad}\n\n{HELP}");
        return ExitCode::from(3);
    }
    let has = |name: &str| args.iter().any(|a| a == name);

    if has("--help") || has("-h") || has("/?") {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }

    if has("--status") {
        let info = query_status();
        println!("{:?}: {}", info.status, info.message);
        return ExitCode::from(exit_code(info.status));
    }

    let code = match patch() {
        Ok(()) => {
            println!("Done: Instant Replay will keep recording.");
            0
        }
        Err((status, e)) => {
            println!("Error: {e}");
            exit_code(status)
        }
    };

    if !has("--no-wait-for-keypress") {
        println!("Press any key to exit...");
        unsafe { _getch() };
    }
    ExitCode::from(code)
}
