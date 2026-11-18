#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include "util/json.h"

/// 文件粉碎：安全随机数覆盖写入后按句柄删除，不走回收站。
namespace shred {

/// 命令 `shred`：passes 缺省 1，范围 1–3。
Json Shred(const std::vector<std::string>& paths,
           std::optional<uint32_t> passes);

/// 不含任务与历史日志副作用（供测试使用）。
Json ShredPaths(const std::vector<std::string>& paths, uint32_t passes);

}  // namespace shred
