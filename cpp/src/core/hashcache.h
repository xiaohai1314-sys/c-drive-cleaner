#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <vector>

#include "core/digest.h"

/// 重复文件哈希缓存：SQLite 按「路径 + 大小 + 修改时间」增量持久化，
/// 表结构与 Rust 版 `hash-cache-v3.db` 逐字节兼容。
namespace hashcache {

struct Entry {
    uint64_t size = 0;
    uint64_t mtime = 0;
    std::optional<Digest> head;
    std::optional<Digest> sample;
    std::optional<Digest> strong;
    std::optional<Digest> full;
    uint64_t lastSeen = 0;
};

std::string DigestToHex(const Digest& digest);
std::optional<Digest> HexToDigest(const std::string& hex);

std::string DefaultDbPath();

/// 调试/验收用：导出全部记录，每行 `path\tsize\tmtime\thead\tsample\tstrong\tfull\tlast_seen`。
std::vector<std::string> DumpAll(const std::string& dbPath);

class HashCache {
public:
    static HashCache LoadForPaths(const std::vector<std::string>& paths);
    static HashCache LoadForPathsAt(const std::string& dbPath,
                                    const std::vector<std::string>& paths,
                                    bool migrateLegacy);

    /// 只有大小与修改时间都对得上才返回，否则视为未命中。
    const Entry* EntryFor(const std::string& path, uint64_t size,
                          uint64_t mtime) const;

    void RememberDigests(const std::string& path, uint64_t size, uint64_t mtime,
                         std::optional<Digest> head,
                         std::optional<Digest> sample,
                         std::optional<Digest> strong,
                         std::optional<Digest> full, uint64_t now);

    /// 只写本次新增或变化的记录；同时清掉 30 天没见过的行。
    void SaveDirty();

    size_t Size() const { return entries_.size(); }
    bool Dirty() const { return !dirty_.empty(); }

private:
    std::unordered_map<std::string, Entry> entries_;
    std::unordered_set<std::string> dirty_;
    std::string dbPath_;
};

}  // namespace hashcache
