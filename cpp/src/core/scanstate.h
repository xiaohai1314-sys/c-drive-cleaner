#pragma once

#include <cstdint>
#include <string>

#include "util/json.h"

namespace scanstate {

/// 与 Rust 的 ScanStatus 一致：前端按 kind 轮询进度与取消状态。
struct Status {
    bool active = false;
    std::string stage;
    bool cancelled = false;
    uint64_t done = 0;
    uint64_t total = 0;
    uint64_t found = 0;
};

void Begin(const std::string& kind, const std::string& stage);
void End(const std::string& kind);
bool WasCancelled(const std::string& kind);
/// kind 为空表示取消所有正在跑的扫描。
void Cancel(const std::string& kind);
void Stage(const std::string& kind, const std::string& stage);
void Progress(const std::string& kind, uint64_t done, uint64_t total);
void SetFound(const std::string& kind, uint64_t found);
/// 重复文件扫描用：重置计数，随后各阶段累加总量与完成量。
void Reset(const std::string& kind, uint64_t total);
void AddTotal(const std::string& kind, uint64_t delta);
void Advance(const std::string& kind, uint64_t delta = 1);
void AddFound(const std::string& kind, uint64_t delta = 1);
Json StatusJson();

}  // namespace scanstate
