#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include "util/json.h"

/// 重复文件专清：`scan_dups` 与 `delete_dups`，分组/哈希流程与 Rust 版一致。
namespace dups {

/// root 有值时只扫这个目录；否则用 drives（为空表示全部固定盘）。
/// fast=true 用 256KB×3 强采样；turbo=true 只比头 4KB（最快，误判组在删除前会被完整核对拦下）。
Json ScanDups(const std::optional<std::string>& root,
              const std::vector<std::string>& drives,
              std::optional<uint64_t> minSize, bool fast, bool turbo = false);

/// items: [{path, hash, size, mtime, keeper?}]，删除前重新核对大小/时间/内容。
Json DeleteDups(const Json& items);

/// 上次扫描的结果（直接读 dups-*.json，不看新鲜度）；没有缓存时 groups 为空。
Json CachedResult(const std::optional<std::string>& root,
                  const std::vector<std::string>& drives,
                  std::optional<uint64_t> minSize, bool fast, bool turbo);

/// `cancel_scan` 对 dup 生效时同步置位，与 Rust 的 DUP_CANCEL 一致。
void CancelScan();

/// 删除成功后清掉 dups-*.json，避免 5 分钟内再次扫描还显示已删掉的组。
void InvalidateCache();

}  // namespace dups
