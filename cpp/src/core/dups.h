#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include "util/json.h"

/// 重复文件专清：`scan_dups` 与 `delete_dups`，分组/哈希流程与 Rust 版一致。
namespace dups {

/// root 有值时只扫这个目录；否则用 drives（为空表示全部固定盘）。
Json ScanDups(const std::optional<std::string>& root,
              const std::vector<std::string>& drives,
              std::optional<uint64_t> minSize, bool fast);

/// items: [{path, hash, size, mtime, keeper?}]，删除前重新核对大小/时间/内容。
Json DeleteDups(const Json& items);

/// `cancel_scan` 对 dup 生效时同步置位，与 Rust 的 DUP_CANCEL 一致。
void CancelScan();

/// 删除成功后清掉 dups-*.json，避免 5 分钟内再次扫描还显示已删掉的组。
void InvalidateCache();

}  // namespace dups
