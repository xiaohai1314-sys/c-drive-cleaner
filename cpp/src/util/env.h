#pragma once

#include <string>

std::string GetEnv(const std::string& name);
// 与 Rust 侧 expand_env 一致：%NAME% 存在则替换，否则原样保留（包括 %）。
std::string ExpandEnv(const std::string& input);
