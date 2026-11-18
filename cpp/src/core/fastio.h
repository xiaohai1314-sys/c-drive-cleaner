#pragma once

#include <windows.h>

#include <atomic>
#include <cstdint>
#include <functional>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "core/digest.h"
#include "util/win32.h"

namespace fastio {

struct OpenedFile {
    UniqueHandle handle;
    std::optional<std::pair<uint32_t, uint64_t>> fileId;
    uint64_t size = 0;
};

/// 只读卷根（`X:\`）的目录句柄，用作 OpenFileById 的卷提示。
UniqueHandle OpenDriveHint(char driveLetter);

/// 按 FRN 优先 OpenFileById，失败再退回路径打开；并检查跳过属性、硬链接标识。
std::optional<OpenedFile> OpenCheckedFileFast(
    HANDLE hint, uint64_t frn, const std::string& path, DWORD flags);

/// 仅查询文件元数据标识（不读内容），用于热缓存路径兜底识别硬链接。
std::optional<std::pair<uint32_t, uint64_t>> QueryFileIdentity(
    const std::string& path);

struct RawHeadTarget {
    size_t idx = 0;
    uint64_t offset = 0;
    size_t want = 0;
};

/// 按物理 LCN 合并读取同一卷上邻近文件的 4KB 头部（直接读 `\\.\X:`）。
std::vector<std::pair<size_t, Digest>> CoalescedRawVolumeHeads(
    char driveLetter, std::vector<RawHeadTarget> targets, bool isSsd,
    const std::atomic<bool>& cancel, const std::function<void()>& onTick);

struct IocpHeadRequest {
    size_t idx = 0;
    std::string path;
    uint64_t frn = 0;
    size_t want = 0;
};

struct IocpHeadResult {
    size_t idx = 0;
    Digest digest{};
    std::optional<std::pair<uint32_t, uint64_t>> fileId;
};

/// OpenFileById + IOCP 异步重叠 I/O 批量读取文件前 4KB（SSD 32 路、HDD 8 路）。
std::vector<IocpHeadResult> IocpReadHeads(
    std::vector<IocpHeadRequest> requests, size_t concurrency,
    const std::atomic<bool>& cancel, const std::function<void()>& onTick);

}  // namespace fastio
