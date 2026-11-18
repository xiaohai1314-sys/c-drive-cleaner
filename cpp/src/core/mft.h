#pragma once

#include <cstdint>
#include <functional>
#include <optional>
#include <string>
#include <unordered_map>
#include <vector>

namespace mft {

constexpr uint32_t kFileAttributeReparsePoint = 0x0000'0400;
constexpr uint32_t kFileAttributeSparseFile = 0x0000'0200;
constexpr uint32_t kFileAttributeOffline = 0x0000'1000;
constexpr uint32_t kFileAttributeEncrypted = 0x0000'4000;
constexpr uint32_t kFileAttributeRecallOnOpen = 0x0004'0000;
constexpr uint32_t kFileAttributeRecallOnDataAccess = 0x0040'0000;

constexpr uint32_t kSkipFileAttrs =
    kFileAttributeReparsePoint | kFileAttributeOffline |
    kFileAttributeRecallOnOpen | kFileAttributeRecallOnDataAccess;

struct FileEntry {
    std::string path;
    uint64_t size = 0;
    uint64_t mtime = 0;
    uint64_t sortKey = 0;
    uint64_t frn = 0;
    uint64_t firstLcn = 0;
    uint32_t bytesPerCluster = 0;
    bool canRawRead = false;
};

using KeepFn = std::function<bool(const std::string& name, uint64_t size)>;

std::vector<FileEntry> ScanFiles(const std::string& root, uint64_t minSize,
                                 const std::vector<std::string>& skipTop);

/// keep(文件名, 大小) 返回 false 的条目在拼接完整路径之前就被丢弃。
std::vector<FileEntry> ScanFilesWhere(const std::string& root, uint64_t minSize,
                                      const std::vector<std::string>& skipTop,
                                      const KeepFn& keep);

/// limit 非 0 时，文件数超过上限就提前中止并把 overflow 置为 true。
std::vector<FileEntry> FallbackWalkFiles(
    const std::string& root, uint64_t minSize,
    const std::vector<std::string>& skipTop, size_t limit = 0,
    bool* overflow = nullptr);

// ---------- 快照（MFT3 二进制，与 Rust 版逐字节兼容） ----------

struct DirNode {
    uint16_t seq = 0;
    uint64_t parentFrn = 0;
    std::string name;
    bool reparse = false;
};

struct RawFile {
    uint64_t frn = 0;
    uint64_t parentFrn = 0;
    std::string name;
    uint64_t size = 0;
    uint64_t mtime = 0;
    uint64_t firstLcn = 0;
    uint64_t sortKey = 0;
    bool canRawRead = false;
};

struct Snapshot {
    uint64_t volumeSerial = 0;
    uint64_t journalId = 0;
    int64_t nextUsn = 0;
    uint64_t minSize = 0;
    uint32_t bytesPerCluster = 0;
    std::unordered_map<uint64_t, DirNode> dirs;
    std::unordered_map<uint64_t, RawFile> files;
};

bool SaveSnapshot(const std::string& path, const Snapshot& snapshot);

/// 等待后台快照写入/重建结束（调试 CLI 退出前用）。
void WaitForBackgroundWrites(int timeoutMs);
std::optional<Snapshot> LoadSnapshot(const std::string& path);

// 供单元测试使用的底层原语。
uint64_t FileTimeToUnix(uint64_t fileTime);
std::pair<uint64_t, uint64_t> ParseFirstExtent(const uint8_t* data,
                                               size_t length);

}  // namespace mft
