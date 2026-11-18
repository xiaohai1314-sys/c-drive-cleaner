#include "core/scanstate.h"

#include <mutex>
#include <unordered_map>

namespace {

std::mutex g_mutex;
std::unordered_map<std::string, scanstate::Status> g_status;

}  // namespace

namespace scanstate {

void Begin(const std::string& kind, const std::string& stage) {
    std::lock_guard<std::mutex> lock(g_mutex);
    Status status;
    status.active = true;
    status.stage = stage;
    g_status[kind] = std::move(status);
}

void End(const std::string& kind) {
    std::lock_guard<std::mutex> lock(g_mutex);
    const auto found = g_status.find(kind);
    if (found != g_status.end()) {
        found->second.active = false;
    }
}

bool WasCancelled(const std::string& kind) {
    std::lock_guard<std::mutex> lock(g_mutex);
    const auto found = g_status.find(kind);
    return found != g_status.end() && found->second.cancelled;
}

void Cancel(const std::string& kind) {
    std::lock_guard<std::mutex> lock(g_mutex);
    if (kind.empty()) {
        for (auto& [key, status] : g_status) {
            (void)key;
            if (status.active) {
                status.cancelled = true;
            }
        }
        return;
    }
    const auto found = g_status.find(kind);
    if (found != g_status.end() && found->second.active) {
        found->second.cancelled = true;
    }
}

void Stage(const std::string& kind, const std::string& stage) {
    std::lock_guard<std::mutex> lock(g_mutex);
    const auto found = g_status.find(kind);
    if (found != g_status.end()) {
        found->second.stage = stage;
    }
}

void Progress(const std::string& kind, uint64_t done, uint64_t total) {
    std::lock_guard<std::mutex> lock(g_mutex);
    const auto found = g_status.find(kind);
    if (found != g_status.end()) {
        found->second.done = done;
        found->second.total = total;
    }
}

void SetFound(const std::string& kind, uint64_t found_count) {
    std::lock_guard<std::mutex> lock(g_mutex);
    const auto found = g_status.find(kind);
    if (found != g_status.end()) {
        found->second.found = found_count;
    }
}

void Reset(const std::string& kind, uint64_t total) {
    std::lock_guard<std::mutex> lock(g_mutex);
    const auto found = g_status.find(kind);
    if (found != g_status.end()) {
        found->second.done = 0;
        found->second.total = total;
        found->second.found = 0;
    }
}

void AddTotal(const std::string& kind, uint64_t delta) {
    std::lock_guard<std::mutex> lock(g_mutex);
    const auto found = g_status.find(kind);
    if (found != g_status.end()) {
        found->second.total += delta;
    }
}

void Advance(const std::string& kind, uint64_t delta) {
    std::lock_guard<std::mutex> lock(g_mutex);
    const auto found = g_status.find(kind);
    if (found != g_status.end()) {
        found->second.done += delta;
    }
}

void AddFound(const std::string& kind, uint64_t delta) {
    std::lock_guard<std::mutex> lock(g_mutex);
    const auto found = g_status.find(kind);
    if (found != g_status.end()) {
        found->second.found += delta;
    }
}

Json StatusJson() {
    std::lock_guard<std::mutex> lock(g_mutex);
    Json result = Json::object();
    for (const auto& [kind, status] : g_status) {
        Json item;
        item["active"] = status.active;
        item["stage"] = status.stage;
        item["cancelled"] = status.cancelled;
        item["done"] = status.done;
        item["total"] = status.total;
        item["found"] = status.found;
        result[kind] = std::move(item);
    }
    return result;
}

}  // namespace scanstate
