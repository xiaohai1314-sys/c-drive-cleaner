#pragma once

#include <atomic>
#include <cstdint>
#include <optional>
#include <string>
#include <utility>

#include "core/digest.h"

/// 重复文件流水线用到的几种分块哈希（与 Rust 版 main.rs 中的实现逐字节一致）。
namespace filehash {

using FileId = std::pair<uint32_t, uint64_t>;

struct Hashed {
    Digest digest{};
    std::optional<FileId> fileId;
};

/// 头、中、尾各取 16KB（≤48KB 直接算全文）。
std::optional<Hashed> SampleHash(const std::string& path, uint64_t frn,
                                 uint64_t size);

/// 头、中、尾各取 256KB（≤768KB 直接算全文）。
std::optional<Hashed> StrongHash(const std::string& path, uint64_t frn,
                                 uint64_t size);

/// 25% 与 75% 处各取 64KB，用于大文件提前淘汰伪碰撞。
std::optional<Hashed> ProbeHash(const std::string& path, uint64_t frn,
                                uint64_t size);

/// 全文件 BLAKE3；≥16MB 时读盘与计算双缓冲并行。
std::optional<Hashed> FullHash(const std::string& path, uint64_t frn,
                               const std::atomic<bool>& cancel);

struct SeqSampleFull {
    Digest sample{};
    Digest full{};
    std::optional<FileId> fileId;
};

/// 机械盘专用（≤2MB）：一次顺序读全文，同时算出采样哈希与完整哈希。
std::optional<SeqSampleFull> ReadHddSeqSampleAndFull(const std::string& path,
                                                     uint64_t frn,
                                                     uint64_t size,
                                                     uint64_t chunkLen);

}  // namespace filehash
