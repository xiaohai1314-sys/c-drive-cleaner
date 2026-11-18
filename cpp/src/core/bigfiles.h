#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "util/json.h"

/// 大文件按类型分档阈值（字节），与 Rust 版 `big_file_threshold_bytes` 一致。
uint64_t BigFileThresholdBytes(const std::string& path);

/// pagefile.sys / hiberfil.sys 等系统文件，扫描结果一律排除。
bool IsSystemFileName(const std::string& path);

/// 命令 `scan_big`：drives 为空时扫描全部固定盘。
Json ScanBig(const std::vector<std::string>& drives);
