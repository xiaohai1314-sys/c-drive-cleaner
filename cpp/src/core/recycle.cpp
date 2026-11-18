#include "core/recycle.h"

#include <windows.h>
#include <shellapi.h>

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <stdexcept>
#include <unordered_map>
#include <utility>

#include "core/logs.h"
#include "core/safety.h"
#include "core/task.h"
#include "util/utf.h"

namespace {

constexpr size_t kChunkSize = 40;

uint64_t FileSizeOf(const std::string& path) {
    WIN32_FILE_ATTRIBUTE_DATA data = {};
    if (GetFileAttributesExW(Utf8ToWide(path).c_str(), GetFileExInfoStandard,
                             &data) == 0) {
        return 0;
    }
    return (static_cast<uint64_t>(data.nFileSizeHigh) << 32) |
           static_cast<uint64_t>(data.nFileSizeLow);
}

bool FileExists(const std::string& path) {
    return GetFileAttributesW(Utf8ToWide(path).c_str()) !=
           INVALID_FILE_ATTRIBUTES;
}

bool IsRegularFile(const std::string& path) {
    const DWORD attributes = GetFileAttributesW(Utf8ToWide(path).c_str());
    return attributes != INVALID_FILE_ATTRIBUTES &&
           (attributes & FILE_ATTRIBUTE_DIRECTORY) == 0;
}

std::string FormatMb(uint64_t bytes) {
    char buffer[64];
    std::snprintf(buffer, sizeof(buffer), "%.1f",
                  static_cast<double>(bytes) / 1048576.0);
    return buffer;
}

std::vector<std::string> SendChunk(const std::vector<std::string>& paths) {
    std::vector<wchar_t> buffer;
    for (const std::string& path : paths) {
        const std::wstring wide = Utf8ToWide(path);
        buffer.insert(buffer.end(), wide.begin(), wide.end());
        buffer.push_back(L'\0');
    }
    buffer.push_back(L'\0');  // 双空字符结尾

    SHFILEOPSTRUCTW operation = {};
    operation.hwnd = nullptr;
    operation.wFunc = FO_DELETE;
    operation.pFrom = buffer.data();
    operation.pTo = nullptr;
    operation.fFlags = FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI;
    const int code = SHFileOperationW(&operation);
    if (code == 0 && operation.fAnyOperationsAborted == 0) {
        return {};
    }
    return paths;
}

}  // namespace

namespace recycle {

std::vector<std::string> SendToRecycle(const std::vector<std::string>& paths) {
    std::vector<std::string> failed;
    std::vector<std::string> eligible;
    for (const std::string& path : paths) {
        if (!IsRegularFile(path) || safety::ExplicitDeleteForbidden(path)) {
            failed.push_back(path);
        } else {
            eligible.push_back(path);
        }
    }
    for (size_t start = 0; start < eligible.size(); start += kChunkSize) {
        const size_t end = std::min(start + kChunkSize, eligible.size());
        const std::vector<std::string> chunk(eligible.begin() + start,
                                             eligible.begin() + end);
        const std::vector<std::string> chunkFailed = SendChunk(chunk);
        failed.insert(failed.end(), chunkFailed.begin(), chunkFailed.end());
    }
    return failed;
}

Json DeleteRecycle(const std::vector<std::string>& paths) {
    const std::string title = "大文件";
    Json report = Json{{"freed", 0}, {"deleted", 0}, {"failed", Json::array()},
                       {"reasons", Json::array()}};
    std::vector<std::string> failed;
    std::vector<std::string> reasons;
    try {
        task::Begin("big_delete", "大文件清理", true, true, 0, "个文件");
    } catch (const std::exception& error) {
        reasons.push_back(error.what());
        report["failed"] = failed;
        report["reasons"] = reasons;
        return report;
    }
    task::Guard guard("big_delete");
    task::SetUnit("big_delete", "个文件");
    task::SetTotal("big_delete", paths.size());
    task::SetStage("big_delete", "校验文件");

    bool cancelled = false;
    std::vector<std::string> eligible;
    std::unordered_map<std::string, uint64_t> sizes;
    for (const std::string& path : paths) {
        if (task::IsCancelled("big_delete")) {
            cancelled = true;
            break;
        }
        task::AddDone("big_delete", 1);
        if (!FileExists(path)) {
            failed.push_back(path);
            reasons.push_back(path + "：文件已不存在");
            continue;
        }
        if (safety::ExplicitDeleteForbidden(path)) {
            failed.push_back(path);
            reasons.push_back(path + "：受保护路径或密码库，已跳过");
            continue;
        }
        sizes[path] = FileSizeOf(path);
        eligible.push_back(path);
    }
    if (cancelled) {
        reasons.push_back("已取消");
        guard.Finish("cancelled", "已取消");
        report["failed"] = failed;
        report["reasons"] = reasons;
        return report;
    }

    task::SetStage("big_delete", "送入回收站");
    SendToRecycle(eligible);
    uint64_t deleted = 0;
    uint64_t freed = 0;
    for (const std::string& path : eligible) {
        if (FileExists(path)) {
            failed.push_back(path);
            reasons.push_back(path + "：送回收站失败（可能被占用）");
        } else {
            ++deleted;
            freed += sizes[path];
        }
    }
    guard.Finish("ok", "删除 " + std::to_string(deleted) + " 个，释放 " +
                           FormatMb(freed) + " MB");
    logs::WriteLog({"开始清理", "清理：" + title,
                    "完成：删除 " + std::to_string(deleted) + " 个，释放 " +
                        std::to_string(freed) + " 字节，失败 " +
                        std::to_string(failed.size()) + " 个"});
    report["freed"] = freed;
    report["deleted"] = deleted;
    report["failed"] = failed;
    report["reasons"] = reasons;
    return report;
}

}  // namespace recycle
