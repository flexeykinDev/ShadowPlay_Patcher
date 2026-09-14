#include "patch.h"
#include "memory.h"
#include "utils.h"

#include <iostream>
#include <array>

std::vector<uint8_t> makeGetWindowDisplayAffinityStub() {
    // test rdx, rdx            48 85 D2
    // jz   set_false           74 0C           ; if pdwAffinity == NULL, skip the store
    // mov  dword [rdx], 0      C7 02 00 00 00 00 ; *pdwAffinity = WDA_NONE (0)
    // mov  eax, 1              B8 01 00 00 00   ; return TRUE
    // ret                      C3
    // set_false:
    // xor  eax, eax            31 C0            ; return FALSE
    // ret                      C3
    return {
        0x48, 0x85, 0xD2,
        0x74, 0x0C,
        0xC7, 0x02, 0x00, 0x00, 0x00, 0x00,
        0xB8, 0x01, 0x00, 0x00, 0x00,
        0xC3,
        0x31, 0xC0,
        0xC3
    };
}

std::vector<uint8_t> makeModule32FirstWStub() {
    // xor rax, rax             48 31 C0        ; return FALSE
    // ret                      C3
    return { 0x48, 0x31, 0xC0, 0xC3 };
}

int installStubHook(HANDLE hProcess,
                    const wchar_t* moduleName,
                    const char* functionName,
                    const std::vector<uint8_t>& stub,
                    size_t patchLen) {
    if (patchLen < 5) {
        std::cout << "Error: patchLen must be at least 5 bytes for a near JMP." << std::endl;
        return 1;
    }

    uintptr_t moduleBase = getRemoteModuleBaseAddress(hProcess, moduleName);
    if (!moduleBase) {
        std::wcout << L"Error: Could not get base address of " << moduleName << std::endl;
        return 1;
    }

    uintptr_t target = getExportedFunctionAddress(hProcess, moduleBase, moduleName, functionName);
    if (!target) {
        std::cout << "Error: Could not resolve remote address of " << functionName << std::endl;
        return 1;
    }
    std::cout << "Info: " << functionName << " resolved at 0x" << std::hex << target << std::dec << std::endl;

    // Idempotency: if the entry already begins with our near-JMP opcode, the
    // patch is in place. Re-patching would leak another scratch page every run.
    uint8_t firstByte = 0;
    if (readMemory(hProcess, target, &firstByte, 1) && firstByte == 0xE9) {
        std::cout << "Info: " << functionName << " is already patched; skipping." << std::endl;
        return 0;
    }

    // Reserve an executable scratch page within JMP range of the function.
    uintptr_t stubMem = allocateMemoryNearAddress(hProcess, target, 0x1000);
    if (!stubMem) {
        std::cout << "Error: Could not allocate memory near " << functionName << std::endl;
        return 1;
    }

    // Write the stub FIRST. If this fails, bail out without touching the
    // function entry -- otherwise the JMP would redirect into an empty page and
    // crash the target process.
    if (!writeMemoryWithProtectionDynamic(hProcess, stubMem, stub)) {
        std::cout << "Error: Could not write stub for " << functionName << "; aborting (function left untouched)." << std::endl;
        VirtualFreeEx(hProcess, reinterpret_cast<void*>(stubMem), 0, MEM_RELEASE);
        return 1;
    }

    // Drop write permission on the stub page now that it is populated (RWX -> RX).
    if (!protectMemory(hProcess, stubMem, stub.size(), PAGE_EXECUTE_READ)) {
        std::cout << "Warning: Could not tighten stub page to R+X for " << functionName << " (continuing)." << std::endl;
    }

    // Build the 5-byte near JMP and pad the rest of the clobbered prologue with NOPs.
    std::array<uint8_t, 5> jmp{};
    if (!assembleJumpNearInstruction(jmp.data(), target, stubMem)) {
        std::cout << "Error: Stub is too far from " << functionName << " for a near JMP." << std::endl;
        VirtualFreeEx(hProcess, reinterpret_cast<void*>(stubMem), 0, MEM_RELEASE);
        return 1;
    }

    std::vector<uint8_t> patch(patchLen, 0x90); // NOP fill
    std::copy(jmp.begin(), jmp.end(), patch.begin());

    if (!writeMemoryWithProtection(hProcess, target, patch.data(), patch.size())) {
        std::cout << "Error: Could not write JMP over " << functionName << std::endl;
        VirtualFreeEx(hProcess, reinterpret_cast<void*>(stubMem), 0, MEM_RELEASE);
        return 1;
    }

    std::cout << "Info: Hook installed at " << functionName
              << " (" << bytesToHexString(jmp.data(), jmp.size()) << ")" << std::endl;
    return 0;
}

int applyShadowPlayPatches(HANDLE hProcess) {
    std::cout << std::endl;
    int rc = installStubHook(hProcess, L"USER32.dll", "GetWindowDisplayAffinity",
                             makeGetWindowDisplayAffinityStub(), 6);
    if (rc) {
        std::cout << "Error: Failed to apply the invisible-window patch. Exiting." << std::endl;
        return rc;
    }

    std::cout << std::endl;
    rc = installStubHook(hProcess, L"KERNEL32.DLL", "Module32FirstW",
                         makeModule32FirstWStub(), 7);
    if (rc) {
        std::cout << "Error: Failed to apply the protected-content patch. Exiting." << std::endl;
        return rc;
    }

    return 0;
}

// ------------------------------------------------------------------ high level

DWORD findShadowPlayProcessId() {
    std::vector<DWORD> candidates;
    for (DWORD pid : getProcessesByName(L"nvcontainer.exe")) {
        if (isModuleLoaded(pid, L"nvd3dumx.dll")) {
            candidates.push_back(pid);
        }
    }
    return candidates.size() == 1 ? candidates[0] : 0;
}

bool isProcessPatched(DWORD pid) {
    if (!pid) return false;
    HANDLE h = OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, FALSE, pid);
    if (!h) return false;

    bool patched = false;
    uintptr_t base = getRemoteModuleBaseAddress(h, L"USER32.dll");
    if (base) {
        uintptr_t addr = getExportedFunctionAddress(h, base, L"USER32.dll", "GetWindowDisplayAffinity");
        uint8_t firstByte = 0;
        if (addr && readMemory(h, addr, &firstByte, 1) && firstByte == 0xE9) {
            patched = true;
        }
    }
    CloseHandle(h);
    return patched;
}

PatchInfo queryShadowPlayStatus() {
    DWORD pid = findShadowPlayProcessId();
    if (!pid) {
        return { PatchStatus::NvidiaNotFound,
                 L"NVIDIA ShadowPlay process not found. Is the NVIDIA App / Instant Replay running?" };
    }
    if (isProcessPatched(pid)) {
        return { PatchStatus::Patched, L"Active — Instant Replay will keep recording." };
    }
    return { PatchStatus::NotPatched, L"NVIDIA found, but the patch is not applied yet." };
}

PatchInfo applyShadowPlayPatch() {
    DWORD pid = findShadowPlayProcessId();
    if (!pid) {
        return { PatchStatus::NvidiaNotFound,
                 L"NVIDIA ShadowPlay process not found. Is the NVIDIA App / Instant Replay running?" };
    }

    const DWORD access = PROCESS_VM_OPERATION | PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_QUERY_INFORMATION;
    HANDLE h = OpenProcess(access, FALSE, pid);
    if (!h) {
        DWORD err = GetLastError();
        std::wstring msg = L"Could not open the NVIDIA process (error " + std::to_wstring(err) + L").";
        if (err == ERROR_ACCESS_DENIED) msg += L" Try running this app as administrator.";
        return { PatchStatus::Error, msg };
    }

    int rc = applyShadowPlayPatches(h);
    CloseHandle(h);

    if (rc != 0) {
        return { PatchStatus::Error, L"Patching failed. See details, or try running as administrator." };
    }
    return { PatchStatus::Patched, L"Active — Instant Replay will keep recording." };
}
