#pragma once

#include <cstdint>
#include <string>
#include <utility>
#include <vector>

#include "util/json.h"

/// 系统级深度瘦身与维护：更新缓存、DISM、休眠文件、回收站，以及微信/QQ 搬家。
namespace sysclean {

Json ScanSystem();

/// 返回清理结果文案；失败抛 runtime_error（与 Rust 的 Err(String) 对应）。
std::string CleanSystemItem(const std::string& key);

Json QueryHiber();
std::string SetHiber(const std::string& mode);
void EmptyRecycleBins();

Json AnalyzeImMigration(const std::string& kind);
std::string ExecuteImMigration(const std::string& source,
                               const std::string& targetDir,
                               const std::string& kind);

/// 启动时调用：上次搬家在「原目录改名」之后中断时把数据改回原名。
void RecoverPendingMove();

// ---------- 搬家用到的底层原语 ----------

/// 目录总大小与文件数（不跟随重解析点）。
std::pair<uint64_t, uint64_t> DirectorySizeCount(const std::string& dir);

/// 清空目录内容但保留目录本身，返回 (释放字节, 删除个数)。
std::pair<uint64_t, uint64_t> CleanDirectoryContents(const std::string& dir);

/// 恢复日志里的源/备份路径是否合法：精确匹配白名单且为同级 `_backup_时间戳`。
bool ValidRecoveryPaths(const std::string& source, const std::string& backup,
                        const std::vector<std::string>& allowedRoots);

/// 传给 `cmd /C` 的引号路径是否安全（拒绝引号、`%` 与控制字符）。
bool SafeForQuotedCmdPath(const std::string& path);

/// 复制目录；失败时 error 给出原因。
bool CopyDirectory(const std::string& source, const std::string& target,
                   std::string& error);

/// 逐文件校验源与目标内容一致。
bool VerifyDirectoryCopy(const std::string& source, const std::string& target,
                         std::string& detail);

}  // namespace sysclean
