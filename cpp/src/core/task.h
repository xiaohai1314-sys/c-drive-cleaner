#pragma once

#include <cstdint>
#include <string>

#include "util/json.h"

struct TaskStatus {
    std::string id;
    std::string title;
    std::string stage;
    std::string unit;
    std::string outcome;
    std::string summary;
    uint64_t done = 0;
    uint64_t total = 0;
    uint64_t started_ms = 0;
    uint64_t finished_ms = 0;
    bool active = false;
    bool cancellable = false;
    bool cancelling = false;
    bool mutating = false;
};

namespace task {

void Begin(const std::string& id, const std::string& title, bool mutating,
           bool cancellable, uint64_t total, const std::string& unit);
void SetStage(const std::string& id, const std::string& stage);
void SetProgress(const std::string& id, uint64_t done, uint64_t total);
void SetTotal(const std::string& id, uint64_t total);
void AddDone(const std::string& id, uint64_t amount);
void SetUnit(const std::string& id, const std::string& unit);
void Finish(const std::string& id, const std::string& outcome,
            const std::string& summary);
bool Cancel(const std::string& id);
void CancelAll();
bool IsCancelled(const std::string& id);
Json StatusJson();

/// 与 Rust 的 TaskGuard 对齐：析构时若没有显式收尾，按「任务中断」标记失败，
/// 避免异常路径把 mutating 任务永久留在 active 状态。
class Guard {
public:
    explicit Guard(std::string id);
    ~Guard();
    Guard(const Guard&) = delete;
    Guard& operator=(const Guard&) = delete;

    void Finish(const std::string& outcome, const std::string& summary);

private:
    std::string id_;
    bool finished_ = false;
};

}  // namespace task
