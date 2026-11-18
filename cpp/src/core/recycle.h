#pragma once

#include <string>
#include <vector>

#include "util/json.h"

namespace recycle {

/// 把文件送进回收站（FO_DELETE + FOF_ALLOWUNDO），返回没能送进去的路径。
/// 失败时不退回到永久删除。
std::vector<std::string> SendToRecycle(const std::vector<std::string>& paths);

/// 命令 `delete_recycle`：校验后送回收站，返回 DeleteReport。
Json DeleteRecycle(const std::vector<std::string>& paths);

}  // namespace recycle
