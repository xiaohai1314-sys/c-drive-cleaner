#include "core/shred.h"

#include <windows.h>
#include <bcrypt.h>

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <string>
#include <utility>
#include <vector>

#include "core/logs.h"
#include "core/safety.h"
#include "core/task.h"
#include "core/timeutil.h"
#include "util/path.h"
#include "util/utf.h"
#include "util/win32.h"

namespace {

constexpr uint32_t kMaxPasses = 3;
constexpr DWORD kDeleteAccess = 0x0001'0000;
constexpr size_t kBlockSize = 1u << 20;

struct ShredReport {
    uint64_t freed = 0;
    uint64_t filesOk = 0;
    uint64_t filesFailed = 0;
    uint64_t finished = 0;
    std::vector<std::string> errors;
};

bool FillRandom(uint8_t* buffer, size_t length) {
    if (length == 0) {
        return true;
    }
    return BCryptGenRandom(nullptr, buffer, static_cast<ULONG>(length),
                           BCRYPT_USE_SYSTEM_PREFERRED_RNG) == 0;
}

std::string FormatMb(uint64_t bytes) {
    char buffer[64];
    std::snprintf(buffer, sizeof(buffer), "%.1f",
                  static_cast<double>(bytes) / 1048576.0);
    return buffer;
}

/// 覆盖写入后按句柄删除；成功返回文件大小。
std::optional<uint64_t> ShredFile(const std::string& path, uint32_t passes,
                                  std::string& error) {
    const HANDLE handle = CreateFileW(
        Utf8ToWide(path).c_str(), GENERIC_READ | GENERIC_WRITE | kDeleteAccess,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr,
        OPEN_EXISTING, FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
    if (handle == INVALID_HANDLE_VALUE) {
        error = "无法安全打开文件：" + path + "（" +
                FormatWin32Error(GetLastError()) + "）";
        return std::nullopt;
    }
    UniqueHandle file(handle);

    BY_HANDLE_FILE_INFORMATION info = {};
    if (GetFileInformationByHandle(file.get(), &info) == 0) {
        error = path + "：" + FormatWin32Error(GetLastError());
        return std::nullopt;
    }
    if ((info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 ||
        (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
        error = "不能粉碎符号链接或目录联接：" + path;
        return std::nullopt;
    }
    const uint64_t size = (static_cast<uint64_t>(info.nFileSizeHigh) << 32) |
                          static_cast<uint64_t>(info.nFileSizeLow);

    std::vector<uint8_t> buffer(kBlockSize);
    for (uint32_t pass = 0; pass < std::max<uint32_t>(passes, 1); ++pass) {
        LARGE_INTEGER start;
        start.QuadPart = 0;
        if (SetFilePointerEx(file.get(), start, nullptr, FILE_BEGIN) == 0) {
            error = path + "：" + FormatWin32Error(GetLastError());
            return std::nullopt;
        }
        uint64_t left = size;
        while (left > 0) {
            const size_t block =
                static_cast<size_t>(std::min<uint64_t>(left, buffer.size()));
            if (!FillRandom(buffer.data(), block)) {
                error = "获取安全随机数失败：随机数生成器不可用";
                return std::nullopt;
            }
            DWORD written = 0;
            if (WriteFile(file.get(), buffer.data(), static_cast<DWORD>(block),
                          &written, nullptr) == 0 ||
                written != static_cast<DWORD>(block)) {
                error = "覆盖写入失败：" + path + "（" +
                        FormatWin32Error(GetLastError()) + "）";
                return std::nullopt;
            }
            left -= block;
        }
        if (FlushFileBuffers(file.get()) == 0) {
            error = path + "：" + FormatWin32Error(GetLastError());
            return std::nullopt;
        }
    }
    if (!safety::MarkDelete(file.get())) {
        error = "覆盖后无法删除：" + path;
        return std::nullopt;
    }
    return size;
}

void ShredDirectory(const std::string& root, uint32_t passes,
                    ShredReport& report) {
    std::vector<std::string> files;
    std::vector<std::string> dirs;

    std::vector<std::string> pending{root};
    while (!pending.empty()) {
        const std::string current = std::move(pending.back());
        pending.pop_back();
        dirs.push_back(current);

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
            if ((data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0) {
                continue;
            }
            const std::string child = current + "\\" + WideToUtf8(name);
            if ((data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
                pending.push_back(child);
            } else {
                files.push_back(child);
            }
        } while (FindNextFileW(find, &data) != 0);
        FindClose(find);
    }

    for (const std::string& path : files) {
        if (task::IsCancelled("shred")) {
            report.errors.push_back("已取消");
            break;
        }
        task::AddDone("shred", 1);
        if (safety::IsProtectedPath(path)) {
            ++report.filesFailed;
            report.errors.push_back("为保护系统，拒绝粉碎：" + path);
            continue;
        }
        std::string error;
        const std::optional<uint64_t> size = ShredFile(path, passes, error);
        if (size.has_value()) {
            report.freed += *size;
            ++report.filesOk;
        } else {
            ++report.filesFailed;
            report.errors.push_back(std::move(error));
        }
    }

    std::stable_sort(dirs.begin(), dirs.end(),
                     [](const std::string& left, const std::string& right) {
                         return left.size() > right.size();
                     });
    for (const std::string& dir : dirs) {
        RemoveDirectoryW(Utf8ToWide(dir).c_str());
    }
    if (report.filesFailed == 0 &&
        RemoveDirectoryW(Utf8ToWide(root).c_str()) != 0) {
        ++report.finished;
    }
}

ShredReport ShredPathsInternal(const std::vector<std::string>& paths,
                               uint32_t passes) {
    ShredReport report;
    for (const std::string& path : paths) {
        if (task::IsCancelled("shred")) {
            report.errors.push_back("已取消");
            break;
        }
        if (path.find_first_not_of(" \t\r\n") == std::string::npos ||
            !PathExists(path)) {
            ++report.filesFailed;
            report.errors.push_back("文件不存在：" + path);
            continue;
        }
        if (safety::IsReparsePoint(path)) {
            ++report.filesFailed;
            report.errors.push_back("不能粉碎符号链接或目录联接：" + path);
            continue;
        }
        if (safety::IsProtectedPath(path)) {
            ++report.filesFailed;
            report.errors.push_back("为保护系统，拒绝粉碎：" + path);
            continue;
        }
        const DWORD attributes = GetFileAttributesW(Utf8ToWide(path).c_str());
        if (attributes != INVALID_FILE_ATTRIBUTES &&
            (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
            ShredDirectory(path, passes, report);
            continue;
        }
        task::AddDone("shred", 1);
        std::string error;
        const std::optional<uint64_t> size = ShredFile(path, passes, error);
        if (size.has_value()) {
            report.freed += *size;
            ++report.filesOk;
        } else {
            ++report.filesFailed;
            report.errors.push_back(std::move(error));
        }
    }
    return report;
}

Json ToJson(const ShredReport& report) {
    return Json{{"freed", report.freed},
                {"files_ok", report.filesOk},
                {"files_failed", report.filesFailed},
                {"finished", report.finished},
                {"errors", report.errors}};
}

}  // namespace

namespace shred {

Json ShredPaths(const std::vector<std::string>& paths, uint32_t passes) {
    return ToJson(
        ShredPathsInternal(paths, std::clamp<uint32_t>(passes, 1, kMaxPasses)));
}

Json Shred(const std::vector<std::string>& paths,
           std::optional<uint32_t> passes) {
    const uint32_t passCount =
        std::clamp<uint32_t>(passes.value_or(1), 1, kMaxPasses);
    try {
        task::Begin("shred", "文件粉碎", true, true, 0, "个文件");
    } catch (const std::exception& error) {
        ShredReport report;
        report.errors.push_back(error.what());
        return ToJson(report);
    }
    task::Guard guard("shred");
    task::SetUnit("shred", "个文件");
    task::SetStage("shred", "粉碎文件");
    const ShredReport report = ShredPathsInternal(paths, passCount);
    const std::string summary =
        "成功 " + std::to_string(report.filesOk) + " 个，失败 " +
        std::to_string(report.filesFailed) + " 个，释放 " +
        FormatMb(report.freed) + " MB";
    guard.Finish(task::IsCancelled("shred") ? "cancelled" : "ok", summary);
    logs::AppendLine("shred_history.log",
                     HumanStamp(NowUnixSeconds()) + "\tok\t成功 " +
                         std::to_string(report.filesOk) + " 个，失败 " +
                         std::to_string(report.filesFailed) + " 个");
    return ToJson(report);
}

}  // namespace shred
