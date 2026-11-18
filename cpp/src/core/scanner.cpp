#include "core/scanner.h"

#include <windows.h>

#include <algorithm>
#include <atomic>
#include <cctype>
#include <cstdint>
#include <iomanip>
#include <optional>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <unordered_map>
#include <utility>
#include <vector>

#include "core/logs.h"
#include "core/rules.h"
#include "core/safety.h"
#include "core/task.h"
#include "core/timeutil.h"
#include "util/glob.h"
#include "util/path.h"
#include "util/utf.h"

namespace {

constexpr uint64_t kWindowsToUnixSeconds = 11644473600ULL;

struct CategorySize {
    std::string key;
    std::string name;
    std::string tier;
    std::string describe;
    std::string warning;
    std::string note;
    int64_t size = 0;
    uint64_t count = 0;
    bool requires_confirm = false;
    bool implemented = true;
};

struct CleanReport {
    uint64_t freed = 0;
    uint64_t deleted = 0;
    uint64_t skipped = 0;
    uint64_t pending = 0;
    std::vector<std::string> errors;
    std::vector<std::string> notes;
};

struct FileMeta {
    uint64_t size = 0;
    uint64_t mtime = 0;
    DWORD attributes = 0;
};

std::string ToLower(std::string_view text) {
    std::string out(text);
    for (char& ch : out) {
        ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    }
    return out;
}

std::optional<FileMeta> GetMeta(const std::string& path) {
    WIN32_FILE_ATTRIBUTE_DATA data = {};
    if (GetFileAttributesExW(Utf8ToWide(path).c_str(), GetFileExInfoStandard,
                             &data) == 0) {
        return std::nullopt;
    }
    const uint64_t size =
        (static_cast<uint64_t>(data.nFileSizeHigh) << 32) |
        static_cast<uint64_t>(data.nFileSizeLow);
    const uint64_t fileTime =
        (static_cast<uint64_t>(data.ftLastWriteTime.dwHighDateTime) << 32) |
        static_cast<uint64_t>(data.ftLastWriteTime.dwLowDateTime);
    const uint64_t mtime =
        fileTime / 10000000ULL - kWindowsToUnixSeconds;
    return FileMeta{size, mtime, data.dwFileAttributes};
}

bool IsDirectory(const std::string& path) {
    const DWORD attributes = GetFileAttributesW(Utf8ToWide(path).c_str());
    return attributes != INVALID_FILE_ATTRIBUTES &&
           (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
}

template <typename Callback>
void WalkFiles(const std::string& root, Callback&& callback) {
    const std::wstring wideRoot = Utf8ToWide(root);
    const DWORD attributes = GetFileAttributesW(wideRoot.c_str());
    if (attributes == INVALID_FILE_ATTRIBUTES ||
        (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0) {
        return;
    }
    if ((attributes & FILE_ATTRIBUTE_DIRECTORY) == 0) {
        const std::optional<FileMeta> meta = GetMeta(root);
        if (meta.has_value()) {
            callback(root, *meta);
        }
        return;
    }

    // 与 Rust 的 walkdir 一致：文件属性/大小/时间直接取目录枚举结果，
    // 不再对每个文件单独 stat（个别文件 GetFileAttributesW 会因 ACL 返回拒绝访问）。
    std::vector<std::wstring> stack = {wideRoot};
    while (!stack.empty()) {
        const std::wstring current = stack.back();
        stack.pop_back();
        WIN32_FIND_DATAW data;
        HANDLE handle = FindFirstFileW((current + L"\\*").c_str(), &data);
        if (handle == INVALID_HANDLE_VALUE) {
            continue;
        }
        do {
            if (wcscmp(data.cFileName, L".") == 0 ||
                wcscmp(data.cFileName, L"..") == 0) {
                continue;
            }
            if ((data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0) {
                continue;
            }
            const std::wstring childWide = current + L"\\" + data.cFileName;
            if ((data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
                stack.push_back(childWide);
                continue;
            }
            FileMeta meta;
            meta.size = (static_cast<uint64_t>(data.nFileSizeHigh) << 32) |
                        static_cast<uint64_t>(data.nFileSizeLow);
            const uint64_t fileTime =
                (static_cast<uint64_t>(data.ftLastWriteTime.dwHighDateTime) << 32) |
                static_cast<uint64_t>(data.ftLastWriteTime.dwLowDateTime);
            meta.mtime = fileTime / 10000000ULL - kWindowsToUnixSeconds;
            meta.attributes = data.dwFileAttributes;
            callback(WideToUtf8(childWide), meta);
        } while (FindNextFileW(handle, &data) != 0);
        FindClose(handle);
    }
}

std::vector<std::string> CollectRoots(const Rule& rule) {
    std::vector<std::string> roots = ExpandPaths(rule.paths);
    for (const std::string& path : ExpandPaths(rule.glob_dirs)) {
        if (IsDirectory(path)) {
            roots.push_back(path);
        }
    }
    std::sort(roots.begin(), roots.end());
    roots.erase(std::unique(roots.begin(), roots.end()), roots.end());
    return roots;
}

std::vector<GlobPattern> ExcludeMatcher(const Rule& rule) {
    std::vector<std::string> excludes;
    excludes.reserve(rule.exclude.size());
    for (const std::string& pattern : rule.exclude) {
        excludes.push_back(NormalizeExcludePattern(pattern));
    }
    return BuildMatcher(excludes);
}

std::optional<CategorySize> ScanRule(const Rule& rule) {
    if (rule.key.empty()) {
        return std::nullopt;
    }
    const std::string kind = rule.kind.empty() ? "files" : rule.kind;
    CategorySize category;
    category.key = rule.key;
    category.name = rule.name.empty() ? rule.key : rule.name;
    category.tier = rule.tier.empty() ? "standard" : rule.tier;
    category.describe = rule.describe;
    category.warning = rule.warning;
    category.note = rule.note;
    if (kind != "files") {
        category.size = 0;
        category.count = 0;
        category.requires_confirm = true;
        category.implemented = false;
        return category;
    }

    const std::vector<std::string> roots = CollectRoots(rule);
    if (roots.empty()) {
        return std::nullopt;
    }
    const std::vector<GlobPattern> include = BuildMatcher(rule.patterns);
    const std::vector<GlobPattern> exclude = ExcludeMatcher(rule);
    const uint64_t minAge = rule.min_age_minutes * 60;
    const uint64_t now = NowUnixSeconds();

    uint64_t size = 0;
    uint64_t count = 0;
    for (const std::string& root : roots) {
        WalkFiles(root, [&](const std::string& path, const FileMeta& meta) {
            const std::string name = ToLower(safety::Basename(path));
            if (IncludeMatches(name, include, exclude) &&
                AgeOk(meta.mtime, minAge, now)) {
                size += meta.size;
                ++count;
            }
        });
    }

    category.size = static_cast<int64_t>(size);
    category.count = count;
    category.requires_confirm =
        rule.requires_confirm || rule.tier == "deep";
    category.implemented = true;
    return category;
}

Json CategoryToJson(const CategorySize& category) {
    return Json{{"key", category.key},
                {"name", category.name},
                {"tier", category.tier},
                {"describe", category.describe},
                {"warning", category.warning},
                {"note", category.note},
                {"size", category.size},
                {"count", category.count},
                {"requires_confirm", category.requires_confirm},
                {"implemented", category.implemented}};
}

Json ReportToJson(const CleanReport& report) {
    return Json{{"freed", report.freed},
                {"deleted", report.deleted},
                {"skipped", report.skipped},
                {"pending", report.pending},
                {"errors", report.errors},
                {"notes", report.notes}};
}

std::string FormatMb(uint64_t bytes) {
    std::ostringstream stream;
    stream << std::fixed << std::setprecision(1)
           << static_cast<double>(bytes) / 1048576.0;
    return stream.str();
}

void CleanOne(const std::string& path,
              const std::vector<std::string>& roots,
              const std::vector<GlobPattern>& include,
              const std::vector<GlobPattern>& exclude, uint64_t minAge,
              CleanReport& report) {
    if (task::IsCancelled("clean")) {
        return;
    }
    const std::string name = ToLower(safety::Basename(path));
    if (!IncludeMatches(name, include, exclude)) {
        return;
    }
    const std::optional<FileMeta> meta = GetMeta(path);
    if (!meta.has_value()) {
        ++report.skipped;
        return;
    }
    if (!AgeOk(meta->mtime, minAge, NowUnixSeconds())) {
        return;
    }
    task::AddDone("clean", 1);
    const safety::DeleteOutcome outcome =
        safety::DeleteFileChecked(path, roots, true);
    switch (outcome.kind) {
        case safety::DeleteOutcomeKind::Deleted:
            ++report.deleted;
            report.freed += meta->size;
            break;
        case safety::DeleteOutcomeKind::Skipped:
            ++report.skipped;
            break;
        case safety::DeleteOutcomeKind::Failed:
            ++report.skipped;
            if (report.errors.size() < 20) {
                report.errors.push_back(outcome.message);
            }
            break;
    }
}

}  // namespace

Json ScanCategories() {
    const std::vector<Rule> rules = AllRules();
    std::optional<task::Guard> guard;
    try {
        task::Begin("scan_categories", "C盘瘦身扫描", false, false,
                    rules.size(), "项");
        guard.emplace("scan_categories");
        task::SetUnit("scan_categories", "项");
        task::SetProgress("scan_categories", 0, rules.size());
        task::SetStage("scan_categories", "统计清理项");
    } catch (...) {
        // 扫描状态失败不影响结果。
    }

    std::vector<std::optional<CategorySize>> results(rules.size());
    std::atomic<size_t> next{0};
    const size_t threadCount =
        std::min<size_t>(8, std::max<size_t>(1, rules.size()));
    std::vector<std::thread> workers;
    workers.reserve(threadCount);
    for (size_t index = 0; index < threadCount; ++index) {
        workers.emplace_back([&] {
            for (;;) {
                const size_t current = next.fetch_add(1);
                if (current >= rules.size()) {
                    break;
                }
                results[current] = ScanRule(rules[current]);
                task::AddDone("scan_categories", 1);
            }
        });
    }
    for (std::thread& worker : workers) {
        worker.join();
    }

    std::vector<CategorySize> categories;
    categories.reserve(results.size());
    for (std::optional<CategorySize>& item : results) {
        if (item.has_value()) {
            categories.push_back(std::move(*item));
        }
    }
    std::sort(categories.begin(), categories.end(),
              [](const CategorySize& left, const CategorySize& right) {
                  return left.size > right.size;
              });
    if (guard.has_value()) {
        guard->Finish("ok", "统计 " + std::to_string(categories.size()) + " 项");
    }

    Json out = Json::array();
    for (const CategorySize& category : categories) {
        out.push_back(CategoryToJson(category));
    }
    return out;
}

Json CleanCategories(const std::vector<std::string>& keys) {
    CleanReport report;
    try {
        task::Begin("clean", "C盘瘦身清理", true, true, 0, "个文件");
    } catch (const std::exception& error) {
        report.errors.push_back(error.what());
        return ReportToJson(report);
    }
    task::Guard guard("clean");
    task::SetUnit("clean", "个文件");
    task::SetStage("clean", "准备清理项");

    bool cancelled = false;
    const std::vector<Rule> rules = AllRules();
    std::unordered_map<std::string, const Rule*> byKey;
    for (const Rule& rule : rules) {
        byKey[rule.key] = &rule;
    }

    for (const std::string& key : keys) {
        const auto found = byKey.find(key);
        if (found == byKey.end()) {
            report.errors.push_back("找不到清理项：" + key);
            continue;
        }
        const Rule& rule = *found->second;
        if (task::IsCancelled("clean")) {
            cancelled = true;
            break;
        }
        const std::string displayName =
            rule.name.empty() ? key : rule.name;
        task::SetStage("clean", "清理" + displayName);
        const std::string kind = rule.kind.empty() ? "files" : rule.kind;
        if (kind != "files") {
            report.notes.push_back("「" + displayName +
                                   "」需要系统命令，Rust 版暂未迁移，已跳过");
            continue;
        }
        const std::vector<std::string> roots = CollectRoots(rule);
        if (roots.empty()) {
            continue;
        }
        const std::vector<GlobPattern> include = BuildMatcher(rule.patterns);
        const std::vector<GlobPattern> exclude = ExcludeMatcher(rule);
        const uint64_t minAge = rule.min_age_minutes * 60;

        for (const std::string& root : roots) {
            if (task::IsCancelled("clean")) {
                cancelled = true;
                break;
            }
            if (safety::IsReparsePoint(root)) {
                ++report.skipped;
                continue;
            }
            WalkFiles(root, [&](const std::string& path, const FileMeta&) {
                CleanOne(path, roots, include, exclude, minAge, report);
            });
            if (cancelled) {
                break;
            }
            if (rule.prune_dirs && !safety::IsNeverClean(root)) {
                const size_t removed = safety::RemoveEmptyDirs(root);
                if (removed > 0) {
                    report.notes.push_back("移除空目录 " +
                                           std::to_string(removed) + " 个");
                }
            }
        }
        if (cancelled) {
            break;
        }
    }

    if (report.errors.size() > 10) {
        report.errors.resize(10);
    }
    if (cancelled) {
        report.errors.push_back("已取消");
    }
    guard.Finish(cancelled ? "cancelled" : "ok",
                 "删除 " + std::to_string(report.deleted) + " 个文件，释放 " +
                     FormatMb(report.freed) + " MB");
    logs::WriteLog(
        {"开始清理", "清理：C盘瘦身",
         "完成：删除 " + std::to_string(report.deleted) + " 个文件，释放 " +
             std::to_string(report.freed) + " 字节，跳过 " +
             std::to_string(report.skipped) + " 个，待重启 0 个"});
    return ReportToJson(report);
}
