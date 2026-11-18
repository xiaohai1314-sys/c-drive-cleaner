#pragma once

#include <array>
#include <cstddef>
#include <cstdint>

/// BLAKE3 32 字节摘要，与 Rust 的 `hashcache::Digest` 相同。
using Digest = std::array<uint8_t, 32>;

Digest Blake3Hash(const void* data, size_t length);
