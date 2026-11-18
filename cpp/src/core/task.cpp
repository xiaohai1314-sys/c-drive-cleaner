#include "core/task.h"

#include <atomic>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <unordered_map>
#include <utility>

#include "core/timeutil.h"

namespace {

struct Task {
    std::string id;
    std::string title;
    std::string stage;
    std::string unit;
    std::string outcome = "running";
    std::string summary;
    std::atomic<uint64_t> done{0};
    std::atomic<uint64_t> total{0};
    std::atomic<bool> cancel{false};
    uint64_t started_ms = 0;
    uint64_t finished_ms = 0;
    bool active = true;
    bool cancellable = false;
    bool cancelling = false;
    bool mutating = false;
};

std::mutex g_mutex;
std::unordered_map<std::string, std::shared_ptr<Task>> g_tasks;

std::shared_ptr<Task> FindLocked(const std::string& id) {
    const auto it = g_tasks.find(id);
    return it == g_tasks.end() ? nullptr : it->second;
}

}  // namespace

namespace task {

void Begin(const std::string& id, const std::string& title, bool mutating,
           bool cancellable, uint64_t total, const std::string& unit) {
    std::lock_guard<std::mutex> lock(g_mutex);
#ifndef CDC_TEST
    if (mutating) {
        for (const auto& [key, existing] : g_tasks) {
            (void)key;
            if (existing->active && existing->mutating) {
                throw std::runtime_error("已有清理任务正在进行，请等它结束后再试");
            }
        }
    }
#endif
    auto created = std::make_shared<Task>();
    created->id = id;
    created->title = title;
    created->unit = unit;
    created->total.store(total);
    created->started_ms = NowMs();
    created->cancellable = cancellable;
    created->mutating = mutating;
    g_tasks[id] = std::move(created);
}

void SetStage(const std::string& id, const std::string& stage) {
    std::lock_guard<std::mutex> lock(g_mutex);
    if (const auto found = FindLocked(id)) {
        found->stage = stage;
    }
}

void SetProgress(const std::string& id, uint64_t done, uint64_t total) {
    std::lock_guard<std::mutex> lock(g_mutex);
    if (const auto found = FindLocked(id)) {
        found->done.store(done);
        found->total.store(total);
    }
}

void SetTotal(const std::string& id, uint64_t total) {
    std::lock_guard<std::mutex> lock(g_mutex);
    if (const auto found = FindLocked(id)) {
        found->total.store(total);
    }
}

void AddDone(const std::string& id, uint64_t amount) {
    std::lock_guard<std::mutex> lock(g_mutex);
    if (const auto found = FindLocked(id)) {
        found->done.fetch_add(amount);
    }
}

void SetUnit(const std::string& id, const std::string& unit) {
    std::lock_guard<std::mutex> lock(g_mutex);
    if (const auto found = FindLocked(id)) {
        found->unit = unit;
    }
}

void Finish(const std::string& id, const std::string& outcome,
            const std::string& summary) {
    std::lock_guard<std::mutex> lock(g_mutex);
    if (const auto found = FindLocked(id)) {
        found->active = false;
        found->outcome = outcome;
        found->summary = summary;
        found->finished_ms = NowMs();
    }
}

bool Cancel(const std::string& id) {
    std::lock_guard<std::mutex> lock(g_mutex);
    const auto found = FindLocked(id);
    if (!found || !found->active || !found->cancellable) {
        return false;
    }
    found->cancel.store(true);
    found->cancelling = true;
    return true;
}

void CancelAll() {
    std::lock_guard<std::mutex> lock(g_mutex);
    for (auto& [key, found] : g_tasks) {
        (void)key;
        if (found->active && found->cancellable) {
            found->cancel.store(true);
            found->cancelling = true;
        }
    }
}

bool IsCancelled(const std::string& id) {
    std::lock_guard<std::mutex> lock(g_mutex);
    const auto found = FindLocked(id);
    return found != nullptr && found->cancel.load();
}

Json StatusJson() {
    std::lock_guard<std::mutex> lock(g_mutex);
    const uint64_t now = NowMs();
    for (auto it = g_tasks.begin(); it != g_tasks.end();) {
        const auto& found = it->second;
        if (!found->active && now > found->finished_ms + 30000ULL) {
            it = g_tasks.erase(it);
        } else {
            ++it;
        }
    }
    Json result = Json::object();
    for (const auto& [id, found] : g_tasks) {
        Json item;
        item["id"] = found->id;
        item["title"] = found->title;
        item["stage"] = found->stage;
        item["done"] = found->done.load();
        item["total"] = found->total.load();
        item["unit"] = found->unit;
        item["active"] = found->active;
        item["cancellable"] = found->cancellable;
        item["cancelling"] = found->cancelling;
        item["outcome"] = found->outcome;
        item["summary"] = found->summary;
        item["started_ms"] = found->started_ms;
        item["finished_ms"] = found->finished_ms;
        item["mutating"] = found->mutating;
        result[id] = std::move(item);
    }
    return result;
}

Guard::Guard(std::string id) : id_(std::move(id)) {}

Guard::~Guard() {
    if (!finished_) {
        Finish("failed", "任务中断");
    }
}

void Guard::Finish(const std::string& outcome, const std::string& summary) {
    task::Finish(id_, outcome, summary);
    finished_ = true;
}

}  // namespace task
