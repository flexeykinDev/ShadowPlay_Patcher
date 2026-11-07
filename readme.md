# ShadowPlay Patcher

Keeps NVIDIA Instant Replay recording when it would otherwise switch itself off.

[![CI](https://github.com/flexeykinDev/ShadowPlay_Patcher/actions/workflows/ci.yml/badge.svg)](https://github.com/flexeykinDev/ShadowPlay_Patcher/actions/workflows/ci.yml)

## The problem

Instant Replay stops recording when it thinks protected content is on screen:

- A window asks Windows not to be captured. Password managers and the Zoom sharing bar do this. Instant Replay turns off completely and often stays off.
- A browser plays DRM video and loads the Widevine module. Recording silently pauses while it is loaded.

## What it does

It finds the ShadowPlay `nvcontainer.exe` process and patches the two Windows functions those checks use, `GetWindowDisplayAffinity` and `Module32FirstW`, so both checks always come back clean. The change lives only in the memory of the running NVIDIA process. A reboot or NVIDIA restart undoes it, which is why the app can re-apply it on its own.

Details are in [re_research.md](re_research.md).

## Use

Run `ShadowPlay_Patcher_GUI.exe`. It sits in the tray next to the clock. Click Patch now, or turn on the two switches:

- Run when Windows starts
- Re-apply when NVIDIA restarts

If the status says Access denied, a Restart as administrator button appears. Usually it isn't needed, because the NVIDIA process runs as your user.

The tray dot shows the state: green is active, amber is not applied yet, grey means the NVIDIA process wasn't found (Instant Replay off or the NVIDIA App not running), red is an error with the reason shown in the window.

GUI options: `--minimized` starts in the tray (the autostart entry uses it), `--no-apply` only shows the status and never patches on its own.

There is also a console version, `ShadowPlay_Patcher.exe`. Run it with `--help` for options and exit codes. `--status` shows the state without changing anything.

## Before you use it

Windows that asked not to be captured, like a password manager, can now end up in your clips. Check clips before sharing them.

What has been checked: on a real NVIDIA install, with the patch applied, Instant Replay kept recording and saved a clip while a capture-protected window was open.

What is not guaranteed:

- DRM video. The patch only hides the Widevine module from NVIDIA's scan. It was not tested with real DRM playback, and browsers or NVIDIA may detect protected video in other ways. The [Windhawk mod](https://windhawk.net/) with the same idea handles DRM in Chrome more completely.
- Future NVIDIA updates may change how the checks work.

## Antivirus

The app writes into another process's memory. Real malware does that too, so Defender and other antivirus tools may flag it as a false positive. Signing the file would not change that, because the behavior is what gets flagged.

If it gets blocked, allow it in Windows Security under Protection history, or add the folder with the exe as an exclusion. You can report the false positive to Microsoft at [microsoft.com/wdsi/filesubmission](https://www.microsoft.com/wdsi/filesubmission). Only do this for a build from the [Releases](../../releases) page or one you built yourself.

The app does not use the network or touch your files. It writes two registry values under your user: the optional Run at startup entry and its own setting in `HKCU\Software\ShadowPlayPatcher`.

## Build

You need a stable Rust toolchain for Windows (MSVC).

```bash
cargo build --release
cargo test
```

The exe files end up in `target/release/`. The tests patch their own test process, not NVIDIA.

The older C++ version is still in the repository for now. It builds with CMake and Visual Studio: `cmake -G Ninja -S . -B build -DCMAKE_BUILD_TYPE=Release` and `cmake --build build`.

## Credits

Original project and research by [furyzenblade](https://github.com/furyzenblade/ShadowPlay_Patcher).

## License

This is a fork; licensing follows the upstream project. Use at your own risk: it modifies a running NVIDIA process.
