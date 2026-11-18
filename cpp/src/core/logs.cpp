#include "core/logs.h"

#include <windows.h>

#include <algorithm>
#include <cstdint>
#include <filesystem>
#include <vector>

#include "core/timeutil.h"
#include "util/path.h"
#include "util/utf.h"
#include "util/win32.h"

namespace {

constexpr uint64_t kWindowsToUnixSeconds = 11644473600ULL;
constexpr size_t kKeepLogs = 100;

bool EnsureDirectory(const std::wstring& path) {
    if (path.empty()) {
        return false;
    }
    if (CreateDirectoryW(path.c_str(), nullptr) != 0) {
        return true;
    }
    return GetLastError() == ERROR_ALREADY_EXISTS;
}

uint64_t FileTimeToUnix(const FILETIME& fileTime) {
    const uint64_t value =
        (static_cast<uint64_t>(fileTime.dwHighDateTime) << 32) |
        static_cast<uint64_t>(fileTime.dwLowDateTime);
    return value / 10000000ULL - kWindowsToUnixSeconds;
}

bool ValidName(const std::string& name) {
    if (name.empty()) {
        return false;
    }
    if (name.find('\\') != std::string::npos ||
        name.find('/') != std::string::npos) {
        return false;
    }
    return name.find("..") == std::string::npos;
}

std::string TrimEnd(std::string text) {
    while (!text.empty()) {
        const char ch = text.back();
        if (ch != ' ' && ch != '\t' && ch != '\r' && ch != '\n') {
            break;
        }
        text.pop_back();
    }
    return text;
}

struct FileInfo {
    std::string name;
    uint64_t size = 0;
    uint64_t mtime = 0;
};

std::vector<FileInfo> Enumerate(const std::wstring& dir) {
    std::vector<FileInfo> files;
    WIN32_FIND_DATAW data;
    const std::wstring pattern = dir + L"\\*";
    HANDLE handle = FindFirstFileW(pattern.c_str(), &data);
    if (handle == INVALID_HANDLE_VALUE) {
        return files;
    }
    do {
        if ((data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
            continue;
        }
        FileInfo info;
        info.name = WideToUtf8(data.cFileName);
        info.size = (static_cast<uint64_t>(data.nFileSizeHigh) << 32) |
                    static_cast<uint64_t>(data.nFileSizeLow);
        info.mtime = FileTimeToUnix(data.ftLastWriteTime);
        files.push_back(std::move(info));
    } while (FindNextFileW(handle, &data) != 0);
    FindClose(handle);
    return files;
}

void RetainNewest(const std::wstring& dir) {
    std::vector<FileInfo> logs;
    for (FileInfo& info : Enumerate(dir)) {
        if (info.name.size() >= 4 &&
            info.name.compare(info.name.size() - 4, 4, ".log") == 0) {
            logs.push_back(std::move(info));
        }
    }
    if (logs.size() <= kKeepLogs) {
        return;
    }
    std::sort(logs.begin(), logs.end(),
              [](const FileInfo& a, const FileInfo& b) {
                  return a.mtime > b.mtime;
              });
    for (size_t i = kKeepLogs; i < logs.size(); ++i) {
        DeleteFileW((dir + L"\\" + Utf8ToWide(logs[i].name)).c_str());
    }
}

bool WriteAll(HANDLE file, const std::string& bytes) {
    size_t written = 0;
    while (written < bytes.size()) {
        DWORD chunk = 0;
        const DWORD want = static_cast<DWORD>(
            std::min<size_t>(bytes.size() - written, 1u << 20));
        if (WriteFile(file, bytes.data() + written, want, &chunk, nullptr) == 0) {
            return false;
        }
        written += chunk;
    }
    return true;
}

}  // namespace

namespace logs {

std::string LogDir() {
    std::string base = LocalAppDataDir();
    if (base.empty()) {
        base = ExeDirUtf8();
    }
    if (base.empty()) {
        base = ".";
    }
    return JoinPath(base, "logs");
}

std::optional<std::string> WriteLog(const std::vector<std::string>& lines) {
    const std::string dirUtf8 = LogDir();
    const std::wstring dir = Utf8ToWide(dirUtf8);
    EnsureDirectory(Utf8ToWide(LocalAppDataDir()));
    if (!EnsureDirectory(dir)) {
        return std::nullopt;
    }
    const std::string stamp = Stamp();
    for (int suffix = 1; suffix < 10000; ++suffix) {
        const std::string name =
            suffix == 1 ? stamp + ".log"
                        : stamp + "_" + std::to_string(suffix) + ".log";
        const std::wstring full = dir + L"\\" + Utf8ToWide(name);
        HANDLE file = CreateFileW(full.c_str(), GENERIC_WRITE, 0, nullptr,
                                  CREATE_NEW, FILE_ATTRIBUTE_NORMAL, nullptr);
        if (file == INVALID_HANDLE_VALUE) {
            if (GetLastError() == ERROR_FILE_EXISTS ||
                GetLastError() == ERROR_ALREADY_EXISTS) {
                continue;
            }
            return std::nullopt;
        }
        std::string body;
        for (const std::string& line : lines) {
            body.append(TrimEnd(line));
            body.push_back('\n');
        }
        const bool ok = WriteAll(file, body);
        CloseHandle(file);
        if (!ok) {
            DeleteFileW(full.c_str());
            return std::nullopt;
        }
        RetainNewest(dir);
        return dirUtf8 + "\\" + name;
    }
    return std::nullopt;
}

void AppendLine(const std::string& name, const std::string& line) {
    if (!ValidName(name)) {
        return;
    }
    const std::wstring dir = Utf8ToWide(LogDir());
    EnsureDirectory(Utf8ToWide(LocalAppDataDir()));
    if (!EnsureDirectory(dir)) {
        return;
    }
    const std::wstring full = dir + L"\\" + Utf8ToWide(name);
    HANDLE file = CreateFileW(full.c_str(), FILE_APPEND_DATA, FILE_SHARE_READ,
                              nullptr, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL,
                              nullptr);
    if (file == INVALID_HANDLE_VALUE) {
        return;
    }
    std::string body = TrimEnd(line);
    body.push_back('\n');
    WriteAll(file, body);
    CloseHandle(file);
}

Json List() {
    const std::wstring dir = Utf8ToWide(LogDir());
    std::vector<FileInfo> files = Enumerate(dir);
    std::sort(files.begin(), files.end(),
              [](const FileInfo& a, const FileInfo& b) {
                  return a.mtime > b.mtime;
              });
    Json result = Json::array();
    for (const FileInfo& info : files) {
        Json item;
        item["name"] = info.name;
        item["size"] = info.size;
        item["mtime"] = info.mtime;
        result.push_back(std::move(item));
    }
    return result;
}

bool ValidLogName(const std::string& name) { return ValidName(name); }

std::optional<std::string> Read(const std::string& name) {
    if (!ValidName(name)) {
        return std::nullopt;
    }
    const std::wstring full = Utf8ToWide(LogDir()) + L"\\" + Utf8ToWide(name);
    HANDLE file = CreateFileW(full.c_str(), GENERIC_READ, FILE_SHARE_READ,
                              nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL,
                              nullptr);
    if (file == INVALID_HANDLE_VALUE) {
        return std::nullopt;
    }
    LARGE_INTEGER size;
    if (GetFileSizeEx(file, &size) == 0 || size.QuadPart > (16 << 20)) {
        CloseHandle(file);
        return std::nullopt;
    }
    std::string bytes(static_cast<size_t>(size.QuadPart), '\0');
    DWORD read = 0;
    const bool ok = ReadFile(file, bytes.data(),
                             static_cast<DWORD>(bytes.size()), &read, nullptr) != 0;
    CloseHandle(file);
    if (!ok) {
        return std::nullopt;
    }
    bytes.resize(read);
    return bytes;
}

}  // namespace logs
