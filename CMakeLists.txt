cmake_minimum_required(VERSION 3.20)
project(ShadowPlay_Patcher CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

# Match the Visual Studio project: build against the wide-character Win32 API.
add_compile_definitions(UNICODE _UNICODE)

# Statically link the C/C++ runtime so the .exe is standalone (no VC++ redist).
if(MSVC)
  set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")
  # Read sources (and encode literals) as UTF-8 so non-ASCII text like the em
  # dash renders correctly regardless of the system code page.
  add_compile_options(/utf-8)
endif()

# Shared implementation, reused by both the app and the test runner.
add_library(sp_core STATIC memory.cpp patch.cpp utils.cpp)
target_link_libraries(sp_core PUBLIC user32)

add_executable(ShadowPlay_Patcher main.cpp)
target_link_libraries(ShadowPlay_Patcher PRIVATE sp_core)

# Tray + mini-window GUI (Windows subsystem, no console).
add_executable(ShadowPlay_Patcher_GUI WIN32 gui.cpp)
target_link_libraries(ShadowPlay_Patcher_GUI PRIVATE sp_core comctl32)

add_executable(ShadowPlay_Tests tests.cpp)
target_link_libraries(ShadowPlay_Tests PRIVATE sp_core)

enable_testing()
add_test(NAME unit_and_integration COMMAND ShadowPlay_Tests)
