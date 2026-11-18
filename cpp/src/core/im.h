#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "util/json.h"

/// 微信 / QQ 专清：只识别媒体、缓存与日志，聊天数据库和配置一律跳过。
namespace im {

struct Record {
    std::string path;
    uint64_t size = 0;
    uint64_t mtime = 0;
    std::string kind;
    std::string sub;
    std::string month;
    std::string name;
};

/// 当前存在的微信/QQ 数据根目录。
std::vector<std::string> Roots(const std::string& kind);

/// 搬家恢复用的白名单候选（不要求目录存在）。
std::vector<std::string> MigrationRoots(const std::string& kind);

/// 位于 AppData 下的根目录是客户端程序数据。
bool IsProgramRoot(const std::string& root);

/// 按「目录优先、扩展名兜底」判定类别；不在清理范围时返回 nullopt。
std::optional<std::pair<std::string, std::string>> Classify(
    const std::string& path, const std::string& root);

std::vector<Record> ListMedia(const std::vector<std::string>& roots);
Json RecordsToJson(const std::vector<Record>& records);

/// 聊天图片缩略图（微信 .dat 按 XOR 0x05 解密）；失败返回 nullopt。
std::optional<std::pair<std::string, std::vector<uint8_t>>> Thumbnail(
    const std::string& path);

/// 文件预览：文本直出，二进制给十六进制；失败抛 runtime_error。
Json Preview(const std::string& path);

Json Clean(const std::string& kind, const std::vector<std::string>& kinds,
           const std::optional<std::vector<std::string>>& paths);

void RememberScanned(const std::vector<Record>& records);
bool IsScanned(const std::string& path);

}  // namespace im
