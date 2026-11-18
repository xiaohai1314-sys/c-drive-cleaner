#pragma once

#include <cstdint>
#include <string>
#include <string_view>

// 与 Rust 侧 safety::normalize 一致：盘符大写、反斜杠、折叠重复分隔符、去掉末尾分隔符。
std::string NormalizePath(std::string path);
std::string Basename(std::string_view path);
bool IsDriveRoot(std::string_view path);
std::string JoinPath(std::string_view base, std::string_view child);
bool PathExists(const std::string& utf8Path);

std::string LocalAppDataDir();
std::string ToLongPath(const std::wstring& path);

/// `%LOCALAPPDATA%\C盘清理工具\mftcache`，所有扫描缓存都放在这里（与 Rust 版相同）。
std::string CacheDir();

/// 最后写入时间（Unix 秒）；读不到返回 0。
uint64_t FileMtimeSeconds(const std::string& utf8Path);
