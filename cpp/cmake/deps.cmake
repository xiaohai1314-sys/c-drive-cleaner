# 依赖由 tools/fetch_deps.ps1 下载并校验，CMake 本身不联网。
set(CDC_REQUIRED_FILES
    "${CDC_THIRD_PARTY}/nlohmann/json.hpp"
    "${CDC_THIRD_PARTY}/sqlite/sqlite3.c"
    "${CDC_THIRD_PARTY}/sqlite/sqlite3.h"
    "${CDC_THIRD_PARTY}/blake3/blake3.c"
    "${CDC_THIRD_PARTY}/blake3/blake3.h"
    "${CDC_THIRD_PARTY}/webview2/include/WebView2.h"
    "${CDC_THIRD_PARTY}/webview2/include/WebView2EnvironmentOptions.h"
    "${CDC_THIRD_PARTY}/webview2/x64/WebView2LoaderStatic.lib"
)

foreach(file IN LISTS CDC_REQUIRED_FILES)
    if(NOT EXISTS "${file}")
        message(FATAL_ERROR
            "缺少依赖文件: ${file}\n"
            "请先运行: powershell -ExecutionPolicy Bypass -File ${CDC_ROOT}/tools/fetch_deps.ps1")
    endif()
endforeach()
