#include "utils.h"
#include "memory.h"
#include "patch.h"

#include <iostream>   // For std::cout

int mainWrap() {
    DWORD pid = findShadowPlayProcessId();
    if (!pid) {
        std::cout << "Error: Expected exactly one nvcontainer.exe with nvd3dumx.dll loaded (found none or several)." << std::endl;
        return 1;
    }
    std::cout << "Info: Correct process has been found. PID: " << pid << std::endl;

    // Attach with only the rights we actually need (read/write/alloc + query),
    // rather than PROCESS_ALL_ACCESS.
    const DWORD access = PROCESS_VM_OPERATION | PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_QUERY_INFORMATION;
    HANDLE hProcess = OpenProcess(access, FALSE, pid);
    if (!hProcess) {
        std::cout << "Error: Could not open process (error " << GetLastError() << "). Try running as administrator." << std::endl;
        return 1;
    }

    int errorCode = applyShadowPlayPatches(hProcess);

    CloseHandle(hProcess);
    return errorCode;
}

int main(int argc, char* argv[]) {
    auto args = parseCommandLineArgs(argc, argv);
    bool waitForKeyPress = args.find("--no-wait-for-keypress") == args.end();

    int errorCode = mainWrap();

    if (waitForKeyPress)
        pressAnyKeyToExit();
    return errorCode;
}
