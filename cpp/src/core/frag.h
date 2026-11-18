#pragma once

#include <cstdint>
#include <optional>
#include <string>

#include "util/json.h"

/// 磁盘碎片：调用系统 defrag.exe 分析/优化，并解析它的中英文控制台输出。
namespace frag {

struct ParsedReport {
    std::optional<double> percent;
    std::optional<double> spaceGb;
    std::optional<uint64_t> count;
    std::optional<double> freeGb;
};

/// 解析 defrag 的分析输出（供测试与命令共用）。
ParsedReport ParseReport(std::string text);

/// 命令 `frag_analyze`：返回 {drive, percent, space_gb, count, free_gb, ssd}。
Json Analyze(const std::string& drive);

/// 命令 `frag_optimize`：失败时抛 runtime_error（消息与 Rust 版相同）。
void Optimize(const std::string& drive);

}  // namespace frag
