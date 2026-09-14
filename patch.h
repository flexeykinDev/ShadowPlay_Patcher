#pragma once

#include <vector>
#include <cstdint>
#include <string>
#include <windows.h>

// The stub bytes we redirect each targeted function to. Exposed so tests can
// assert their behavior directly without hard-coding magic numbers twice.

// GetWindowDisplayAffinity(HWND, DWORD* pdwAffinity):
//   if (pdwAffinity) *pdwAffinity = WDA_NONE; return TRUE;
// This is the honest "this window is not capture-protected" answer, and it is
// null-safe, unlike a bare `xor eax,eax; ret` which returns FALSE and leaves the
// caller's affinity variable uninitialized.
std::vector<uint8_t> makeGetWindowDisplayAffinityStub();

// Module32FirstW(HANDLE, LPMODULEENTRY32W): return FALSE (enumeration finds
// nothing), so the protected-content scan never spots widevinecdm.dll.
std::vector<uint8_t> makeModule32FirstWStub();

// Redirect an exported function to `stub` (written into fresh RX memory near the
// function). `patchLen` is how many bytes of the function entry to overwrite
// (>= 5: a 5-byte near JMP plus NOP padding to a clean instruction boundary).
//
// Returns 0 on success or if the function is already patched; non-zero on failure.
// On any failure after allocation, the scratch page is released (no leak), and
// the JMP is only written once the stub is confirmed in place (so a failed stub
// write can never redirect the function into empty memory).
int installStubHook(HANDLE hProcess,
                    const wchar_t* moduleName,
                    const char* functionName,
                    const std::vector<uint8_t>& stub,
                    size_t patchLen);

// Apply both ShadowPlay patches to an already-opened process handle.
// Returns 0 on success, non-zero on the first failure.
int applyShadowPlayPatches(HANDLE hProcess);

// ------------------------------------------------------------------ high level
// Convenience layer shared by the CLI and the GUI: locate the process, report
// status, and apply the patch, without the caller juggling handles.

// The SPUser nvcontainer.exe (the one with nvd3dumx.dll loaded). Returns its PID,
// or 0 if it isn't running or more than one candidate is found.
DWORD findShadowPlayProcessId();

// Best-effort: is that process already patched? (Checks whether the
// GetWindowDisplayAffinity entry starts with our near-JMP.) Needs read access.
bool isProcessPatched(DWORD pid);

enum class PatchStatus {
    NvidiaNotFound,   // no single SPUser nvcontainer.exe located
    NotPatched,       // process found, patch not (yet) applied
    Patched,          // process found and already patched
    Error             // an operation failed (see message)
};

struct PatchInfo {
    PatchStatus status;
    std::wstring message; // short, human-readable, safe to show in a UI
};

// Non-mutating snapshot of the current state, for the UI to render.
PatchInfo queryShadowPlayStatus();

// Find + open + apply. Returns the resulting state and a message. Safe to call
// repeatedly (idempotent thanks to installStubHook's already-patched check).
PatchInfo applyShadowPlayPatch();
