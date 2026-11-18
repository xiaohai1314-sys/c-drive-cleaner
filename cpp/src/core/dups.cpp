#include "core/dups.h"

#include <windows.h>

#include <algorithm>
#include <atomic>
#include <cctype>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <iterator>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <type_traits>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <vector>

#include "core/bigfiles.h"
#include "core/digest.h"
#include "core/disk.h"
#include "core/fastio.h"
#include "core/filehash.h"
#include "core/hashcache.h"
#include "core/logs.h"
#include "core/mft.h"
#include "core/recycle.h"
#include "core/safety.h"
#include "core/scanstate.h"
#include "core/task.h"
#include "core/timeutil.h"
#include "util/env.h"
#include "util/path.h"
#include "util/utf.h"

namespace {

constexpr uint64_t kHeadChunk = 4096;
constexpr uint64_t kSampleChunk = 16 * 1024;
constexpr uint64_t kStrongChunk = 256 * 1024;
constexpr uint64_t kProbeChunk = 64 * 1024;
constexpr uint64_t kProbeThreshold = 16 * 1024 * 1024;
constexpr uint64_t kHddSeqThreshold = 2 * 1024 * 1024;
constexpr uint64_t kDefaultMinSize = 1 << 20;
constexpr size_t kSsdThreads = 8;
constexpr uint64_t kCacheFreshSeconds = 300;

const char* const kTopSkip[] = {
    "$recycle.bin",           "system volume information",
    "windows",                "program files",
    "program files (x86)",    "programdata",
    "recovery",               "perflogs",
    "msocache",               "config.msi",
    "$windows.~bt",           "$windows.~ws",
    "windows.old",
};

std::vector<std::string> SkipTop() {
    return std::vector<std::string>(std::begin(kTopSkip), std::end(kTopSkip));
}

std::string LowerAscii(std::string text) {
    for (char& ch : text) {
        ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    }
    return text;
}

bool EqualsIgnoreCase(const std::string& left, const std::string& right) {
    return LowerAscii(left) == LowerAscii(right);
}

std::string FormatMb(uint64_t bytes) {
    char buffer[64];
    std::snprintf(buffer, sizeof(buffer), "%.1f",
                  static_cast<double>(bytes) / 1048576.0);
    return buffer;
}

std::atomic<bool> gDupCancel{false};

bool DupCancelled() {
    return gDupCancel.load() || scanstate::WasCancelled("dup");
}

void Tick() { scanstate::Advance("dup", 1); }
void Found() { scanstate::AddFound("dup", 1); }

template <typename F>
void ParallelForEachIndex(size_t count, size_t threads, F body) {
    if (count == 0) {
        return;
    }
    threads = std::min(threads, count);
    if (threads <= 1) {
        for (size_t index = 0; index < count; ++index) {
            body(index);
        }
        return;
    }
    std::atomic<size_t> next{0};
    std::vector<std::thread> pool;
    pool.reserve(threads);
    for (size_t slot = 0; slot < threads; ++slot) {
        pool.emplace_back([&] {
            while (true) {
                const size_t index = next.fetch_add(1);
                if (index >= count) {
                    break;
                }
                body(index);
            }
        });
    }
    for (std::thread& thread : pool) {
        thread.join();
    }
}

/// 按物理磁盘调度的任务执行器：SSD 走 8 路并行池，每块物理机械盘独立排队
/// （盘内按 sort_key 升序，hddWorkers 控制浅队列并发度）。
template <typename T, typename PathOf, typename SortKeyOf, typename Worker>
auto RunDiskAwareNcq(std::vector<T> items, PathOf pathOf, SortKeyOf sortKeyOf,
                     size_t hddWorkers, Worker worker)
    -> std::vector<std::invoke_result_t<Worker&, T>> {
    using R = std::invoke_result_t<Worker&, T>;
    std::vector<R> results;
    if (items.empty()) {
        return results;
    }

    std::unordered_map<char, disk::DiskKey> cache;
    std::vector<T> ssd;
    std::unordered_map<disk::DiskKey, std::vector<T>, disk::DiskKeyHash> hdd;
    for (T& item : items) {
        const disk::DiskKey key = disk::KeyForPath(pathOf(item), cache);
        if (key.kind == disk::DiskKeyKind::Ssd) {
            ssd.push_back(std::move(item));
        } else {
            hdd[key].push_back(std::move(item));
        }
    }

    if (hdd.empty()) {
        std::vector<R> out(ssd.size());
        ParallelForEachIndex(ssd.size(), kSsdThreads, [&](size_t index) {
            out[index] = worker(std::move(ssd[index]));
        });
        return out;
    }

    std::vector<std::vector<T>> queues;
    queues.reserve(hdd.size());
    for (auto& entry : hdd) {
        queues.push_back(std::move(entry.second));
    }
    for (std::vector<T>& queue : queues) {
        std::stable_sort(queue.begin(), queue.end(),
                         [&](const T& left, const T& right) {
                             const uint64_t leftKey = sortKeyOf(left);
                             const uint64_t rightKey = sortKeyOf(right);
                             if (leftKey != rightKey) {
                                 return leftKey < rightKey;
                             }
                             return pathOf(left) < pathOf(right);
                         });
    }

    const size_t workersPerHdd = std::clamp<size_t>(hddWorkers, 1, 4);
    std::vector<std::vector<R>> parts(queues.size());
    std::mutex partsMutex;
    std::vector<std::thread> threads;

    for (size_t slot = 0; slot < queues.size(); ++slot) {
        std::vector<T>& queue = queues[slot];
        if (workersPerHdd == 1 || queue.size() <= 2) {
            threads.emplace_back([&worker, &queue, &parts, slot] {
                std::vector<R>& part = parts[slot];
                part.reserve(queue.size());
                for (T& item : queue) {
                    part.push_back(worker(std::move(item)));
                }
            });
        } else {
            auto shared = std::make_shared<std::vector<T>>(std::move(queue));
            auto next = std::make_shared<std::atomic<size_t>>(0);
            for (size_t workerIndex = 0; workerIndex < workersPerHdd;
                 ++workerIndex) {
                threads.emplace_back([&worker, shared, next, &parts,
                                      &partsMutex, slot] {
                    std::vector<R> local;
                    while (true) {
                        const size_t index = next->fetch_add(1);
                        if (index >= shared->size()) {
                            break;
                        }
                        local.push_back(worker(std::move((*shared)[index])));
                    }
                    std::lock_guard<std::mutex> lock(partsMutex);
                    parts[slot].insert(parts[slot].end(),
                                       std::make_move_iterator(local.begin()),
                                       std::make_move_iterator(local.end()));
                });
            }
        }
    }

    std::vector<R> ssdResults(ssd.size());
    std::thread ssdThread;
    if (!ssd.empty()) {
        ssdThread = std::thread([&] {
            ParallelForEachIndex(ssd.size(), kSsdThreads, [&](size_t index) {
                ssdResults[index] = worker(std::move(ssd[index]));
            });
        });
    }

    for (std::thread& thread : threads) {
        thread.join();
    }
    if (ssdThread.joinable()) {
        ssdThread.join();
    }

    results.reserve(ssdResults.size());
    for (R& item : ssdResults) {
        results.push_back(std::move(item));
    }
    for (std::vector<R>& part : parts) {
        for (R& item : part) {
            results.push_back(std::move(item));
        }
    }
    return results;
}

/// 按物理磁盘用独立线程并发枚举（固态盘每个分区一个线程，同盘机械盘共用串行线程）。
std::vector<std::vector<mft::FileEntry>> ScanDrivesPerPhysicalDisk(
    const std::vector<std::string>& roots, uint64_t minSize) {
    std::unordered_map<char, disk::DiskKey> cache;
    std::vector<std::string> ssd;
    std::unordered_map<disk::DiskKey, std::vector<std::string>, disk::DiskKeyHash>
        hdd;
    for (const std::string& root : roots) {
        const disk::DiskKey key = disk::KeyForPath(root, cache);
        if (key.kind == disk::DiskKeyKind::Ssd) {
            ssd.push_back(root);
        } else {
            hdd[key].push_back(root);
        }
    }

    std::atomic<uint64_t> enumerated{0};
    const uint64_t total = roots.size();
    auto worker = [&](const std::string& root) -> std::vector<mft::FileEntry> {
        if (gDupCancel.load() || scanstate::WasCancelled("dup")) {
            return {};
        }
        std::vector<mft::FileEntry> files =
            mft::ScanFiles(root, minSize, SkipTop());
        const uint64_t done = enumerated.fetch_add(1) + 1;
        std::string label = root;
        while (!label.empty() && label.back() == '\\') {
            label.pop_back();
        }
        scanstate::Stage("dup", "枚举文件 " + std::to_string(done) + "/" +
                                    std::to_string(total) + "（" + label +
                                    "）");
        return files;
    };

    std::vector<std::vector<mft::FileEntry>> parts(ssd.size() + hdd.size());
    std::vector<std::thread> threads;
    size_t slot = 0;
    for (const std::string& root : ssd) {
        const size_t index = slot++;
        threads.emplace_back([&worker, &parts, index, root] {
            parts[index] = worker(root);
        });
    }
    for (auto& entry : hdd) {
        const size_t index = slot++;
        threads.emplace_back([&worker, &parts, index, queue = entry.second] {
            for (const std::string& root : queue) {
                std::vector<mft::FileEntry> found = worker(root);
                parts[index].insert(parts[index].end(),
                                    std::make_move_iterator(found.begin()),
                                    std::make_move_iterator(found.end()));
            }
        });
    }
    for (std::thread& thread : threads) {
        thread.join();
    }
    return parts;
}

struct DupCandidate {
    std::string path;
    uint64_t size = 0;
    uint64_t mtime = 0;
    uint64_t sortKey = 0;
    uint64_t frn = 0;
    uint64_t firstLcn = 0;
    uint32_t bytesPerCluster = 0;
    bool canRawRead = false;
    bool isSsd = false;
    std::optional<filehash::FileId> fileId;
    std::optional<Digest> head;
    std::optional<Digest> sample;
    std::optional<Digest> strong;
    std::optional<Digest> full;
    std::optional<Digest> probe;
    bool dirtyHead = false;
    bool dirtySample = false;
    bool dirtyStrong = false;
    bool dirtyFull = false;
};

struct PipelineOutput {
    std::vector<DupCandidate> finalItems;
    std::vector<DupCandidate> eliminated;
};

struct HashKey {
    uint64_t size = 0;
    Digest digest{};

    bool operator==(const HashKey& other) const {
        return size == other.size && digest == other.digest;
    }
};

struct HashKeyHash {
    size_t operator()(const HashKey& key) const {
        size_t value = std::hash<uint64_t>()(key.size);
        for (const uint8_t byte : key.digest) {
            value ^= static_cast<size_t>(byte) + 0x9e3779b97f4a7c15ULL +
                     (value << 6) + (value >> 2);
        }
        return value;
    }
};

struct FileIdHash {
    size_t operator()(const filehash::FileId& id) const {
        return std::hash<uint64_t>()(
            (static_cast<uint64_t>(id.first) << 40) ^ id.second);
    }
};

struct FileIdEqual {
    bool operator()(const filehash::FileId& left,
                    const filehash::FileId& right) const {
        return left.first == right.first && left.second == right.second;
    }
};

using FileIdSet =
    std::unordered_set<filehash::FileId, FileIdHash, FileIdEqual>;

using GroupMap = std::unordered_map<HashKey, std::vector<DupCandidate>, HashKeyHash>;

struct DupGroup {
    Digest digest{};
    uint64_t size = 0;
    std::vector<std::string> files;
    std::vector<uint64_t> mtimes;
};

void BuildDupGroups(GroupMap byHash, std::vector<DupGroup>& groups) {
    std::vector<GroupMap::value_type> candidates;
    candidates.reserve(byHash.size());
    for (auto& entry : byHash) {
        if (entry.second.size() >= 2) {
            candidates.push_back(std::move(entry));
        }
    }

    // 只有候选组（≥2 个文件）才需要硬链接去重；缺 file_id 的条目并行补查。
    ParallelForEachIndex(candidates.size(), kSsdThreads, [&](size_t index) {
        for (DupCandidate& item : candidates[index].second) {
            if (!item.fileId.has_value()) {
                item.fileId = fastio::QueryFileIdentity(item.path);
            }
        }
    });

    for (auto& entry : candidates) {
        std::vector<DupCandidate>& items = entry.second;
        std::vector<DupCandidate> kept;
        kept.reserve(items.size());
        FileIdSet seen;
        for (DupCandidate& item : items) {
            if (item.fileId.has_value() &&
                !seen.insert(*item.fileId).second) {
                continue;
            }
            kept.push_back(std::move(item));
        }
        if (kept.size() < 2) {
            continue;
        }
        std::sort(kept.begin(), kept.end(),
                  [](const DupCandidate& left, const DupCandidate& right) {
                      return left.path < right.path;
                  });
        DupGroup group;
        group.digest = entry.first.digest;
        group.size = entry.first.size;
        group.files.reserve(kept.size());
        group.mtimes.reserve(kept.size());
        for (const DupCandidate& item : kept) {
            group.files.push_back(item.path);
            group.mtimes.push_back(item.mtime);
        }
        Found();
        groups.push_back(std::move(group));
    }
}

void SortDupGroups(std::vector<DupGroup>& groups) {
    std::stable_sort(groups.begin(), groups.end(),
                     [](const DupGroup& left, const DupGroup& right) {
                         const uint64_t leftWeight =
                             left.size * left.files.size();
                         const uint64_t rightWeight =
                             right.size * right.files.size();
                         return leftWeight > rightWeight;
                     });
}

void FlushCandidateHashes(hashcache::HashCache& cache,
                          const std::vector<DupCandidate>& items,
                          uint64_t stampNow) {
    for (const DupCandidate& item : items) {
        if (!(item.dirtyHead || item.dirtySample || item.dirtyStrong ||
              item.dirtyFull)) {
            continue;
        }
        cache.RememberDigests(
            item.path, item.size, item.mtime,
            item.dirtyHead ? item.head : std::nullopt,
            item.dirtySample ? item.sample : std::nullopt,
            item.dirtyStrong ? item.strong : std::nullopt,
            item.dirtyFull ? item.full : std::nullopt, stampNow);
    }
}

PipelineOutput RunDupPipeline(std::vector<DupCandidate> stage1Items, bool fast,
                              bool turbo, size_t hddNcqWorkers,
                              bool updateStage, bool trace,
                              const char* label) {
    PipelineOutput output;
    if (stage1Items.empty()) {
        return output;
    }

    // 阶段 1：先按物理 LCN 合并直读卷头部，剩余项再走 OpenFileById + IOCP。
    if (updateStage) {
        scanstate::Stage("dup", "初步筛选");
    }
    std::vector<DupCandidate> stage1Done = std::move(stage1Items);

    struct RawJob {
        char letter = 'C';
        std::string root;
        bool isSsd = false;
        std::vector<fastio::RawHeadTarget> targets;
    };
    std::unordered_map<char, RawJob> rawByDrive;
    for (size_t index = 0; index < stage1Done.size(); ++index) {
        DupCandidate& item = stage1Done[index];
        if (item.head.has_value()) {
            Tick();
            continue;
        }
        if (item.canRawRead && item.bytesPerCluster > 0 && item.firstLcn > 0) {
            char letter = 'C';
            for (const char ch : item.path) {
                if (ch != '\\' && ch != '/') {
                    letter = static_cast<char>(
                        std::toupper(static_cast<unsigned char>(ch)));
                    break;
                }
            }
            const uint64_t offset =
                item.firstLcn * static_cast<uint64_t>(item.bytesPerCluster);
            fastio::RawHeadTarget target;
            target.idx = index;
            target.offset = offset;
            target.want = static_cast<size_t>(std::min(kHeadChunk, item.size));
            auto found = rawByDrive.find(letter);
            if (found == rawByDrive.end()) {
                RawJob job;
                job.letter = letter;
                job.root = std::string(1, letter) + ":\\";
                job.isSsd = item.isSsd;
                found = rawByDrive.emplace(letter, std::move(job)).first;
            }
            found->second.targets.push_back(target);
        }
    }

    if (!rawByDrive.empty() && !gDupCancel.load()) {
        std::vector<RawJob> jobs;
        jobs.reserve(rawByDrive.size());
        for (auto& entry : rawByDrive) {
            jobs.push_back(std::move(entry.second));
        }
        const std::atomic<bool>& cancel = gDupCancel;
        const std::vector<std::vector<std::pair<size_t, Digest>>> batches =
            RunDiskAwareNcq(
                std::move(jobs),
                [](const RawJob& job) -> const std::string& { return job.root; },
                [](const RawJob&) -> uint64_t { return 0; },
                1,
                [&cancel](RawJob job) {
                    return fastio::CoalescedRawVolumeHeads(
                        job.letter, std::move(job.targets), job.isSsd, cancel,
                        [] { Tick(); });
                });
        for (const std::vector<std::pair<size_t, Digest>>& batch : batches) {
            for (const auto& [index, digest] : batch) {
                DupCandidate& item = stage1Done[index];
                item.head = digest;
                item.dirtyHead = true;
                if (item.size <= kHeadChunk) {
                    item.sample = digest;
                    item.strong = digest;
                    item.full = digest;
                    item.dirtySample = true;
                    item.dirtyStrong = true;
                    item.dirtyFull = true;
                }
            }
        }
    }

    // 未命中卷直读的项（近期修改/加密/压缩/非 NTFS/降级遍历）走 IOCP 异步批读。
    {
        std::unordered_map<char, disk::DiskKey> diskCache;
        std::vector<fastio::IocpHeadRequest> ssdIocp;
        std::unordered_map<disk::DiskKey,
                           std::vector<std::pair<uint64_t, fastio::IocpHeadRequest>>,
                           disk::DiskKeyHash>
            hddIocp;
        for (size_t index = 0; index < stage1Done.size(); ++index) {
            const DupCandidate& item = stage1Done[index];
            if (item.head.has_value()) {
                continue;
            }
            fastio::IocpHeadRequest request;
            request.idx = index;
            request.path = item.path;
            request.frn = item.frn;
            request.want = static_cast<size_t>(std::min(kHeadChunk, item.size));
            const disk::DiskKey key = disk::KeyForPath(item.path, diskCache);
            if (key.kind == disk::DiskKeyKind::Ssd) {
                ssdIocp.push_back(std::move(request));
            } else {
                hddIocp[key].emplace_back(item.sortKey, std::move(request));
            }
        }

        if (!ssdIocp.empty() || !hddIocp.empty()) {
            std::vector<std::vector<fastio::IocpHeadRequest>> hddLists;
            hddLists.reserve(hddIocp.size());
            for (auto& entry : hddIocp) {
                std::vector<std::pair<uint64_t, fastio::IocpHeadRequest>>& list =
                    entry.second;
                std::stable_sort(
                    list.begin(), list.end(),
                    [](const std::pair<uint64_t, fastio::IocpHeadRequest>& left,
                       const std::pair<uint64_t, fastio::IocpHeadRequest>& right) {
                        return left.first < right.first;
                    });
                std::vector<fastio::IocpHeadRequest> requests;
                requests.reserve(list.size());
                for (auto& item : list) {
                    requests.push_back(std::move(item.second));
                }
                hddLists.push_back(std::move(requests));
            }

            std::vector<fastio::IocpHeadResult> all;
            std::vector<std::vector<fastio::IocpHeadResult>> hddResults(
                hddLists.size());
            std::vector<std::thread> threads;
            for (size_t slot = 0; slot < hddLists.size(); ++slot) {
                threads.emplace_back([&hddLists, &hddResults, slot] {
                    hddResults[slot] = fastio::IocpReadHeads(
                        std::move(hddLists[slot]), 8, gDupCancel,
                        [] { Tick(); });
                });
            }
            if (!ssdIocp.empty()) {
                all = fastio::IocpReadHeads(std::move(ssdIocp), 32, gDupCancel,
                                            [] { Tick(); });
            }
            for (std::thread& thread : threads) {
                thread.join();
            }
            for (std::vector<fastio::IocpHeadResult>& part : hddResults) {
                for (fastio::IocpHeadResult& item : part) {
                    all.push_back(std::move(item));
                }
            }

            for (const fastio::IocpHeadResult& result : all) {
                DupCandidate& item = stage1Done[result.idx];
                if (!item.fileId.has_value()) {
                    item.fileId = result.fileId;
                }
                item.head = result.digest;
                item.dirtyHead = true;
                if (item.size <= kHeadChunk) {
                    item.sample = result.digest;
                    item.strong = result.digest;
                    item.full = result.digest;
                    item.dirtySample = true;
                    item.dirtyStrong = true;
                    item.dirtyFull = true;
                }
            }
        }
    }

    if (DupCancelled()) {
        output.eliminated = std::move(stage1Done);
        return output;
    }

    GroupMap byHead;
    for (DupCandidate& item : stage1Done) {
        if (item.head.has_value()) {
            HashKey key;
            key.size = item.size;
            key.digest = *item.head;
            byHead[key].push_back(std::move(item));
        }
    }

    std::vector<DupCandidate> stage2Items;
    for (auto& entry : byHead) {
        std::vector<DupCandidate>& group = entry.second;
        std::vector<DupCandidate> kept;
        kept.reserve(group.size());
        FileIdSet seen;
        for (DupCandidate& item : group) {
            if (item.fileId.has_value() &&
                !seen.insert(*item.fileId).second) {
                continue;
            }
            kept.push_back(std::move(item));
        }
        if (kept.size() >= 2) {
            for (DupCandidate& item : kept) {
                stage2Items.push_back(std::move(item));
            }
        } else {
            for (DupCandidate& item : kept) {
                output.eliminated.push_back(std::move(item));
            }
        }
    }

    if (trace) {
        std::fprintf(stderr, "dup[%s]: 4KB 初筛后剩余 %zu 个文件\n", label,
                     stage2Items.size());
    }
    if (stage2Items.empty()) {
        return output;
    }

    if (turbo) {
        // 极速模式：4KB 头哈希已经是最终判据，不再做采样与全量校验。
        // 误判的组会在 delete_dups 的逐字节复核阶段被拦下。
        for (DupCandidate& item : stage2Items) {
            output.finalItems.push_back(std::move(item));
        }
        return output;
    }

    if (fast) {
        size_t uncached = 0;
        for (const DupCandidate& item : stage2Items) {
            if (!item.strong.has_value()) {
                ++uncached;
            }
        }
        scanstate::AddTotal("dup", uncached);
        if (updateStage) {
            scanstate::Stage("dup", "快速比对");
        }
        std::vector<DupCandidate> stage2Done = RunDiskAwareNcq(
            std::move(stage2Items),
            [](const DupCandidate& item) -> const std::string& {
                return item.path;
            },
            [](const DupCandidate& item) { return item.sortKey; },
            hddNcqWorkers, [](DupCandidate item) {
                if (gDupCancel.load()) {
                    return item;
                }
                if (!item.strong.has_value()) {
                    if (!item.isSsd && item.size <= kHddSeqThreshold) {
                        const std::optional<filehash::SeqSampleFull> hashed =
                            filehash::ReadHddSeqSampleAndFull(
                                item.path, item.frn, item.size, kStrongChunk);
                        if (hashed.has_value()) {
                            if (!item.fileId.has_value()) {
                                item.fileId = hashed->fileId;
                            }
                            item.strong = hashed->sample;
                            item.full = hashed->full;
                            item.dirtyStrong = true;
                            item.dirtyFull = true;
                        }
                    } else {
                        const std::optional<filehash::Hashed> hashed =
                            filehash::StrongHash(item.path, item.frn, item.size);
                        if (hashed.has_value()) {
                            if (!item.fileId.has_value()) {
                                item.fileId = hashed->fileId;
                            }
                            item.strong = hashed->digest;
                            item.dirtyStrong = true;
                            if (item.size <= kStrongChunk * 3) {
                                item.full = hashed->digest;
                                item.dirtyFull = true;
                            }
                        }
                    }
                    Tick();
                }
                return item;
            });

        if (DupCancelled()) {
            for (DupCandidate& item : stage2Done) {
                output.eliminated.push_back(std::move(item));
            }
            return output;
        }
        output.finalItems = std::move(stage2Done);
        return output;
    }

    // 阶段 2：4KB 碰撞候选做采样比对（机械盘 ≤2MB 一次顺序读同时算出 sample 与 full）。
    size_t needSample = 0;
    for (const DupCandidate& item : stage2Items) {
        if (!item.sample.has_value() && !item.full.has_value()) {
            ++needSample;
        }
    }
    scanstate::AddTotal("dup", needSample);
    if (updateStage) {
        scanstate::Stage("dup", "采样比对");
    }
    std::vector<DupCandidate> stage2Done = RunDiskAwareNcq(
        std::move(stage2Items),
        [](const DupCandidate& item) -> const std::string& { return item.path; },
        [](const DupCandidate& item) { return item.sortKey; }, hddNcqWorkers,
        [](DupCandidate item) {
            if (gDupCancel.load()) {
                return item;
            }
            if (!item.sample.has_value() && !item.full.has_value()) {
                if (!item.isSsd && item.size <= kHddSeqThreshold) {
                    const std::optional<filehash::SeqSampleFull> hashed =
                        filehash::ReadHddSeqSampleAndFull(
                            item.path, item.frn, item.size, kSampleChunk);
                    if (hashed.has_value()) {
                        if (!item.fileId.has_value()) {
                            item.fileId = hashed->fileId;
                        }
                        item.sample = hashed->sample;
                        item.full = hashed->full;
                        item.dirtySample = true;
                        item.dirtyFull = true;
                    }
                } else {
                    const std::optional<filehash::Hashed> hashed =
                        filehash::SampleHash(item.path, item.frn, item.size);
                    if (hashed.has_value()) {
                        if (!item.fileId.has_value()) {
                            item.fileId = hashed->fileId;
                        }
                        item.sample = hashed->digest;
                        item.dirtySample = true;
                        if (item.size <= kSampleChunk * 3) {
                            item.full = hashed->digest;
                            item.dirtyFull = true;
                        }
                    }
                }
                Tick();
            }
            return item;
        });

    if (DupCancelled()) {
        for (DupCandidate& item : stage2Done) {
            output.eliminated.push_back(std::move(item));
        }
        return output;
    }

    GroupMap bySample;
    std::vector<DupCandidate> stage3Items;
    for (DupCandidate& item : stage2Done) {
        if (item.sample.has_value()) {
            HashKey key;
            key.size = item.size;
            key.digest = *item.sample;
            bySample[key].push_back(std::move(item));
        } else if (item.full.has_value()) {
            stage3Items.push_back(std::move(item));
        }
    }
    for (auto& entry : bySample) {
        if (entry.second.size() >= 2) {
            for (DupCandidate& item : entry.second) {
                stage3Items.push_back(std::move(item));
            }
        } else {
            for (DupCandidate& item : entry.second) {
                output.eliminated.push_back(std::move(item));
            }
        }
    }

    if (trace) {
        std::fprintf(stderr, "dup[%s]: 采样后仍需全校验 %zu 个文件\n", label,
                     stage3Items.size());
    }
    if (stage3Items.empty() || std::getenv("CDC_DUP_SAMPLE_ONLY") != nullptr) {
        for (DupCandidate& item : stage3Items) {
            output.eliminated.push_back(std::move(item));
        }
        return output;
    }

    // 阶段 3 前置：>16MB 且全员未算过全哈希的碰撞组，先读 25%/75% 各 64KB 探针。
    GroupMap probeGroups;
    std::vector<DupCandidate> directStage3;
    for (DupCandidate& item : stage3Items) {
        if (item.size > kProbeThreshold && item.sample.has_value()) {
            HashKey key;
            key.size = item.size;
            key.digest = *item.sample;
            probeGroups[key].push_back(std::move(item));
            continue;
        }
        directStage3.push_back(std::move(item));
    }

    std::vector<DupCandidate> needProbe;
    for (auto& entry : probeGroups) {
        std::vector<DupCandidate>& group = entry.second;
        bool allMissingFull = true;
        for (const DupCandidate& item : group) {
            if (item.full.has_value()) {
                allMissingFull = false;
                break;
            }
        }
        if (group.size() >= 2 && allMissingFull) {
            for (DupCandidate& item : group) {
                needProbe.push_back(std::move(item));
            }
        } else {
            for (DupCandidate& item : group) {
                directStage3.push_back(std::move(item));
            }
        }
    }

    if (!needProbe.empty() && !gDupCancel.load()) {
        scanstate::AddTotal("dup", needProbe.size());
        std::vector<DupCandidate> probed = RunDiskAwareNcq(
            std::move(needProbe),
            [](const DupCandidate& item) -> const std::string& {
                return item.path;
            },
            [](const DupCandidate& item) { return item.sortKey; },
            hddNcqWorkers, [](DupCandidate item) {
                if (gDupCancel.load()) {
                    return item;
                }
                const std::optional<filehash::Hashed> hashed =
                    filehash::ProbeHash(item.path, item.frn, item.size);
                if (hashed.has_value()) {
                    if (!item.fileId.has_value()) {
                        item.fileId = hashed->fileId;
                    }
                    item.probe = hashed->digest;
                }
                Tick();
                return item;
            });
        if (DupCancelled()) {
            for (DupCandidate& item : directStage3) {
                output.eliminated.push_back(std::move(item));
            }
            for (DupCandidate& item : probed) {
                output.eliminated.push_back(std::move(item));
            }
            return output;
        }
        GroupMap byProbe;
        for (DupCandidate& item : probed) {
            if (item.probe.has_value()) {
                HashKey key;
                key.size = item.size;
                key.digest = *item.probe;
                byProbe[key].push_back(std::move(item));
            } else {
                directStage3.push_back(std::move(item));
            }
        }
        for (auto& entry : byProbe) {
            if (entry.second.size() >= 2) {
                for (DupCandidate& item : entry.second) {
                    directStage3.push_back(std::move(item));
                }
            } else {
                for (DupCandidate& item : entry.second) {
                    output.eliminated.push_back(std::move(item));
                }
            }
        }
    }

    if (directStage3.empty()) {
        return output;
    }

    // 阶段 3：全文件顺序双缓冲校验（机械盘保持单路顺序流式读）。
    size_t needFull = 0;
    for (const DupCandidate& item : directStage3) {
        if (!item.full.has_value()) {
            ++needFull;
        }
    }
    scanstate::AddTotal("dup", needFull);
    if (updateStage) {
        scanstate::Stage("dup", "校验内容");
    }

    std::vector<DupCandidate> stage3Done = RunDiskAwareNcq(
        std::move(directStage3),
        [](const DupCandidate& item) -> const std::string& { return item.path; },
        [](const DupCandidate& item) { return item.sortKey; }, 1,
        [](DupCandidate item) {
            if (gDupCancel.load()) {
                return item;
            }
            if (!item.full.has_value()) {
                const std::optional<filehash::Hashed> hashed =
                    filehash::FullHash(item.path, item.frn, gDupCancel);
                if (hashed.has_value()) {
                    if (!item.fileId.has_value()) {
                        item.fileId = hashed->fileId;
                    }
                    item.full = hashed->digest;
                    item.dirtyFull = true;
                }
                Tick();
            }
            return item;
        });

    if (DupCancelled()) {
        for (DupCandidate& item : stage3Done) {
            output.eliminated.push_back(std::move(item));
        }
        return output;
    }

    output.finalItems = std::move(stage3Done);
    return output;
}

/// Rust 的 `format!("{:?}", roots)`：用于生成 dups-<blake3>.json 的缓存键。
std::string DebugStringList(const std::vector<std::string>& values) {
    std::string out = "[";
    for (size_t index = 0; index < values.size(); ++index) {
        if (index != 0) {
            out += ", ";
        }
        out.push_back('"');
        for (const char ch : values[index]) {
            switch (ch) {
                case '"':
                    out += "\\\"";
                    break;
                case '\\':
                    out += "\\\\";
                    break;
                case '\n':
                    out += "\\n";
                    break;
                case '\r':
                    out += "\\r";
                    break;
                case '\t':
                    out += "\\t";
                    break;
                default:
                    out.push_back(ch);
                    break;
            }
        }
        out.push_back('"');
    }
    out += "]";
    return out;
}

std::string DupsCachePath(const std::vector<std::string>& roots, uint64_t min,
                          bool fast, bool turbo) {
    const std::string key = DebugStringList(roots) + ":" +
                            std::to_string(min) + ":" +
                            (fast ? "true" : "false") +
                            (turbo ? ":turbo" : "");
    const Digest digest = Blake3Hash(key.data(), key.size());
    return JoinPath(CacheDir(), "dups-" + hashcache::DigestToHex(digest) + ".json");
}

std::optional<uint64_t> FileMtimeUnixSeconds(const std::string& path) {
    WIN32_FILE_ATTRIBUTE_DATA data = {};
    if (GetFileAttributesExW(Utf8ToWide(path).c_str(), GetFileExInfoStandard,
                             &data) == 0) {
        return std::nullopt;
    }
    const uint64_t fileTime =
        (static_cast<uint64_t>(data.ftLastWriteTime.dwHighDateTime) << 32) |
        static_cast<uint64_t>(data.ftLastWriteTime.dwLowDateTime);
    constexpr uint64_t kWindowsEpochDiffSecs = 11'644'473'600ULL;
    const uint64_t seconds = fileTime / 10'000'000ULL;
    if (seconds <= kWindowsEpochDiffSecs) {
        return 0;
    }
    return seconds - kWindowsEpochDiffSecs;
}

bool CacheIsFresh(const std::string& path) {
    const std::optional<uint64_t> stamp = FileMtimeUnixSeconds(path);
    if (!stamp.has_value()) {
        return false;
    }
    const uint64_t now = NowUnixSeconds();
    return now >= *stamp && now - *stamp < kCacheFreshSeconds;
}

std::vector<std::string> ResolveRoots(const std::optional<std::string>& root,
                                      const std::vector<std::string>& drives) {
    const std::vector<std::string> fixed = disk::FixedDrives();
    std::vector<std::string> roots;
    if (!drives.empty()) {
        for (const std::string& drive : drives) {
            for (const std::string& candidate : fixed) {
                if (EqualsIgnoreCase(drive, candidate)) {
                    roots.push_back(drive);
                    break;
                }
            }
        }
    } else if (root.has_value()) {
        roots.push_back(*root);
    } else {
        roots = fixed;
    }
    return roots;
}

std::optional<std::string> ReadTextFile(const std::string& path) {
    FILE* file = nullptr;
    if (_wfopen_s(&file, Utf8ToWide(path).c_str(), L"rb") != 0 ||
        file == nullptr) {
        return std::nullopt;
    }
    std::string text;
    char buffer[65536];
    size_t read = 0;
    while ((read = fread(buffer, 1, sizeof(buffer), file)) > 0) {
        text.append(buffer, read);
    }
    fclose(file);
    return text;
}

bool WriteTextFile(const std::string& path, const std::string& text) {
    FILE* file = nullptr;
    if (_wfopen_s(&file, Utf8ToWide(path).c_str(), L"wb") != 0 ||
        file == nullptr) {
        return false;
    }
    const size_t written = fwrite(text.data(), 1, text.size(), file);
    fclose(file);
    return written == text.size();
}

Json GroupsToJson(const std::vector<DupGroup>& groups) {
    Json out = Json::array();
    for (const DupGroup& group : groups) {
        Json item;
        item["hash"] = hashcache::DigestToHex(group.digest);
        item["size"] = group.size;
        item["files"] = group.files;
        item["mtimes"] = group.mtimes;
        out.push_back(std::move(item));
    }
    return out;
}

bool GroupsFromJson(const Json& value, std::vector<Json>& out) {
    if (!value.is_array()) {
        return false;
    }
    for (const Json& item : value) {
        if (!item.is_object()) {
            return false;
        }
        const auto hash = item.find("hash");
        const auto size = item.find("size");
        const auto files = item.find("files");
        const auto mtimes = item.find("mtimes");
        if (hash == item.end() || !hash->is_string() ||
            size == item.end() || !size->is_number_unsigned() ||
            files == item.end() || !files->is_array() ||
            mtimes == item.end() || !mtimes->is_array()) {
            return false;
        }
        out.push_back(item);
    }
    return true;
}

}  // namespace

namespace dups {

Json ScanDups(const std::optional<std::string>& root,
              const std::vector<std::string>& drives,
              std::optional<uint64_t> minSize, bool fast, bool turbo) {
    scanstate::Begin("dup", "枚举文件");
    gDupCancel.store(false);
    const bool trace = std::getenv("CDC_TRACE") != nullptr;

    const std::vector<std::string> roots = ResolveRoots(root, drives);
    if (roots.empty()) {
        scanstate::End("dup");
        return Json::array();
    }

    const uint64_t min = minSize.value_or(kDefaultMinSize);
    const std::string cachePath = DupsCachePath(roots, min, fast, turbo);
    if (CacheIsFresh(cachePath)) {
        if (const std::optional<std::string> text = ReadTextFile(cachePath)) {
            const Json parsed = Json::parse(*text, nullptr, false);
            std::vector<Json> groups;
            if (!parsed.is_discarded() && GroupsFromJson(parsed, groups)) {
                scanstate::End("dup");
                return parsed;
            }
        }
    }

    const auto started = std::chrono::steady_clock::now();
    scanstate::Stage("dup", "枚举文件");
    std::vector<std::vector<mft::FileEntry>> perDrive =
        ScanDrivesPerPhysicalDisk(roots, min);

    if (gDupCancel.load() || scanstate::WasCancelled("dup")) {
        scanstate::End("dup");
        return Json::array();
    }

    if (trace) {
        size_t enumeratedCount = 0;
        for (const std::vector<mft::FileEntry>& files : perDrive) {
            enumeratedCount += files.size();
        }
        std::fprintf(stderr, "dup: 枚举得到 %zu 个文件\n", enumeratedCount);
    }

    // 第一层快速过滤：大小唯一的文件直接剔除。
    std::unordered_map<uint64_t, std::vector<mft::FileEntry>> bySize;
    for (std::vector<mft::FileEntry>& files : perDrive) {
        for (mft::FileEntry& entry : files) {
            if (IsSystemFileName(entry.path)) {
                continue;
            }
            bySize[entry.size].push_back(std::move(entry));
        }
    }
    perDrive.clear();

    std::vector<std::vector<mft::FileEntry>> sizeBuckets;
    sizeBuckets.reserve(bySize.size());
    size_t candidatesCount = 0;
    for (auto& entry : bySize) {
        if (entry.second.size() >= 2) {
            candidatesCount += entry.second.size();
            sizeBuckets.push_back(std::move(entry.second));
        }
    }
    bySize.clear();

    if (trace) {
        const double seconds =
            std::chrono::duration<double>(std::chrono::steady_clock::now() -
                                          started)
                .count();
        std::fprintf(stderr, "dup: 参与比对 %zu 个文件, 枚举用时 %.1fs\n",
                     candidatesCount, seconds);
    }
    if (candidatesCount == 0) {
        scanstate::End("dup");
        return Json::array();
    }

    std::vector<std::string> pathList;
    pathList.reserve(candidatesCount);
    for (const std::vector<mft::FileEntry>& bucket : sizeBuckets) {
        for (const mft::FileEntry& entry : bucket) {
            pathList.push_back(entry.path);
        }
    }
    hashcache::HashCache cache = hashcache::HashCache::LoadForPaths(pathList);
    pathList.clear();
    pathList.shrink_to_fit();
    const uint64_t stampNow = NowUnixSeconds();

    std::unordered_map<char, disk::DiskKey> diskCache;
    GroupMap readyFinal;
    std::vector<DupCandidate> ssdOnlyStage1;
    std::vector<DupCandidate> mixedStage1;
    size_t cacheHits = 0;

    for (std::vector<mft::FileEntry>& bucket : sizeBuckets) {
        std::vector<DupCandidate> converted;
        converted.reserve(bucket.size());
        bool allReady = true;
        bool allSsd = true;
        for (mft::FileEntry& entry : bucket) {
            const bool isSsd = disk::KeyForPath(entry.path, diskCache).kind ==
                               disk::DiskKeyKind::Ssd;
            if (!isSsd) {
                allSsd = false;
            }
            const uint64_t mtime =
                entry.mtime != 0 ? entry.mtime : FileMtimeSeconds(entry.path);
            const hashcache::Entry* cached =
                cache.EntryFor(entry.path, entry.size, mtime);
            if (cached != nullptr) {
                ++cacheHits;
            }
            DupCandidate candidate;
            candidate.path = std::move(entry.path);
            candidate.size = entry.size;
            candidate.mtime = mtime;
            candidate.sortKey = entry.sortKey;
            candidate.frn = entry.frn;
            candidate.firstLcn = entry.firstLcn;
            candidate.bytesPerCluster = entry.bytesPerCluster;
            candidate.canRawRead = entry.canRawRead;
            candidate.isSsd = isSsd;
            if (cached != nullptr) {
                candidate.head = cached->head;
                candidate.sample = cached->sample;
                candidate.strong = cached->strong;
                candidate.full = cached->full;
            }
            const bool hasFinal = turbo
                                      ? candidate.head.has_value()
                                      : (fast ? candidate.strong.has_value()
                                              : candidate.full.has_value());
            if (!hasFinal) {
                allReady = false;
            }
            converted.push_back(std::move(candidate));
        }
        if (allReady) {
            for (DupCandidate& item : converted) {
                const Digest digest = turbo ? *item.head
                                            : (fast ? *item.strong : *item.full);
                HashKey key;
                key.size = item.size;
                key.digest = digest;
                readyFinal[key].push_back(std::move(item));
            }
        } else if (allSsd) {
            for (DupCandidate& item : converted) {
                ssdOnlyStage1.push_back(std::move(item));
            }
        } else {
            for (DupCandidate& item : converted) {
                mixedStage1.push_back(std::move(item));
            }
        }
    }
    sizeBuckets.clear();

    if (trace) {
        std::fprintf(stderr, "dup: 哈希缓存命中 %zu / %zu 个候选\n", cacheHits,
                     candidatesCount);
    }
    const size_t pendingTotal = ssdOnlyStage1.size() + mixedStage1.size();
    if (pendingTotal == 0) {
        std::vector<DupGroup> groups;
        BuildDupGroups(std::move(readyFinal), groups);
        SortDupGroups(groups);
        WriteTextFile(cachePath, GroupsToJson(groups).dump());
        scanstate::End("dup");
        return GroupsToJson(groups);
    }

    scanstate::Reset("dup", pendingTotal);

    // 消除栅栏：纯固态候选桶独立跑完 1→2→3 阶段，不等待慢速机械盘的同步点。
    const bool hasMixed = !mixedStage1.empty();
    PipelineOutput ssdOut;
    PipelineOutput mixedOut;
    {
        std::optional<std::thread> ssdThread;
        if (!ssdOnlyStage1.empty()) {
            ssdThread.emplace([&ssdOnlyStage1, &ssdOut, fast, turbo, hasMixed,
                               trace] {
                ssdOut = RunDupPipeline(std::move(ssdOnlyStage1), fast, turbo,
                                        1, !hasMixed, trace, "ssd");
            });
        }
        if (hasMixed) {
            mixedOut = RunDupPipeline(std::move(mixedStage1), fast, turbo, 2,
                                      true, trace, "hdd+mixed");
        }
        if (ssdThread.has_value()) {
            ssdThread->join();
        }
    }

    FlushCandidateHashes(cache, ssdOut.eliminated, stampNow);
    FlushCandidateHashes(cache, ssdOut.finalItems, stampNow);
    FlushCandidateHashes(cache, mixedOut.eliminated, stampNow);
    FlushCandidateHashes(cache, mixedOut.finalItems, stampNow);
    cache.SaveDirty();

    if (DupCancelled()) {
        scanstate::End("dup");
        return Json::array();
    }

    auto mergeFinal = [&readyFinal, fast, turbo](
                          std::vector<DupCandidate>& items) {
        for (DupCandidate& item : items) {
            const std::optional<Digest>& digest =
                turbo ? item.head : (fast ? item.strong : item.full);
            if (!digest.has_value()) {
                continue;
            }
            HashKey key;
            key.size = item.size;
            key.digest = *digest;
            readyFinal[key].push_back(std::move(item));
        }
    };
    mergeFinal(ssdOut.finalItems);
    mergeFinal(mixedOut.finalItems);

    std::vector<DupGroup> groups;
    BuildDupGroups(std::move(readyFinal), groups);
    SortDupGroups(groups);

    if (trace) {
        const double seconds =
            std::chrono::duration<double>(std::chrono::steady_clock::now() -
                                          started)
                .count();
        std::fprintf(stderr, "dup: 找到 %zu 组, 总用时 %.1fs\n", groups.size(),
                     seconds);
    }
    if (!scanstate::WasCancelled("dup")) {
        WriteTextFile(cachePath, GroupsToJson(groups).dump());
    }
    scanstate::End("dup");
    return GroupsToJson(groups);
}

Json CachedResult(const std::optional<std::string>& root,
                  const std::vector<std::string>& drives,
                  std::optional<uint64_t> minSize, bool fast, bool turbo) {
    Json result;
    result["groups"] = Json::array();
    result["cached_at"] = 0;

    const std::vector<std::string> roots = ResolveRoots(root, drives);
    if (roots.empty()) {
        return result;
    }
    const std::string cachePath =
        DupsCachePath(roots, minSize.value_or(kDefaultMinSize), fast, turbo);
    const std::optional<uint64_t> stamp = FileMtimeUnixSeconds(cachePath);
    if (!stamp.has_value()) {
        return result;
    }
    const std::optional<std::string> text = ReadTextFile(cachePath);
    if (!text.has_value()) {
        return result;
    }
    const Json parsed = Json::parse(*text, nullptr, false);
    std::vector<Json> groups;
    if (parsed.is_discarded() || !GroupsFromJson(parsed, groups)) {
        return result;
    }
    result["groups"] = parsed;
    result["cached_at"] = *stamp;
    return result;
}

void InvalidateCache() {
    std::error_code error;
    const std::filesystem::path dir(Utf8ToWide(CacheDir()));
    for (const auto& entry : std::filesystem::directory_iterator(dir, error)) {
        if (error) {
            return;
        }
        const std::string name = WideToUtf8(entry.path().filename().wstring());
        if (name.rfind("dups-", 0) == 0 && name.size() > 5 &&
            name.compare(name.size() - 5, 5, ".json") == 0) {
            std::error_code removeError;
            std::filesystem::remove(entry.path(), removeError);
        }
    }
}

void CancelScan() { gDupCancel.store(true); }

Json DeleteDups(const Json& items) {
    Json report;
    report["freed"] = 0;
    report["deleted"] = 0;
    report["failed"] = Json::array();
    report["reasons"] = Json::array();
    std::vector<std::string> failed;
    std::vector<std::string> reasons;

    struct VerifyItem {
        std::string path;
        std::string hash;
        uint64_t size = 0;
        uint64_t mtime = 0;
        std::string keeper;
    };

    std::vector<VerifyItem> parsed;
    if (items.is_array()) {
        parsed.reserve(items.size());
        for (const Json& item : items) {
            if (!item.is_object()) {
                continue;
            }
            VerifyItem entry;
            if (const auto path = item.find("path");
                path != item.end() && path->is_string()) {
                entry.path = path->get<std::string>();
            }
            if (const auto hash = item.find("hash");
                hash != item.end() && hash->is_string()) {
                entry.hash = hash->get<std::string>();
            }
            auto readUint = [&item](const char* key, uint64_t& out) {
                const auto found = item.find(key);
                if (found == item.end()) {
                    return;
                }
                if (found->is_number_unsigned()) {
                    out = found->get<uint64_t>();
                } else if (found->is_number_integer()) {
                    const int64_t value = found->get<int64_t>();
                    if (value >= 0) {
                        out = static_cast<uint64_t>(value);
                    }
                }
            };
            readUint("size", entry.size);
            readUint("mtime", entry.mtime);
            if (const auto keeper = item.find("keeper");
                keeper != item.end() && keeper->is_string()) {
                entry.keeper = keeper->get<std::string>();
            }
            if (entry.path.empty()) {
                continue;
            }
            parsed.push_back(std::move(entry));
        }
    }

    try {
        task::Begin("dup_delete", "重复文件清理", true, true, 0, "个文件");
    } catch (const std::exception& error) {
        reasons.push_back(error.what());
        report["failed"] = failed;
        report["reasons"] = reasons;
        return report;
    }
    task::Guard guard("dup_delete");
    task::SetUnit("dup_delete", "个文件");
    task::SetTotal("dup_delete", parsed.size());
    task::SetStage("dup_delete", "校验文件");

    bool cancelled = false;
    std::vector<std::string> eligible;
    std::unordered_map<std::string, uint64_t> sizes;
    std::vector<VerifyItem> pendingVerify;
    std::vector<std::string> keeperSet;

    for (const VerifyItem& item : parsed) {
        if (task::IsCancelled("dup_delete")) {
            cancelled = true;
            break;
        }
        task::AddDone("dup_delete", 1);
        const std::string& path = item.path;
        if (!PathExists(path)) {
            failed.push_back(path);
            reasons.push_back(path + "：文件已不存在");
            continue;
        }
        if (safety::ExplicitDeleteForbidden(path)) {
            failed.push_back(path);
            reasons.push_back(path + "：受保护路径或密码库，已跳过");
            continue;
        }
        WIN32_FILE_ATTRIBUTE_DATA data = {};
        if (GetFileAttributesExW(Utf8ToWide(path).c_str(), GetFileExInfoStandard,
                                 &data) == 0) {
            failed.push_back(path);
            reasons.push_back(path + "：读不到文件属性");
            continue;
        }
        const uint64_t size =
            (static_cast<uint64_t>(data.nFileSizeHigh) << 32) |
            static_cast<uint64_t>(data.nFileSizeLow);
        if (size != item.size) {
            failed.push_back(path);
            reasons.push_back(path + "：扫描后大小变了，已跳过");
            continue;
        }
        if (FileMtimeSeconds(path) != item.mtime) {
            failed.push_back(path);
            reasons.push_back(path + "：扫描后修改时间变了，已跳过");
            continue;
        }
        if (!item.keeper.empty()) {
            bool known = false;
            for (const std::string& keeper : keeperSet) {
                if (keeper == item.keeper) {
                    known = true;
                    break;
                }
            }
            if (!known) {
                keeperSet.push_back(item.keeper);
            }
        }
        pendingVerify.push_back(item);
    }

    if (cancelled) {
        reasons.push_back("已取消");
        guard.Finish("cancelled", "已取消");
        report["failed"] = failed;
        report["reasons"] = reasons;
        return report;
    }

    // 并行按盘计算保留项与待删副本的完整哈希（宁可跳过，不误删）。
    std::vector<std::string> pathsToHash = keeperSet;
    for (const VerifyItem& item : pendingVerify) {
        pathsToHash.push_back(item.path);
    }
    task::SetStage("dup_delete", "核对内容");
    task::SetProgress("dup_delete", 0, pathsToHash.size());
    const std::atomic<bool>& cancel = gDupCancel;
    const std::vector<std::pair<std::string, std::optional<Digest>>> hashedList =
        RunDiskAwareNcq(
            std::move(pathsToHash),
            [](const std::string& path) -> const std::string& { return path; },
            [](const std::string&) -> uint64_t { return 0; }, 1,
            [&cancel](std::string path) {
                std::optional<Digest> digest;
                const std::optional<filehash::Hashed> hashed =
                    filehash::FullHash(path, 0, cancel);
                if (hashed.has_value()) {
                    digest = hashed->digest;
                }
                task::AddDone("dup_delete", 1);
                return std::make_pair(std::move(path), digest);
            });
    std::unordered_map<std::string, std::optional<Digest>> hashedMap;
    hashedMap.reserve(hashedList.size());
    for (const auto& entry : hashedList) {
        hashedMap[entry.first] = entry.second;
    }

    task::SetStage("dup_delete", "复核内容");
    task::SetProgress("dup_delete", 0, pendingVerify.size());
    for (const VerifyItem& item : pendingVerify) {
        if (task::IsCancelled("dup_delete")) {
            cancelled = true;
            break;
        }
        task::AddDone("dup_delete", 1);
        const std::string& path = item.path;
        std::optional<Digest> expected;
        if (item.keeper.empty()) {
            expected = hashcache::HexToDigest(item.hash);
        } else {
            const auto found = hashedMap.find(item.keeper);
            if (found != hashedMap.end()) {
                expected = found->second;
            }
        }
        if (!expected.has_value()) {
            failed.push_back(path);
            reasons.push_back(path + "：保留的文件读不到，已跳过");
            continue;
        }
        const auto actual = hashedMap.find(path);
        if (actual != hashedMap.end() && actual->second.has_value() &&
            *actual->second == *expected) {
            sizes[path] = item.size;
            eligible.push_back(path);
        } else {
            failed.push_back(path);
            reasons.push_back(path + "：内容与保留文件不一致，已跳过");
        }
    }
    if (cancelled) {
        reasons.push_back("已取消");
        guard.Finish("cancelled", "已取消");
        report["failed"] = failed;
        report["reasons"] = reasons;
        return report;
    }

    task::SetStage("dup_delete", "送入回收站");
    recycle::SendToRecycle(eligible);
    uint64_t deleted = 0;
    uint64_t freed = 0;
    for (const std::string& path : eligible) {
        if (PathExists(path)) {
            failed.push_back(path);
            reasons.push_back(path + "：送回收站失败（可能被占用）");
        } else {
            ++deleted;
            const auto found = sizes.find(path);
            if (found != sizes.end()) {
                freed += found->second;
            }
        }
    }
    InvalidateCache();
    guard.Finish("ok", "删除 " + std::to_string(deleted) + " 个，释放 " +
                           FormatMb(freed) + " MB");
    logs::WriteLog({"开始清理", "清理：重复文件",
                    "完成：删除 " + std::to_string(deleted) + " 个，释放 " +
                        std::to_string(freed) + " 字节，失败 " +
                        std::to_string(failed.size()) + " 个"});
    report["freed"] = freed;
    report["deleted"] = deleted;
    report["failed"] = failed;
    report["reasons"] = reasons;
    return report;
}

}  // namespace dups
