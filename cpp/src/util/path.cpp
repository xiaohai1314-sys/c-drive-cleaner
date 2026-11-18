#include "util/path.h"

#include <windows.h>

#include <cctype>

#include "util/env.h"
#include "util/utf.h"

std::string NormalizePath(std::string path) {
    for (char& ch : path) {
        if (ch == '/') {
            ch = '\\';
        }
    }
    if (path.size() >= 2 && path[1] == ':') {
        path[0] = static_cast<char>(
            std::toupper(static_cast<unsigned char>(path[0])));
    }
    std::string out;
    out.reserve(path.size());
    for (char ch : path) {
        if (ch == '\\' && !out.empty() && out.back() == '\\') {
            continue;
        }
        out.push_back(ch);
    }
    while (out.size() > 3 && out.back() == '\\') {
        out.pop_back();
    }
    return out;
}

std::string Basename(std::string_view path) {
    const size_t slash = path.find_last_of("\\/");
    if (slash == std::string_view::npos) {
        return std::string(path);
    }
    return std::string(path.substr(slash + 1));
}

bool IsDriveRoot(std::string_view path) {
    return (path.size() == 3 && path[1] == ':' && path[2] == '\\') ||
           (path.size() == 2 && path[1] == ':');
}

std::string JoinPath(std::string_view base, std::string_view child) {
    if (child.empty()) {
        return std::string(base);
    }
    if (child.size() >= 2 && child[1] == ':') {
        return std::string(child);
    }
    if (base.empty()) {
        return std::string(child);
    }
    std::string out(base);
    if (out.back() != '\\' && out.back() != '/') {
        out.push_back('\\');
    }
    out.append(child);
    return out;
}

bool PathExists(const std::string& utf8Path) {
    const std::wstring wide = Utf8ToWide(utf8Path);
    if (wide.empty()) {
        return false;
    }
    return GetFileAttributesW(wide.c_str()) != INVALID_FILE_ATTRIBUTES;
}

std::string LocalAppDataDir() {
    const std::string local = GetEnv("LOCALAPPDATA");
    if (local.empty()) {
        return {};
    }
    return JoinPath(local, "C盘清理工具");
}

std::string CacheDir() {
    std::string base = LocalAppDataDir();
    if (base.empty()) {
        base = ".";
    }
    const std::string dir = JoinPath(base, "mftcache");
    CreateDirectoryW(Utf8ToWide(dir).c_str(), nullptr);
    return dir;
}

uint64_t FileMtimeSeconds(const std::string& utf8Path) {
    WIN32_FILE_ATTRIBUTE_DATA data = {};
    if (GetFileAttributesExW(Utf8ToWide(utf8Path).c_str(), GetFileExInfoStandard,
                             &data) == 0) {
        return 0;
    }
    const uint64_t fileTime =
        (static_cast<uint64_t>(data.ftLastWriteTime.dwHighDateTime) << 32) |
        static_cast<uint64_t>(data.ftLastWriteTime.dwLowDateTime);
    constexpr uint64_t kWindowsEpochDiffSecs = 11'644'473'600ULL;
    const uint64_t seconds = fileTime / 10'000'000ULL;
    return seconds > kWindowsEpochDiffSecs ? seconds - kWindowsEpochDiffSecs : 0;
}

std::string ToLongPath(const std::wstring& path) {
    if (path.empty()) {
        return {};
    }
    if (path.rfind(L"\\\\?\\", 0) == 0) {
        return WideToUtf8(path);
    }
    if (path.rfind(L"\\\\", 0) == 0) {
        return WideToUtf8(L"\\\\?\\UNC\\" + path.substr(2));
    }
    if (path.size() >= 2 && path[1] == L':') {
        return WideToUtf8(L"\\\\?\\" + path);
    }
    return WideToUtf8(path);
}
