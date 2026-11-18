cmake_minimum_required(VERSION 3.25)

project(c_drive_cleaner_cpp LANGUAGES C CXX)

if(NOT MSVC)
    message(FATAL_ERROR "本工程只支持 MSVC（Visual Studio 2022 Build Tools）。")
endif()
if(NOT CMAKE_SIZEOF_VOID_P EQUAL 8)
    message(FATAL_ERROR "只支持 64 位构建，请使用 -A x64。")
endif()

enable_language(ASM_MASM)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")

set(CDC_ROOT "${CMAKE_CURRENT_SOURCE_DIR}")
set(CDC_REPO_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/..")
set(CDC_THIRD_PARTY "${CDC_ROOT}/third_party")
set(CDC_WEB_DIST "${CDC_REPO_ROOT}/web/dist")

include(cmake/warnings.cmake)
include(cmake/deps.cmake)
include(cmake/webassets.cmake)

enable_testing()

# ---------- 编译期嵌入资源 ----------

set(CDC_GENERATED_DIR "${CMAKE_BINARY_DIR}/generated")
file(MAKE_DIRECTORY "${CDC_GENERATED_DIR}")

function(cdc_embed_file input output var)
    add_custom_command(
        OUTPUT "${output}"
        COMMAND ${CMAKE_COMMAND}
            -DINPUT=${input}
            -DOUTPUT=${output}
            -DVAR=${var}
            -P "${CDC_ROOT}/cmake/embed_file.cmake"
        DEPENDS "${input}" "${CDC_ROOT}/cmake/embed_file.cmake"
        COMMENT "嵌入 ${input}"
        VERBATIM
    )
endfunction()

set(CDC_SHIM_HEADER "${CDC_GENERATED_DIR}/shim_js.h")
cdc_embed_file("${CDC_ROOT}/src/app/shim.js" "${CDC_SHIM_HEADER}" kShimJs)

# ---------- 三方库 ----------

add_library(cdc_blake3 STATIC
    "${CDC_THIRD_PARTY}/blake3/blake3.c"
    "${CDC_THIRD_PARTY}/blake3/blake3_dispatch.c"
    "${CDC_THIRD_PARTY}/blake3/blake3_portable.c"
    "${CDC_THIRD_PARTY}/blake3/blake3_avx2_x86-64_windows_msvc.asm"
    "${CDC_THIRD_PARTY}/blake3/blake3_avx512_x86-64_windows_msvc.asm"
    "${CDC_THIRD_PARTY}/blake3/blake3_sse2_x86-64_windows_msvc.asm"
    "${CDC_THIRD_PARTY}/blake3/blake3_sse41_x86-64_windows_msvc.asm"
)
target_include_directories(cdc_blake3 PUBLIC "${CDC_THIRD_PARTY}/blake3")
target_compile_features(cdc_blake3 PUBLIC c_std_99)
target_compile_options(cdc_blake3 PRIVATE /w)

add_library(cdc_sqlite STATIC "${CDC_THIRD_PARTY}/sqlite/sqlite3.c")
target_include_directories(cdc_sqlite PUBLIC "${CDC_THIRD_PARTY}/sqlite")
target_compile_definitions(cdc_sqlite PRIVATE
    SQLITE_THREADSAFE=1
    SQLITE_OMIT_LOAD_EXTENSION
    SQLITE_DQS=0
    SQLITE_DEFAULT_MEMSTATUS=0
    SQLITE_DEFAULT_WAL_SYNCHRONOUS=1
    SQLITE_LIKE_DOESNT_MATCH_BLOBS
    SQLITE_MAX_EXPR_DEPTH=0
)
target_compile_options(cdc_sqlite PRIVATE /w)

add_library(cdc_nlohmann INTERFACE)
target_include_directories(cdc_nlohmann INTERFACE "${CDC_THIRD_PARTY}")

add_library(cdc_webview2 INTERFACE)
target_include_directories(cdc_webview2 INTERFACE "${CDC_THIRD_PARTY}/webview2/include")
target_link_libraries(cdc_webview2 INTERFACE "${CDC_THIRD_PARTY}/webview2/x64/WebView2LoaderStatic.lib")

# ---------- 核心库 ----------

file(GLOB_RECURSE CDC_CORE_SOURCES CONFIGURE_DEPENDS
    "${CDC_ROOT}/src/util/*.cpp"
    "${CDC_ROOT}/src/core/*.cpp"
)

add_library(cdc_core STATIC ${CDC_CORE_SOURCES})
target_include_directories(cdc_core PUBLIC "${CDC_ROOT}/src")
target_link_libraries(cdc_core
    PUBLIC cdc_nlohmann cdc_blake3 cdc_sqlite
    PRIVATE bcrypt winhttp shell32 ole32 oleaut32 advapi32 shlwapi ntdll user32 gdi32
)
target_compile_definitions(cdc_core PUBLIC
    UNICODE _UNICODE WIN32_LEAN_AND_MEAN NOMINMAX
    _WIN32_WINNT=0x0A00 WINVER=0x0A00
    _CRT_SECURE_NO_WARNINGS
)
target_compile_options(cdc_core PRIVATE ${CDC_WARNING_FLAGS} /WX)

# ---------- 应用 ----------

file(GLOB CDC_APP_SOURCES CONFIGURE_DEPENDS
    "${CDC_ROOT}/src/app/*.cpp"
)

# rules.json / appdata_path_allowlist.json 作为 RCDATA 嵌入 exe。
set_source_files_properties(
    "${CDC_ROOT}/src/app/app.rc"
    "${CDC_ROOT}/src/app/data.rc"
    PROPERTIES COMPILE_FLAGS "/I\"${CDC_REPO_ROOT}\""
)

# 把 shim.js 嵌入为 C++ 字符串，避免运行时依赖额外文件。
set(CDC_SHIM_JS "${CDC_ROOT}/src/app/shim.js")

add_executable(cdc_app WIN32
    "${CDC_ROOT}/src/main.cpp"
    ${CDC_APP_SOURCES}
    "${CDC_ROOT}/src/app/app.rc"
    "${CDC_SHIM_HEADER}"
)
set_target_properties(cdc_app PROPERTIES OUTPUT_NAME "c-drive-cleaner-cpp")
target_include_directories(cdc_app PRIVATE "${CDC_GENERATED_DIR}")
target_link_libraries(cdc_app
    PRIVATE cdc_core cdc_webview2
    comctl32 dwmapi version uuid
    shell32 ole32 oleaut32 user32 gdi32 advapi32
)
target_compile_definitions(cdc_app PRIVATE
    UNICODE _UNICODE WIN32_LEAN_AND_MEAN NOMINMAX
    _WIN32_WINNT=0x0A00 WINVER=0x0A00
)
target_compile_options(cdc_app PRIVATE ${CDC_WARNING_FLAGS} /WX)
# app.rc 已经嵌入自定义 manifest，禁止链接器再生成一份。
target_link_options(cdc_app PRIVATE /MANIFEST:NO)
add_dependencies(cdc_app cdc_web)

# 把前端与规则文件放到 exe 旁边；发布版可改为嵌入。
# 先清掉旧的 web/，避免带哈希的旧资源越积越多（打包时会一起进去）。
add_custom_command(TARGET cdc_app POST_BUILD
    COMMAND ${CMAKE_COMMAND} -E rm -rf "$<TARGET_FILE_DIR:cdc_app>/web"
    COMMAND ${CMAKE_COMMAND} -E copy_directory
        "${CDC_WEB_DIST}" "$<TARGET_FILE_DIR:cdc_app>/web"
    COMMAND ${CMAKE_COMMAND} -E copy_if_different
        "${CDC_REPO_ROOT}/rules.json" "$<TARGET_FILE_DIR:cdc_app>/rules.json"
    COMMAND ${CMAKE_COMMAND} -E copy_if_different
        "${CDC_REPO_ROOT}/appdata_path_allowlist.json" "$<TARGET_FILE_DIR:cdc_app>/appdata_path_allowlist.json"
    COMMAND ${CMAKE_COMMAND} -E copy_if_different
        "${CDC_REPO_ROOT}/rules_update.json" "$<TARGET_FILE_DIR:cdc_app>/rules_update.json"
    COMMENT "复制前端与规则文件"
)

# ---------- 测试 ----------

file(GLOB CDC_TEST_SOURCES CONFIGURE_DEPENDS "${CDC_ROOT}/tests/*.cpp")
if(CDC_TEST_SOURCES)
    add_executable(cdc_tests ${CDC_TEST_SOURCES}
        "${CDC_ROOT}/src/app/data.rc")
    target_link_libraries(cdc_tests PRIVATE cdc_core shell32 ole32 user32 advapi32)
    target_compile_definitions(cdc_tests PRIVATE
        UNICODE _UNICODE WIN32_LEAN_AND_MEAN NOMINMAX CDC_TEST
        _WIN32_WINNT=0x0A00 WINVER=0x0A00
    )
    target_compile_options(cdc_tests PRIVATE ${CDC_WARNING_FLAGS} /WX)
    add_test(NAME cdc_tests COMMAND cdc_tests)
endif()

# ---------- 安装与打包 ----------

install(TARGETS cdc_app RUNTIME DESTINATION .)
install(DIRECTORY "${CDC_WEB_DIST}/" DESTINATION web)
install(FILES
    "${CDC_REPO_ROOT}/rules.json"
    "${CDC_REPO_ROOT}/appdata_path_allowlist.json"
    "${CDC_REPO_ROOT}/rules_update.json"
    DESTINATION .
)

# 默认只出 ZIP（本机无需 NSIS）；CI 用 `cpack -G "NSIS;ZIP"` 覆盖。
set(CPACK_GENERATOR "ZIP")
set(CPACK_PACKAGE_DIRECTORY "${CMAKE_BINARY_DIR}")
set(CPACK_PACKAGE_NAME "C-drive-cleaner-cpp")
set(CPACK_PACKAGE_FILE_NAME
    "${CPACK_PACKAGE_NAME}-0.1.0-win64")
set(CPACK_PACKAGE_VENDOR "CDriveCleaner")
set(CPACK_PACKAGE_VERSION "0.1.0")
set(CPACK_PACKAGE_DESCRIPTION_SUMMARY "C 盘清理工具（C++ 版）")
set(CPACK_PACKAGE_EXECUTABLES "c-drive-cleaner-cpp" "C盘清理工具")
set(CPACK_NSIS_DISPLAY_NAME "C盘清理工具 (C++)")
set(CPACK_NSIS_PACKAGE_NAME "C盘清理工具 (C++)")
set(CPACK_NSIS_MUI_ICON "${CDC_ROOT}/src/app/app.ico")
set(CPACK_NSIS_ENABLE_UNINSTALL_BEFORE_INSTALL ON)
set(CPACK_NSIS_MODIFY_PATH OFF)
set(CPACK_PACKAGE_INSTALL_DIRECTORY "C盘清理工具")
include(CPack)
