#pragma once

#include <string>
#include <string_view>

std::wstring Utf8ToWide(std::string_view text);
std::string WideToUtf8(std::wstring_view text);

/// 严格校验：不合法即返回 false（对应 Rust 的 `str::from_utf8`）。
bool IsValidUtf8(const std::string& text);
