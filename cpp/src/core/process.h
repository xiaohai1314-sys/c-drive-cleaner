#pragma once

#include <cstddef>
#include <string>
#include <unordered_set>
#include <vector>

/// 进程检测与管理：清理前确认微信/QQ/浏览器等是否在运行，避免文件被占用。
namespace process {

/// 当前运行的进程名（小写，如 "wechat.exe"）。
std::unordered_set<std::string> RunningProcesses();

/// 返回 targets 中正在运行的项（同样是小写形式）。
std::vector<std::string> CheckRunning(const std::vector<std::string>& targets);

/// 按可执行文件名结束进程，返回结束的数量。
size_t KillProcessByName(const std::string& targetName);

}  // namespace process
