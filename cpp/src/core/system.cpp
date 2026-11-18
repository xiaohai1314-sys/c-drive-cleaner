#include "core/system.h"

#include <windows.h>
#include <shellapi.h>

#include <algorithm>
#include <cctype>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <optional>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

#include "core/disk.h"
#include "core/im.h"
#include "core/logs.h"
#include "core/process.h"
#include "core/safety.h"
#include "core/task.h"
#include "core/timeutil.h"
#include "util/env.h"
#include "util/json.h"
#include "util/path.h"
#include "util/utf.h"
#include "util/win32.h"

namespace {

constexpr uint32_t kSherbNoConfirmation = 0x0000'0001;
constexpr uint32_t kSherbNoProgressUi = 0x0000'0002;
constexpr uint32_t kSherbNoSound = 0x0000'0004;
constexpr uint32_t kFileAttributeReparsePoint = 0x0000'0400;
constexpr uint64_t kOneGb = 1024ull * 1024ull * 1024ull;

std::string LowerAscii(std::string text) {
    for (char& ch : text) {
        ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    }
    return text;
}

bool EqualsIgnoreCase(const std::string& left, const std::string& right) {
    return LowerAscii(left) == LowerAscii(right);
}

std::string TrimEndSlashes(std::string text) {
    while (!text.empty() && (text.back() == '\\' || text.back() == '/')) {
        text.pop_back();
    }
    return text;
}

std::string TrimAsciiOf(const std::string& text) {
    size_t begin = 0;
    size_t end = text.size();
    while (begin < end &&
           std::isspace(static_cast<unsigned char>(text[begin])) != 0) {
        ++begin;
    }
    while (end > begin &&
           std::isspace(static_cast<unsigned char>(text[end - 1])) != 0) {
        --end;
    }
    return text.substr(begin, end - begin);
}

std::string SlashToBackslash(std::string path) {
    for (char& ch : path) {
        if (ch == '/') {
            ch = '\\';
        }
    }
    return path;
}

std::string ProgramDataDir() {
    std::string value = GetEnv("ProgramData");
    if (value.empty()) {
        value = "C:\\ProgramData";
    }
    return value;
}

std::string HiberPath() {
    std::string drive = GetEnv("SystemDrive");
    if (drive.empty()) {
        drive = "C:";
    }
    return TrimEndSlashes(drive) + "\\hiberfil.sys";
}

std::string UpdateDownloadDir() {
    return WindowsDir() + "\\SoftwareDistribution\\Download";
}

std::string DeliveryOptDir() {
    return WindowsDir() + "\\SoftwareDistribution\\DeliveryOptimization";
}

std::vector<std::string> CrashDumpDirs() {
    std::vector<std::string> dirs{WindowsDir() + "\\Minidump",
                                  ProgramDataDir() + "\\Microsoft\\Windows\\WER"};
    const std::string local = GetEnv("LOCALAPPDATA");
    if (!local.empty()) {
        dirs.push_back(local + "\\CrashDumps");
    }
    return dirs;
}

bool IsDirectory(const std::string& path) {
    const DWORD attributes = GetFileAttributesW(Utf8ToWide(path).c_str());
    return attributes != INVALID_FILE_ATTRIBUTES &&
           (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
}

std::string ParentOf(const std::string& path) {
    std::string trimmed = path;
    while (trimmed.size() > 3 && (trimmed.back() == '\\' || trimmed.back() == '/')) {
        trimmed.pop_back();
    }
    const size_t slash = trimmed.find_last_of("\\/");
    if (slash == std::string::npos || slash < 2) {
        return {};
    }
    return trimmed.substr(0, slash);
}

std::string NameOf(const std::string& path) {
    std::string trimmed = path;
    while (trimmed.size() > 3 && (trimmed.back() == '\\' || trimmed.back() == '/')) {
        trimmed.pop_back();
    }
    const size_t slash = trimmed.find_last_of("\\/");
    return slash == std::string::npos ? trimmed : trimmed.substr(slash + 1);
}

bool IsAbsolutePath(const std::string& path) {
    return path.size() >= 3 &&
           std::isalpha(static_cast<unsigned char>(path[0])) != 0 &&
           path[1] == ':' && (path[2] == '\\' || path[2] == '/');
}

/// 从 path 自身开始逐级向上检查重解析点；不存在的层级跳过（恢复时属正常）。
bool PathHasReparseAncestor(const std::string& path) {
    std::string current = path;
    while (!current.empty()) {
        const DWORD attributes =
            GetFileAttributesW(Utf8ToWide(current).c_str());
        if (attributes == INVALID_FILE_ATTRIBUTES) {
            const DWORD error = GetLastError();
            if (error != ERROR_FILE_NOT_FOUND &&
                error != ERROR_PATH_NOT_FOUND) {
                return true;
            }
        } else if ((attributes & kFileAttributeReparsePoint) != 0) {
            return true;
        }
        const std::string parent = ParentOf(current);
        if (parent.empty() || parent == current) {
            break;
        }
        current = parent;
    }
    return false;
}

bool PathHasReparseParent(const std::string& path) {
    const std::string parent = ParentOf(path);
    if (parent.empty()) {
        return true;
    }
    return PathHasReparseAncestor(parent);
}

std::pair<uint64_t, uint64_t> DirSizeCount(const std::string& dir) {
    uint64_t size = 0;
    uint64_t count = 0;
    std::vector<std::string> pending{dir};
    while (!pending.empty()) {
        const std::string current = std::move(pending.back());
        pending.pop_back();
        WIN32_FIND_DATAW data;
        const HANDLE find =
            FindFirstFileW((Utf8ToWide(current) + L"\\*").c_str(), &data);
        if (find == INVALID_HANDLE_VALUE) {
            continue;
        }
        do {
            const std::wstring name(data.cFileName);
            if (name.empty() || name == L"." || name == L"..") {
                continue;
            }
            if ((data.dwFileAttributes & kFileAttributeReparsePoint) != 0) {
                continue;
            }
            const std::string child = current + "\\" + WideToUtf8(name);
            if ((data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
                pending.push_back(child);
            } else {
                size += (static_cast<uint64_t>(data.nFileSizeHigh) << 32) |
                        static_cast<uint64_t>(data.nFileSizeLow);
                ++count;
            }
        } while (FindNextFileW(find, &data) != 0);
        FindClose(find);
    }
    return {size, count};
}

/// 清空目录内容（保留目录本身），逐个文件走句柄相对删除。
std::pair<uint64_t, uint64_t> CleanDirContents(const std::string& dir) {
    if (!IsDirectory(dir) || safety::IsReparsePoint(dir)) {
        return {0, 0};
    }
    const std::vector<std::string> roots{dir};
    uint64_t freed = 0;
    uint64_t count = 0;
    bool cancelled = false;
    std::vector<std::string> pending{dir};
    while (!pending.empty() && !cancelled) {
        const std::string current = std::move(pending.back());
        pending.pop_back();
        WIN32_FIND_DATAW data;
        const HANDLE find =
            FindFirstFileW((Utf8ToWide(current) + L"\\*").c_str(), &data);
        if (find == INVALID_HANDLE_VALUE) {
            continue;
        }
        do {
            if (task::IsCancelled("system")) {
                cancelled = true;
                break;
            }
            const std::wstring name(data.cFileName);
            if (name.empty() || name == L"." || name == L"..") {
                continue;
            }
            if ((data.dwFileAttributes & kFileAttributeReparsePoint) != 0) {
                continue;
            }
            const std::string child = current + "\\" + WideToUtf8(name);
            if ((data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
                pending.push_back(child);
                continue;
            }
            const uint64_t length =
                (static_cast<uint64_t>(data.nFileSizeHigh) << 32) |
                static_cast<uint64_t>(data.nFileSizeLow);
            const safety::DeleteOutcome outcome =
                safety::DeleteFileChecked(child, roots, false);
            if (outcome.kind == safety::DeleteOutcomeKind::Deleted) {
                freed += length;
                ++count;
                task::AddDone("system", 1);
            }
        } while (FindNextFileW(find, &data) != 0);
        FindClose(find);
    }
    safety::RemoveEmptyDirs(dir);
    return {freed, count};
}

bool ServiceRunning(const std::string& name) {
    const ProcessResult result = RunCapture(
        Utf8ToWide(System32Path("sc.exe")), {L"query", Utf8ToWide(name)});
    return result.started && result.out.find("RUNNING") != std::string::npos;
}

void StartServices(const std::vector<std::string>& names) {
    for (auto it = names.rbegin(); it != names.rend(); ++it) {
        RunCapture(Utf8ToWide(System32Path("net.exe")),
                   {L"start", Utf8ToWide(*it)});
    }
}

std::vector<std::string> StopServices(
    const std::vector<std::string>& names) {
    std::vector<std::string> stopped;
    for (const std::string& name : names) {
        if (!ServiceRunning(name)) {
            continue;
        }
        RunCapture(Utf8ToWide(System32Path("net.exe")),
                   {L"stop", Utf8ToWide(name), L"/y"});
        if (ServiceRunning(name)) {
            StartServices(stopped);
            throw std::runtime_error(
                name +
                " 服务正在工作且无法暂停，请等待 Windows 更新完成后再清理");
        }
        stopped.push_back(name);
    }
    return stopped;
}

std::string FormatMb(uint64_t bytes) {
    char buffer[64];
    std::snprintf(buffer, sizeof(buffer), "%.2f",
                  static_cast<double>(bytes) / 1048576.0);
    return buffer;
}

// ---------- 搬家日志 ----------

std::string JournalPath() {
    return JoinPath(CacheDir(), "im_move_pending.json");
}

bool WriteJournal(const std::string& source, const std::string& backup,
                  const std::string& kind, DWORD& error) {
    const std::string text =
        Json{{"source", source}, {"backup", backup}, {"kind", kind}}.dump();
    const std::vector<std::string> roots{CacheDir()};
    safety::HandleChain chain;
    UniqueHandle file;
    if (!safety::CreateNewFileChecked(JournalPath(), roots, chain, file,
                                      error)) {
        return false;
    }
    DWORD written = 0;
    if (WriteFile(file.get(), text.data(), static_cast<DWORD>(text.size()),
                  &written, nullptr) == 0 ||
        written != text.size()) {
        error = GetLastError();
        safety::DeleteFileChecked(JournalPath(), roots, false);
        return false;
    }
    if (FlushFileBuffers(file.get()) == 0) {
        error = GetLastError();
        safety::DeleteFileChecked(JournalPath(), roots, false);
        return false;
    }
    return true;
}

std::optional<std::string> ReadJournal() {
    const std::vector<std::string> roots{CacheDir()};
    safety::HandleChain chain;
    UniqueHandle file;
    DWORD error = 0;
    if (!safety::OpenReadFileChecked(JournalPath(), roots, chain, file, error)) {
        return std::nullopt;
    }
    std::string text;
    char buffer[4096];
    while (true) {
        DWORD read = 0;
        if (ReadFile(file.get(), buffer, sizeof(buffer), &read, nullptr) == 0) {
            return std::nullopt;
        }
        if (read == 0) {
            break;
        }
        text.append(buffer, read);
    }
    return text;
}

void ClearJournal() {
    safety::DeleteFileChecked(JournalPath(), {CacheDir()}, false);
}

bool BackupAbsent(const std::string& backup) {
    const DWORD attributes = GetFileAttributesW(Utf8ToWide(backup).c_str());
    return attributes == INVALID_FILE_ATTRIBUTES &&
           (GetLastError() == ERROR_FILE_NOT_FOUND ||
            GetLastError() == ERROR_PATH_NOT_FOUND);
}

bool CheckRecoveryPaths(const std::string& source, const std::string& backup,
                        const std::vector<std::string>& allowedRoots) {
    if (safety::PathSyntaxError(source) || safety::PathSyntaxError(backup) ||
        safety::IsProtectedPath(source) || safety::IsNeverClean(source) ||
        safety::IsDriveRoot(source) || safety::IsCriticalFile(source) ||
        safety::IsPasswordStore(source)) {
        return false;
    }
    if (!IsAbsolutePath(source) || !IsAbsolutePath(backup)) {
        return false;
    }
    const std::string sourceNormalized = safety::Normalize(source);
    bool allowed = false;
    for (const std::string& root : allowedRoots) {
        if (EqualsIgnoreCase(safety::Normalize(root), sourceNormalized)) {
            allowed = true;
            break;
        }
    }
    if (!allowed) {
        return false;
    }
    const std::string sourceParent = ParentOf(source);
    const std::string backupParent = ParentOf(backup);
    const std::string sourceName = NameOf(source);
    const std::string backupName = NameOf(backup);
    if (sourceParent.empty() || backupParent.empty() || sourceName.empty() ||
        backupName.empty()) {
        return false;
    }
    if (!EqualsIgnoreCase(safety::Normalize(sourceParent),
                          safety::Normalize(backupParent))) {
        return false;
    }
    const std::string loweredBackup = LowerAscii(backupName);
    const std::string prefix = LowerAscii(sourceName) + "_backup_";
    if (loweredBackup.rfind(prefix, 0) != 0) {
        return false;
    }
    const std::string stamp = loweredBackup.substr(prefix.size());
    if (stamp.size() != 15 || stamp[8] != '_') {
        return false;
    }
    for (size_t index = 0; index < stamp.size(); ++index) {
        if (index == 8) {
            continue;
        }
        if (std::isdigit(static_cast<unsigned char>(stamp[index])) == 0) {
            return false;
        }
    }
    return true;
}

bool CheckQuotedCmdPath(const std::string& path) {
    if (path.empty()) {
        return false;
    }
    for (const char ch : path) {
        const unsigned char byte = static_cast<unsigned char>(ch);
        if (ch == '"' || ch == '%' || byte < 0x20 || byte == 0x7F) {
            return false;
        }
    }
    return true;
}

// ---------- 复制与校验 ----------

bool CopyDirContents(const std::string& src, const std::string& dst,
                     const std::vector<std::string>& sourceRoot,
                     const std::vector<std::string>& targetRoot,
                     DWORD& error) {
    safety::HandleChain sourceChain;
    if (!safety::PinDirectoryChecked(src, sourceRoot, sourceChain, error)) {
        return false;
    }
    WIN32_FIND_DATAW data;
    const HANDLE find = FindFirstFileW((Utf8ToWide(src) + L"\\*").c_str(), &data);
    if (find == INVALID_HANDLE_VALUE) {
        error = GetLastError();
        return false;
    }
    bool ok = true;
    do {
        const std::wstring name(data.cFileName);
        if (name.empty() || name == L"." || name == L"..") {
            continue;
        }
        if (task::IsCancelled("im_move")) {
            error = ERROR_CANCELLED;
            ok = false;
            break;
        }
        const std::string from = src + "\\" + WideToUtf8(name);
        const std::string to = dst + "\\" + WideToUtf8(name);
        const std::optional<bool> reparse = safety::ReparsePointStatus(from);
        if (!reparse.has_value()) {
            error = ERROR_GEN_FAILURE;
            ok = false;
            break;
        }
        if (*reparse) {
            continue;
        }
        if ((data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
            safety::HandleChain directoryChain;
            UniqueHandle directory;
            if (!safety::CreateDirectoryChecked(to, targetRoot, directoryChain,
                                                directory, error)) {
                ok = false;
                break;
            }
            if (!CopyDirContents(from, to, sourceRoot, targetRoot, error)) {
                ok = false;
                break;
            }
            continue;
        }
        safety::HandleChain sourceParents;
        UniqueHandle sourceFile;
        if (!safety::OpenReadFileChecked(from, sourceRoot, sourceParents,
                                         sourceFile, error)) {
            ok = false;
            break;
        }
        safety::HandleChain targetParents;
        UniqueHandle targetFile;
        if (!safety::CreateNewFileChecked(to, targetRoot, targetParents,
                                          targetFile, error)) {
            ok = false;
            break;
        }
        uint64_t copied = 0;
        char buffer[65536];
        while (true) {
            DWORD read = 0;
            if (ReadFile(sourceFile.get(), buffer, sizeof(buffer), &read,
                         nullptr) == 0) {
                error = GetLastError();
                ok = false;
                break;
            }
            if (read == 0) {
                break;
            }
            DWORD written = 0;
            if (WriteFile(targetFile.get(), buffer, read, &written, nullptr) ==
                    0 ||
                written != read) {
                error = GetLastError();
                ok = false;
                break;
            }
            copied += read;
        }
        if (!ok) {
            break;
        }
        task::AddDone("im_move", copied);
        if (FlushFileBuffers(targetFile.get()) == 0) {
            error = GetLastError();
            ok = false;
            break;
        }
    } while (FindNextFileW(find, &data) != 0);
    FindClose(find);
    return ok;
}

struct PinnedChains {
    safety::HandleChain source;
    safety::HandleChain target;
};

bool CopyDirRecursive(const std::string& src, const std::string& dst,
                      PinnedChains& chains, DWORD& error) {
    const std::vector<std::string> sourceRoot{src};
    const std::vector<std::string> targetRoot{dst};
    if (!safety::PinDirectoryChecked(src, sourceRoot, chains.source, error)) {
        return false;
    }
    if (!safety::CreateDirectoryTreeChecked(dst, chains.target, error)) {
        return false;
    }
    if (!CopyDirContents(src, dst, sourceRoot, targetRoot, error)) {
        uint64_t removed = 0;
        safety::RemoveTreeChecked(dst, targetRoot, removed, error);
        return false;
    }
    return true;
}

bool VerifyCopy(const std::string& src, const std::string& dst,
                std::string& detail) {
    const std::vector<std::string> sourceRoot{src};
    const std::vector<std::string> targetRoot{dst};
    DWORD error = 0;
    safety::HandleChain sourceChain;
    if (!safety::PinDirectoryChecked(src, sourceRoot, sourceChain, error)) {
        detail = FormatWin32Error(error);
        return false;
    }
    safety::HandleChain targetChain;
    if (!safety::PinDirectoryChecked(dst, targetRoot, targetChain, error)) {
        detail = FormatWin32Error(error);
        return false;
    }

    struct WalkItem {
        std::string from;
        std::string to;
        std::string relative;
    };
    std::vector<WalkItem> pending;
    auto listDir = [](const std::string& dir) {
        std::vector<WIN32_FIND_DATAW> entries;
        WIN32_FIND_DATAW data;
        const HANDLE find =
            FindFirstFileW((Utf8ToWide(dir) + L"\\*").c_str(), &data);
        if (find == INVALID_HANDLE_VALUE) {
            return entries;
        }
        do {
            const std::wstring name(data.cFileName);
            if (name.empty() || name == L"." || name == L"..") {
                continue;
            }
            entries.push_back(data);
        } while (FindNextFileW(find, &data) != 0);
        FindClose(find);
        return entries;
    };

    struct DirItem {
        std::string dir;
        std::string relative;
    };
    std::vector<DirItem> dirs{{src, ""}};
    std::vector<WalkItem> files;
    while (!dirs.empty()) {
        const DirItem current = std::move(dirs.back());
        dirs.pop_back();
        for (const WIN32_FIND_DATAW& data : listDir(current.dir)) {
            const std::string name = WideToUtf8(data.cFileName);
            const std::string from = current.dir + "\\" + name;
            const std::string relative =
                current.relative.empty() ? name : current.relative + "\\" + name;
            const std::optional<bool> reparse = safety::ReparsePointStatus(from);
            if (!reparse.has_value()) {
                detail = FormatWin32Error(GetLastError());
                return false;
            }
            if (*reparse) {
                continue;
            }
            if ((data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
                dirs.push_back({from, relative});
            } else {
                files.push_back({from, dst + "\\" + relative, relative});
            }
        }
    }

    for (const WalkItem& item : files) {
        if (task::IsCancelled("im_move")) {
            detail = "已取消";
            return false;
        }
        safety::HandleChain sourceParents;
        UniqueHandle sourceFile;
        DWORD openError = 0;
        if (!safety::OpenReadFileChecked(item.from, sourceRoot, sourceParents,
                                         sourceFile, openError)) {
            detail = item.relative + " 无法安全读取：" +
                     FormatWin32Error(openError);
            return false;
        }
        safety::HandleChain targetParents;
        UniqueHandle targetFile;
        if (!safety::OpenReadFileChecked(item.to, targetRoot, targetParents,
                                         targetFile, openError)) {
            detail = item.relative + " 未完整拷贝：" +
                     FormatWin32Error(openError);
            return false;
        }
        LARGE_INTEGER sourceSize;
        LARGE_INTEGER targetSize;
        if (GetFileSizeEx(sourceFile.get(), &sourceSize) == 0 ||
            GetFileSizeEx(targetFile.get(), &targetSize) == 0) {
            detail = FormatWin32Error(GetLastError());
            return false;
        }
        if (sourceSize.QuadPart != targetSize.QuadPart) {
            detail = item.relative + " 大小不一致";
            return false;
        }
        std::vector<uint8_t> left(64 * 1024);
        std::vector<uint8_t> right(64 * 1024);
        while (true) {
            DWORD leftRead = 0;
            DWORD rightRead = 0;
            if (ReadFile(sourceFile.get(), left.data(),
                         static_cast<DWORD>(left.size()), &leftRead,
                         nullptr) == 0 ||
                ReadFile(targetFile.get(), right.data(),
                         static_cast<DWORD>(right.size()), &rightRead,
                         nullptr) == 0) {
                detail = FormatWin32Error(GetLastError());
                return false;
            }
            if (leftRead != rightRead ||
                std::memcmp(left.data(), right.data(), leftRead) != 0) {
                detail = item.relative + " 内容不一致";
                return false;
            }
            if (leftRead == 0) {
                break;
            }
        }
        task::AddDone("im_move", 1);
    }
    return true;
}

bool CleanupTarget(const std::string& target,
                   safety::HandleChain& targetChain) {
    const std::vector<std::string> roots{target};
    uint64_t removed = 0;
    DWORD error = 0;
    const bool first = safety::RemoveTreeChecked(target, roots, removed, error);
    targetChain.handles.clear();
    if (first) {
        return true;
    }
    return safety::RemoveTreeChecked(target, roots, removed, error);
}

}  // namespace

namespace sysclean {

std::pair<uint64_t, uint64_t> DirectorySizeCount(const std::string& dir) {
    return DirSizeCount(dir);
}

std::pair<uint64_t, uint64_t> CleanDirectoryContents(const std::string& dir) {
    return CleanDirContents(dir);
}

bool ValidRecoveryPaths(const std::string& source, const std::string& backup,
                        const std::vector<std::string>& allowedRoots) {
    return CheckRecoveryPaths(source, backup, allowedRoots);
}

bool SafeForQuotedCmdPath(const std::string& path) {
    return CheckQuotedCmdPath(path);
}

bool CopyDirectory(const std::string& source, const std::string& target,
                   std::string& error) {
    PinnedChains chains;
    DWORD code = 0;
    if (!CopyDirRecursive(source, target, chains, code)) {
        error = FormatWin32Error(code);
        return false;
    }
    return true;
}

bool VerifyDirectoryCopy(const std::string& source, const std::string& target,
                         std::string& detail) {
    return VerifyCopy(source, target, detail);
}

Json ScanSystem() {
    Json items = Json::array();

    const std::string updateDownload = UpdateDownloadDir();
    if (IsDirectory(updateDownload)) {
        const auto [size, count] = DirSizeCount(updateDownload);
        items.push_back(
            Json{{"key", "sys_update_dl"},
                 {"name", "Windows 更新下载安装包"},
                 {"describe",
                  "系统更新完成后的旧安装包和补丁残留，清理后不影响已安装的更新"},
                 {"size", size},
                 {"count", count},
                 {"safe_to_clean", true},
                 {"is_command", false}});
    }

    const std::string deliveryOpt = DeliveryOptDir();
    if (IsDirectory(deliveryOpt)) {
        const auto [size, count] = DirSizeCount(deliveryOpt);
        items.push_back(Json{{"key", "sys_delivery_opt"},
                             {"name", "Windows 传递优化网络缓存"},
                             {"describe", "微软更新与其他电脑局域网共享的下载分片缓存"},
                             {"size", size},
                             {"count", count},
                             {"safe_to_clean", true},
                             {"is_command", false}});
    }

    uint64_t dumpSize = 0;
    uint64_t dumpCount = 0;
    for (const std::string& dir : CrashDumpDirs()) {
        if (IsDirectory(dir)) {
            const auto [size, count] = DirSizeCount(dir);
            dumpSize += size;
            dumpCount += count;
        }
    }
    items.push_back(Json{{"key", "sys_crash_dumps"},
                         {"name", "系统崩溃转储与错误报告"},
                         {"describe", "软件或系统蓝屏崩溃时记录的 .dmp 文件与诊断日志"},
                         {"size", dumpSize},
                         {"count", dumpCount},
                         {"safe_to_clean", true},
                         {"is_command", false}});

    items.push_back(Json{
        {"key", "sys_dism_winsxs"},
        {"name", "WinSxS 系统组件库精简 (DISM)"},
        {"describe",
         "执行 DISM 原生命令精简 WinSxS 目录下的历史备份更新，通常可释放 "
         "3~15GB（耗时约 1~3 分钟）"},
        {"size", 0},
        {"count", 0},
        {"safe_to_clean", true},
        {"is_command", true}});
    return items;
}

std::string CleanSystemItem(const std::string& key) {
    if (key == "sys_update_dl") {
        // 更新服务运行时删 Download 可能打断正在进行的更新。
        task::SetStage("system", "停止更新服务");
        const std::vector<std::string> stopped =
            StopServices({"wuauserv", "bits"});
        task::SetStage("system", "清理更新安装包");
        const auto [freed, count] = CleanDirContents(UpdateDownloadDir());
        task::SetStage("system", "恢复更新服务");
        StartServices(stopped);
        return "已清理更新安装包 " + std::to_string(count) + " 个，释放 " +
               FormatMb(freed) + " MB";
    }
    if (key == "sys_delivery_opt") {
        task::SetStage("system", "调用系统清理传递优化缓存");
        const std::string dir = DeliveryOptDir();
        const auto [before, beforeCount] = DirSizeCount(dir);
        const ProcessResult result = RunCapture(
            Utf8ToWide(System32Path("WindowsPowerShell\\v1.0\\powershell.exe")),
            {L"-NoProfile", L"-NonInteractive", L"-ExecutionPolicy", L"Bypass",
             L"-Command",
             L"Delete-DeliveryOptimizationCache -Force -ErrorAction Stop"});
        if (!result.started) {
            throw std::runtime_error("执行传递优化清理失败：进程无法启动");
        }
        if (result.exitCode != 0) {
            throw std::runtime_error("传递优化缓存清理失败：" +
                                     TrimEndSlashes(TrimAsciiOf(result.err)));
        }
        const auto [after, afterCount] = DirSizeCount(dir);
        const uint64_t freed = before > after ? before - after : 0;
        const uint64_t count = beforeCount > afterCount ? beforeCount - afterCount : 0;
        return "已清理传递优化分片 " + std::to_string(count) + " 个，释放 " +
               FormatMb(freed) + " MB";
    }
    if (key == "sys_crash_dumps") {
        task::SetStage("system", "清理崩溃转储与错误报告");
        uint64_t totalFreed = 0;
        uint64_t totalCount = 0;
        for (const std::string& dir : CrashDumpDirs()) {
            const auto [freed, count] = CleanDirContents(dir);
            totalFreed += freed;
            totalCount += count;
        }
        return "已清理转储文件 " + std::to_string(totalCount) + " 个，释放 " +
               FormatMb(totalFreed) + " MB";
    }
    if (key == "sys_dism_winsxs") {
        task::SetStage("system", "DISM 组件库精简（约 1~3 分钟）");
        const ProcessResult result =
            RunCapture(Utf8ToWide(System32Path("Dism.exe")),
                       {L"/Online", L"/Cleanup-Image",
                        L"/StartComponentCleanup"});
        if (!result.started) {
            throw std::runtime_error("执行 DISM 失败：进程无法启动");
        }
        if (result.exitCode == 0) {
            return "DISM WinSxS 组件库精简优化已成功完成！";
        }
        throw std::runtime_error("DISM 组件精简失败：" +
                                 TrimAsciiOf(result.err));
    }
    throw std::runtime_error("未知系统清理项");
}

Json QueryHiber() {
    const std::string path = HiberPath();
    uint64_t size = 0;
    bool exists = false;
    WIN32_FILE_ATTRIBUTE_DATA info = {};
    if (GetFileAttributesExW(Utf8ToWide(path).c_str(), GetFileExInfoStandard,
                             &info) != 0) {
        size = (static_cast<uint64_t>(info.nFileSizeHigh) << 32) |
               static_cast<uint64_t>(info.nFileSizeLow);
        exists = true;
    }
    std::string description;
    if (exists) {
        char buffer[512];
        std::snprintf(buffer, sizeof(buffer),
                      "休眠文件正在占用 %.2f GB 空间。若无需使用系统休眠（如只用"
                      "睡眠），可一键关闭释放空间。",
                      static_cast<double>(size) / (1024.0 * 1024.0 * 1024.0));
        description = buffer;
    } else {
        description = "系统休眠未开启，未占用磁盘空间。";
    }
    return Json{{"enabled", exists},
                {"size", size},
                {"path", path},
                {"description", description}};
}

std::string SetHiber(const std::string& mode) {
    std::vector<std::wstring> args;
    if (mode == "off") {
        args = {L"/h", L"off"};
    } else if (mode == "reduced") {
        args = {L"/h", L"/type", L"reduced"};
    } else if (mode == "full" || mode == "on") {
        args = {L"/h", L"on"};
    } else {
        throw std::runtime_error("无效的休眠模式参数");
    }
    const ProcessResult result =
        RunCapture(Utf8ToWide(System32Path("powercfg.exe")), args);
    if (!result.started) {
        throw std::runtime_error("执行 powercfg 失败：进程无法启动");
    }
    if (result.exitCode != 0) {
        throw std::runtime_error("设置休眠失败：" + TrimAsciiOf(result.err));
    }
    if (mode == "off") {
        return "休眠功能已彻底关闭，hiberfil.sys 已移除并释放全部空间。";
    }
    if (mode == "reduced") {
        return "休眠文件已精简为 Reduced 模式（体积缩减约 50%）。";
    }
    return "休眠功能已开启。";
}

void EmptyRecycleBins() {
    const HRESULT code = SHEmptyRecycleBinW(
        nullptr, nullptr,
        kSherbNoConfirmation | kSherbNoProgressUi | kSherbNoSound);
    // S_OK = 0, S_FALSE = 1
    if (code == S_OK || code == S_FALSE) {
        return;
    }
    char buffer[64];
    std::snprintf(buffer, sizeof(buffer), "清空回收站返回错误码：0x%X",
                  static_cast<unsigned>(code));
    throw std::runtime_error(buffer);
}

Json AnalyzeImMigration(const std::string& kind) {
    const std::vector<std::string> roots = im::Roots(kind);
    std::string bestSource;
    uint64_t maxSize = 0;
    uint64_t maxCount = 0;
    for (const std::string& root : roots) {
        const std::string normalized = safety::Normalize(root);
        if (im::IsProgramRoot(normalized)) {
            continue;
        }
        if (normalized.size() >= 2 && normalized[0] == 'C' &&
            normalized[1] == ':') {
            const auto [size, count] = DirSizeCount(normalized);
            if (size > maxSize) {
                maxSize = size;
                maxCount = count;
                bestSource = normalized;
            }
        }
    }

    const bool isJunction =
        !bestSource.empty() && safety::IsReparsePoint(bestSource);

    Json targets = Json::array();
    for (const std::string& drive : disk::FixedDrives()) {
        if (drive.size() >= 2 && (drive[0] == 'C' || drive[0] == 'c') &&
            drive[1] == ':') {
            continue;
        }
        const std::optional<disk::DriveSpace> space = disk::FreeSpace(drive);
        if (space.has_value()) {
            targets.push_back(Json{{"letter", drive},
                                   {"free_bytes", space->freeBytes},
                                   {"total_bytes", space->totalBytes}});
        }
    }

    return Json{{"source", bestSource},
                {"exists", !bestSource.empty()},
                {"size", maxSize},
                {"file_count", maxCount},
                {"is_junction", isJunction},
                {"target_drives", targets}};
}

std::string ExecuteImMigration(const std::string& sourceStr,
                               const std::string& targetDirStr,
                               const std::string& kind) {
    if (!CheckQuotedCmdPath(sourceStr) || !CheckQuotedCmdPath(targetDirStr)) {
        throw std::runtime_error("迁移路径包含不受支持的命令行字符");
    }
    if (!IsDirectory(sourceStr)) {
        throw std::runtime_error("源数据目录不存在");
    }
    if (kind != "wechat" && kind != "qq") {
        throw std::runtime_error("无效的搬家类型");
    }

    // 迁移和恢复日志都只接受探测到的根目录本身。
    const std::vector<std::string> roots = im::Roots(kind);
    const std::string sourceNorm = safety::Normalize(sourceStr);
    bool allowed = false;
    for (const std::string& root : roots) {
        if (EqualsIgnoreCase(safety::Normalize(root), sourceNorm)) {
            allowed = true;
            break;
        }
    }
    if (!allowed) {
        throw std::runtime_error(
            "只能迁移微信/QQ 自己探测到的数据根目录，已拒绝本次操作");
    }
    if (safety::ExplicitDeleteForbidden(sourceStr)) {
        throw std::runtime_error("该目录受保护，不能迁移");
    }
    const std::optional<bool> reparse = safety::ReparsePointStatus(sourceStr);
    if (!reparse.has_value()) {
        throw std::runtime_error("无法安全检查源目录：读取重解析点状态失败");
    }
    if (*reparse) {
        throw std::runtime_error("源目录已经是软链接/联接点，之前已经完成过搬家");
    }

    if (safety::IsWithin(targetDirStr, {sourceStr})) {
        throw std::runtime_error("目标目录不能位于源目录内部");
    }
    if (PathExists(targetDirStr)) {
        throw std::runtime_error("目标目录已存在，请换一个位置或先手动清理");
    }

    const std::vector<std::string> active =
        process::CheckRunning({"wechat.exe", "qq.exe", "xwechat.exe"});
    if (!active.empty()) {
        std::string names;
        for (const std::string& name : active) {
            if (!names.empty()) {
                names += "、";
            }
            names += name;
        }
        throw std::runtime_error("检测到 " + names +
                                 " 正在运行，请先彻底退出该软件后再执行搬家！");
    }

    task::SetStage("im_move", "统计源数据");
    const auto [srcSize, fileCount] = DirSizeCount(sourceStr);
    if (srcSize == 0 || fileCount == 0) {
        throw std::runtime_error("源数据目录是空的，无需搬家");
    }
    const std::string targetNorm = safety::Normalize(targetDirStr);
    const std::string targetDrive = targetNorm.substr(0, 3);
    bool fixedTarget = false;
    for (const std::string& drive : disk::FixedDrives()) {
        if (EqualsIgnoreCase(drive, targetDrive)) {
            fixedTarget = true;
            break;
        }
    }
    if (!fixedTarget) {
        throw std::runtime_error("目标位置不是本机固定磁盘");
    }
    if (const std::optional<disk::DriveSpace> space =
            disk::FreeSpace(targetDrive)) {
        if (space->freeBytes < srcSize + kOneGb) {
            throw std::runtime_error("目标磁盘剩余空间不足，请选择剩余空间更充裕的磁盘");
        }
    }

    task::SetStage("im_move", "复制文件");
    task::SetUnit("im_move", "字节");
    task::SetProgress("im_move", 0, srcSize);
    PinnedChains chains;
    DWORD copyError = 0;
    if (!CopyDirRecursive(sourceStr, targetDirStr, chains, copyError)) {
        throw std::runtime_error("拷贝文件至新目录失败：" +
                                 FormatWin32Error(copyError));
    }
    task::SetStage("im_move", "校验文件");
    task::SetUnit("im_move", "个文件");
    task::SetProgress("im_move", 0, fileCount);
    const auto [dstSize, dstCount] = DirSizeCount(targetDirStr);
    if (dstSize != srcSize || dstCount != fileCount) {
        chains.source.handles.clear();
        CleanupTarget(targetDirStr, chains.target);
        throw std::runtime_error(
            "拷贝校验失败（源 " + std::to_string(fileCount) + " 个文件 / " +
            std::to_string(srcSize) + " 字节，目标 " +
            std::to_string(dstCount) + " 个文件 / " + std::to_string(dstSize) +
            " 字节），已放弃搬家并清理目标目录");
    }
    std::string detail;
    if (!VerifyCopy(sourceStr, targetDirStr, detail)) {
        chains.source.handles.clear();
        CleanupTarget(targetDirStr, chains.target);
        throw std::runtime_error("逐文件校验失败（" + detail +
                                 "），已放弃搬家并清理目标目录");
    }
    chains.source.handles.clear();

    // 3. 将原目录重命名为临时备份（先写恢复日志）。
    task::SetStage("im_move", "备份原目录");
    const std::string backupPath =
        ParentOf(sourceStr) + "\\" + NameOf(sourceStr) + "_backup_" + Stamp();
    DWORD journalError = 0;
    if (!WriteJournal(sourceStr, backupPath, kind, journalError)) {
        CleanupTarget(targetDirStr, chains.target);
        if (journalError == ERROR_FILE_EXISTS ||
            journalError == ERROR_ALREADY_EXISTS) {
            throw std::runtime_error(
                "发现尚未解决的搬家恢复日志，原目录未改名；请先处理上次搬家留下"
                "的备份状态。");
        }
        throw std::runtime_error("搬家恢复日志未能可靠写入，原目录未改名：" +
                                 FormatWin32Error(journalError));
    }
    DWORD renameError = 0;
    if (!safety::RenameDirectorySiblingChecked(sourceStr, backupPath, roots,
                                               renameError)) {
        ClearJournal();
        CleanupTarget(targetDirStr, chains.target);
        throw std::runtime_error("备份原目录失败（可能被占用）：" +
                                 FormatWin32Error(renameError));
    }

    // 4. 在原位置创建 NTFS Junction。
    task::SetStage("im_move", "创建目录联接");
    const std::wstring commandLine =
        BuildCommandLine(Utf8ToWide(System32Path("cmd.exe")), {L"/D", L"/C"}) +
        L" mklink /J \"" +
        Utf8ToWide(TrimEndSlashes(SlashToBackslash(sourceStr))) + L"\" \"" +
        Utf8ToWide(TrimEndSlashes(SlashToBackslash(targetDirStr))) + L"\"";
    const ProcessResult result = RunCommandLine(commandLine);

    if (result.started && result.exitCode == 0) {
        task::SetStage("im_move", "清理备份");
        const std::vector<std::string> backupRoot{backupPath};
        uint64_t removedBytes = 0;
        DWORD removeError = 0;
        const bool removed = safety::RemoveTreeChecked(
            backupPath, backupRoot, removedBytes, removeError);
        if (removed) {
            ClearJournal();
        } else {
            logs::WriteLog({"搬家已完成，但备份清理未完成；恢复日志保留：" +
                            backupPath});
        }
        char sizeText[64];
        std::snprintf(sizeText, sizeof(sizeText), "%.2f",
                      static_cast<double>(srcSize) / (1024.0 * 1024.0 * 1024.0));
        return "数据搬家成功！已将 " + std::to_string(fileCount) + " 个文件（" +
               sizeText + " GB）迁移至 " + targetDirStr + "，原 C 盘空间已释放" +
               (removed ? "" : "（备份目录删除失败，请手动清理残留）") + "。";
    }

    // Junction 失败：只在句柄相对恢复成功后清日志。
    DWORD restoreError = 0;
    if (!safety::RenameDirectorySiblingChecked(backupPath, sourceStr, roots,
                                               restoreError)) {
        throw std::runtime_error(
            "创建目录联接失败，自动恢复暂未成功；原数据保留在 " + backupPath +
            "，恢复日志已保留供下次重试：" + FormatWin32Error(restoreError));
    }
    const bool targetRemoved = CleanupTarget(targetDirStr, chains.target);
    ClearJournal();
    if (targetRemoved) {
        throw std::runtime_error("创建 NTFS 目录联接失败，原数据已自动恢复。");
    }
    throw std::runtime_error(
        "创建 NTFS 目录联接失败，原数据已恢复；目标目录有残留，请手动清理。");
}

void RecoverPendingMove() {
    const std::optional<std::string> text = ReadJournal();
    if (!text.has_value()) {
        return;
    }
    const Json journal = Json::parse(*text, nullptr, false);
    if (journal.is_discarded() || !journal.is_object()) {
        return;
    }
    const std::string source = journal.value("source", std::string());
    const std::string backup = journal.value("backup", std::string());
    const std::string kind = journal.value("kind", std::string());
    if (source.empty() || backup.empty()) {
        return;
    }
    std::vector<std::string> allowedRoots;
    if (kind == "wechat") {
        allowedRoots = im::MigrationRoots("wechat");
    } else if (kind == "qq") {
        allowedRoots = im::MigrationRoots("qq");
    } else if (kind.empty()) {
        // 兼容旧版日志：合并微信与 QQ 的候选根目录。
        allowedRoots = im::MigrationRoots("wechat");
        const std::vector<std::string> qq = im::MigrationRoots("qq");
        allowedRoots.insert(allowedRoots.end(), qq.begin(), qq.end());
    } else {
        return;
    }
    if (!CheckRecoveryPaths(source, backup, allowedRoots) ||
        PathHasReparseParent(source) || PathHasReparseParent(backup)) {
        return;
    }

    const DWORD sourceAttributes =
        GetFileAttributesW(Utf8ToWide(source).c_str());
    if (sourceAttributes != INVALID_FILE_ATTRIBUTES) {
        // 源路径已存在：绝不覆盖；备份还在就保留日志供诊断。
        if (BackupAbsent(backup)) {
            ClearJournal();
        } else {
            logs::WriteLog({"搬家恢复日志保留：源路径已存在，但备份目录仍在或状态"
                            "不确定：" +
                            backup});
        }
        return;
    }
    const DWORD sourceError = GetLastError();
    if (sourceError != ERROR_FILE_NOT_FOUND &&
        sourceError != ERROR_PATH_NOT_FOUND) {
        return;
    }

    const DWORD backupAttributes =
        GetFileAttributesW(Utf8ToWide(backup).c_str());
    if (backupAttributes == INVALID_FILE_ATTRIBUTES ||
        (backupAttributes & FILE_ATTRIBUTE_DIRECTORY) == 0 ||
        (backupAttributes & kFileAttributeReparsePoint) != 0) {
        return;
    }
    DWORD error = 0;
    if (safety::RenameDirectorySiblingChecked(backup, source, allowedRoots,
                                              error)) {
        logs::WriteLog({"微信/QQ 搬家中断恢复：" + backup + " 已还原为 " + source});
        ClearJournal();
    }
}

}  // namespace sysclean
