#pragma once

#include <string>
#include <vector>

#include "util/json.h"

/// 隐私痕迹：浏览器历史/Cookie/下载、最近文件、注册表使用痕迹。
namespace privacy {

/// 某类痕迹对应的 SQL 语句（history/cookies/downloads/shop/money/medical）。
std::vector<std::string> StatementsFor(const std::string& kind);

/// 删除注册表值之前先导出 `.reg` 备份到 logs\backups\reg。
bool BackupRegistryKey(const std::string& hive, const std::string& path,
                       const std::string& label);

/// 命令 `privacy_scan`：返回 PrivacyItem 数组。
Json Scan();

/// 命令 `privacy_clean`：返回 {files, sql, registry, failed}。
Json Clean(const std::vector<std::string>& ids);

}  // namespace privacy
