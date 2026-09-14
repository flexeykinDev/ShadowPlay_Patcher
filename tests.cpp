// Test runner for ShadowPlay_Patcher.
//
// Two layers:
//   1. Pure unit tests  - assembler, stub bytes, string/arg utilities.
//   2. Integration tests - patch THIS process's own USER32.GetWindowDisplayAffinity
//      and KERNEL32.Module32FirstW, then call them to prove the hook changed
//      behavior. This drives the exact same inject path used against nvcontainer,
//      but the target is ourselves, so nothing else on the system is touched.
//
// No external test framework (keeps the build dependency-free); a tiny CHECK
// macro tallies pass/fail and the process exits non-zero if anything failed.

#include "utils.h"
#include "memory.h"
#include "patch.h"

#include <iostream>
#include <cstring>
#include <windows.h>
#include <tlhelp32.h>

#ifndef WDA_NONE
#define WDA_NONE 0x00000000
#endif
#ifndef WDA_MONITOR
#define WDA_MONITOR 0x00000001
#endif
#ifndef WDA_EXCLUDEFROMCAPTURE
#define WDA_EXCLUDEFROMCAPTURE 0x00000011
#endif

static int g_checks = 0;
static int g_failures = 0;

#define CHECK(cond)                                                      \
    do {                                                                 \
        ++g_checks;                                                      \
        if (cond) {                                                      \
            std::cout << "  [ok]   " << #cond << "\n";                   \
        } else {                                                         \
            ++g_failures;                                                \
            std::cout << "  [FAIL] " << #cond << "  (line " << __LINE__ << ")\n"; \
        }                                                                \
    } while (0)

// ---------------------------------------------------------------- unit tests

static void test_assembleJump() {
    std::cout << "[unit] assembleJumpNearInstruction\n";
    uint8_t buf[5] = {0};
    bool ok = assembleJumpNearInstruction(buf, 0x140000000ull, 0x140000100ull);
    CHECK(ok);
    CHECK(buf[0] == 0xE9);
    int32_t rel = 0;
    std::memcpy(&rel, buf + 1, sizeof(rel));
    CHECK(rel == static_cast<int32_t>(0x140000100ull - (0x140000000ull + 5))); // 0xFB

    // Target ~2.3GB away: out of int32 near-jump range -> must refuse.
    // (Do not name this 'far' -- it is a legacy Windows macro defined to nothing.)
    uint8_t farBuf[5] = {0};
    bool ok2 = assembleJumpNearInstruction(farBuf, 0x10000000ull, 0xA0000000ull);
    CHECK(!ok2);
}

static void test_stubs() {
    std::cout << "[unit] stub byte builders\n";
    std::vector<uint8_t> expectG = {
        0x48, 0x85, 0xD2, 0x74, 0x0C,
        0xC7, 0x02, 0x00, 0x00, 0x00, 0x00,
        0xB8, 0x01, 0x00, 0x00, 0x00,
        0xC3, 0x31, 0xC0, 0xC3
    };
    CHECK(makeGetWindowDisplayAffinityStub() == expectG);

    std::vector<uint8_t> expectM = { 0x48, 0x31, 0xC0, 0xC3 };
    CHECK(makeModule32FirstWStub() == expectM);
}

static void test_utils() {
    std::cout << "[unit] utils\n";
    CHECK(toLower(L"AbC123XyZ") == L"abc123xyz");

    uint8_t bytes[] = { 0x0f, 0xe9, 0x00 };
    CHECK(bytesToHexString(bytes, 3) == "0f e9 00 ");

    char a0[] = "prog";
    char a1[] = "--no-wait-for-keypress";
    char* argv[] = { a0, a1 };
    auto args = parseCommandLineArgs(2, argv);
    CHECK(args.find("--no-wait-for-keypress") != args.end());
    CHECK(args.find("--not-passed") == args.end());
}

// --------------------------------------------------------- integration tests

// Read the first byte of a resolved export in `self` and confirm it is our JMP.
static bool firstByteIsJmp(HANDLE self, const wchar_t* mod, const char* fn) {
    uintptr_t base = getRemoteModuleBaseAddress(self, mod);
    if (!base) return false;
    uintptr_t addr = getExportedFunctionAddress(self, base, mod, fn);
    if (!addr) return false;
    uint8_t b = 0;
    return readMemory(self, addr, &b, 1) && b == 0xE9;
}

static void test_integration_gwda(HANDLE self) {
    std::cout << "[integration] GetWindowDisplayAffinity self-patch\n";

    HWND hwnd = CreateWindowExW(0, L"STATIC", L"sp-test", WS_OVERLAPPED,
                                0, 0, 16, 16, nullptr, nullptr, nullptr, nullptr);
    CHECK(hwnd != nullptr);

    // Try to make the window genuinely capture-protected so we get a non-trivial
    // "before" reading. If the OS refuses, we still prove the patch via the
    // post-patch semantics and the installed JMP byte.
    BOOL setOk = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
    DWORD affBefore = 0xDEAD;
    BOOL retBefore = GetWindowDisplayAffinity(hwnd, &affBefore);
    std::cout << "  (before) SetAffinity=" << setOk
              << " GetAffinity ret=" << retBefore
              << " value=0x" << std::hex << affBefore << std::dec << "\n";
    if (setOk) {
        CHECK(retBefore != 0);
        CHECK(affBefore == WDA_EXCLUDEFROMCAPTURE); // really protected pre-patch
    }

    int rc = installStubHook(self, L"USER32.dll", "GetWindowDisplayAffinity",
                             makeGetWindowDisplayAffinityStub(), 6);
    CHECK(rc == 0);
    CHECK(firstByteIsJmp(self, L"USER32.dll", "GetWindowDisplayAffinity"));

    // After patch: reports "not protected" (TRUE + WDA_NONE) regardless of the
    // real affinity that was set above.
    DWORD affAfter = 0xDEAD;
    BOOL retAfter = GetWindowDisplayAffinity(hwnd, &affAfter);
    CHECK(retAfter != 0);
    CHECK(affAfter == WDA_NONE);

    // Null-safety: our stub returns FALSE for a null out-pointer, like the real API.
    BOOL retNull = GetWindowDisplayAffinity(hwnd, nullptr);
    CHECK(retNull == 0);

    // Idempotency: patching again detects the existing JMP and no-ops.
    int rc2 = installStubHook(self, L"USER32.dll", "GetWindowDisplayAffinity",
                              makeGetWindowDisplayAffinityStub(), 6);
    CHECK(rc2 == 0);

    if (hwnd) DestroyWindow(hwnd);
}

// NOTE: must run LAST. Patching Module32FirstW makes it return FALSE, which
// breaks getRemoteModuleBaseAddress (it walks modules via Module32First). So we
// resolve the address up front, then never call an inject helper afterward.
static void test_integration_module32(HANDLE self) {
    std::cout << "[integration] Module32FirstW self-patch (runs last)\n";

    uintptr_t base = getRemoteModuleBaseAddress(self, L"KERNEL32.DLL");
    CHECK(base != 0);
    uintptr_t addr = getExportedFunctionAddress(self, base, L"KERNEL32.DLL", "Module32FirstW");
    CHECK(addr != 0);

    // Before patch: enumeration of our own modules succeeds.
    {
        HANDLE snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, GetCurrentProcessId());
        MODULEENTRY32W me{}; me.dwSize = sizeof(me);
        BOOL got = Module32FirstW(snap, &me);
        CHECK(got != 0);
        if (snap != INVALID_HANDLE_VALUE) CloseHandle(snap);
    }

    int rc = installStubHook(self, L"KERNEL32.DLL", "Module32FirstW",
                             makeModule32FirstWStub(), 7);
    CHECK(rc == 0);

    uint8_t b = 0;
    CHECK(readMemory(self, addr, &b, 1) && b == 0xE9);

    // After patch: the very first module lookup now fails, so a widevine scan
    // would find nothing.
    {
        HANDLE snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, GetCurrentProcessId());
        MODULEENTRY32W me{}; me.dwSize = sizeof(me);
        BOOL got = Module32FirstW(snap, &me);
        CHECK(got == 0);
        if (snap != INVALID_HANDLE_VALUE) CloseHandle(snap);
    }
}

int main() {
    std::cout << "=== ShadowPlay_Patcher test suite ===\n\n";

    test_assembleJump();
    test_stubs();
    test_utils();

    const DWORD access = PROCESS_VM_OPERATION | PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_QUERY_INFORMATION;
    HANDLE self = OpenProcess(access, FALSE, GetCurrentProcessId());
    if (!self) {
        std::cout << "\n[FATAL] OpenProcess(self) failed: " << GetLastError() << "\n";
        return 2;
    }
    test_integration_gwda(self);
    test_integration_module32(self); // keep last
    CloseHandle(self);

    std::cout << "\n=== " << (g_checks - g_failures) << "/" << g_checks
              << " checks passed, " << g_failures << " failed ===\n";
    return g_failures == 0 ? 0 : 1;
}
