#include "core/commands/registry.h"

#include <windows.h>
#include <shellapi.h>

#include <functional>
#include <stdexcept>
#include <string>
#include <unordered_map>

#include "core/bigfiles.h"
#include "core/dups.h"
#include "core/frag.h"
#include "core/im.h"
#include "core/logs.h"
#include "core/privacy.h"
#include "core/process.h"
#include "core/recycle.h"
#include "core/rules.h"
#include "core/scanner.h"
#include "core/scanstate.h"
#include "core/shred.h"
#include "core/system.h"
#include "core/task.h"
#include "util/env.h"
#include "util/path.h"
#include "util/utf.h"
#include "util/win32.h"

namespace {

using Handler = std::function<Json(const Json&)>;

std::unordered_map<std::string, Handler>& Table() {
    static std::unordered_map<std::string, Handler> table;
    return table;
}

std::string ArgString(const Json& args, const char* key) {
    if (!args.is_object()) {
        return {};
    }
    const auto found = args.find(key);
    if (found == args.end() || found->is_null()) {
        return {};
    }
    if (!found->is_string()) {
        throw std::runtime_error(std::string("参数类型错误: ") + key);
    }
    return found->get<std::string>();
}

/// JSON 里的整数既可能是无符号（解析而来）也可能是有符号（本地构造），都要接受。
std::optional<uint64_t> ArgUint(const Json& args, const char* key) {
    if (!args.is_object()) {
        return std::nullopt;
    }
    const auto found = args.find(key);
    if (found == args.end() || found->is_null()) {
        return std::nullopt;
    }
    if (found->is_number_unsigned()) {
        return found->get<uint64_t>();
    }
    if (found->is_number_integer()) {
        const int64_t value = found->get<int64_t>();
        if (value >= 0) {
            return static_cast<uint64_t>(value);
        }
    }
    return std::nullopt;
}

std::vector<std::string> ArgStringList(const Json& args, const char* key) {
    std::vector<std::string> out;
    if (!args.is_object()) {
        return out;
    }
    const auto found = args.find(key);
    if (found == args.end() || !found->is_array()) {
        return out;
    }
    for (const Json& item : *found) {
        if (item.is_string()) {
            out.push_back(item.get<std::string>());
        }
    }
    return out;
}

Json EmptyDeleteReport() {
    return Json{{"freed", 0}, {"deleted", 0}, {"failed", Json::array()},
                {"reasons", Json::array()}};
}

struct DupArgs {
    std::optional<std::string> root;
    std::vector<std::string> drives;
    std::optional<uint64_t> minSize;
    bool fast = false;
    bool turbo = false;
};

DupArgs ParseDupArgs(const Json& args) {
    DupArgs parsed;
    const std::string rootText = ArgString(args, "root");
    if (!rootText.empty()) {
        parsed.root = rootText;
    }
    parsed.drives = ArgStringList(args, "drives");
    if (!args.is_object()) {
        return parsed;
    }
    const char* key = args.contains("minSize") ? "minSize" : "min_size";
    parsed.minSize = ArgUint(args, key);
    const auto found = args.find("fast");
    if (found != args.end() && found->is_boolean()) {
        parsed.fast = found->get<bool>();
    }
    const auto turboFound = args.find("turbo");
    if (turboFound != args.end() && turboFound->is_boolean()) {
        parsed.turbo = turboFound->get<bool>();
    }
    return parsed;
}

Json DeleteReportJson(uint64_t freed, uint64_t deleted,
                      const std::vector<std::string>& failed,
                      const std::vector<std::string>& reasons) {
    return Json{{"freed", freed},
                {"deleted", deleted},
                {"failed", failed},
                {"reasons", reasons}};
}

std::string FormatMbOne(uint64_t bytes) {
    char buffer[64];
    std::snprintf(buffer, sizeof(buffer), "%.1f",
                  static_cast<double>(bytes) / 1048576.0);
    return buffer;
}

Json EmptyCleanReport() {
    return Json{{"freed", 0}, {"deleted", 0}, {"skipped", 0}, {"pending", 0},
                {"errors", Json::array()}, {"notes", Json::array()}};
}

Json DiskInfo() {
    ULARGE_INTEGER freeToCaller{};
    ULARGE_INTEGER total{};
    ULARGE_INTEGER totalFree{};
    if (GetDiskFreeSpaceExW(L"C:\\", &freeToCaller, &total, &totalFree) == 0 ||
        total.QuadPart == 0) {
        throw std::runtime_error("读取磁盘信息失败");
    }
    return Json{{"total", total.QuadPart},
                {"used", total.QuadPart - totalFree.QuadPart},
                {"free", totalFree.QuadPart}};
}

Json ListDrives() {
    Json result = Json::array();
    const DWORD mask = GetLogicalDrives();
    for (int index = 0; index < 26; ++index) {
        if ((mask & (1u << index)) == 0) {
            continue;
        }
        const wchar_t letter = static_cast<wchar_t>(L'A' + index);
        const std::wstring root = {letter, L':', L'\\', L'\0'};
        if (GetDriveTypeW(root.c_str()) == DRIVE_FIXED) {
            result.push_back(std::string(1, static_cast<char>(letter)) + ":\\");
        }
    }
    if (result.empty()) {
        result.push_back("C:\\");
    }
    return result;
}

std::string ExplorerExe() {
    std::string root = GetEnv("SystemRoot");
    if (root.empty()) {
        root = "C:\\Windows";
    }
    while (!root.empty() && root.back() == '\\') {
        root.pop_back();
    }
    return root + "\\explorer.exe";
}

std::string SlashToBackslash(std::string path) {
    for (char& ch : path) {
        if (ch == '/') {
            ch = '\\';
        }
    }
    return path;
}

void RevealPath(const Json& args) {
    const std::string path = ArgString(args, "path");
    if (!PathExists(path)) {
        throw std::runtime_error("文件不存在：" + path);
    }
    const std::wstring command =
        BuildCommandLine(Utf8ToWide(ExplorerExe()), {}) + L" /select,\"" +
        Utf8ToWide(SlashToBackslash(path)) + L"\"";
    const SpawnResult result = SpawnCommandLine(command);
    if (!result.started) {
        throw std::runtime_error("启动资源管理器失败：" +
                                 FormatWin32Error(result.error));
    }
}

void OpenPath(const Json& args) {
    const std::string path = ArgString(args, "path");
    if (!PathExists(path)) {
        throw std::runtime_error("路径不存在：" + path);
    }
    const SpawnResult result = SpawnCommandLine(BuildCommandLine(
        Utf8ToWide(ExplorerExe()), {Utf8ToWide(SlashToBackslash(path))}));
    if (!result.started) {
        throw std::runtime_error("打开路径失败：" +
                                 FormatWin32Error(result.error));
    }
}

Json HistoryRead(const Json& args) {
    const std::string name = ArgString(args, "name");
    if (!logs::ValidLogName(name)) {
        throw std::runtime_error("日志名不合法");
    }
    const std::optional<std::string> text = logs::Read(name);
    if (!text.has_value()) {
        throw std::runtime_error("日志不存在");
    }
    return *text;
}

}  // namespace

namespace commands {

void RegisterAll() {
    auto& table = Table();

    table["disk_info"] = [](const Json&) { return DiskInfo(); };
    table["list_drives"] = [](const Json&) { return ListDrives(); };
    table["user_profile"] = [](const Json&) -> Json {
        const std::string profile = GetEnv("USERPROFILE");
        return profile.empty() ? "C:\\" : profile;
    };
    table["task_status"] = [](const Json&) { return task::StatusJson(); };
    table["cancel_task"] = [](const Json& args) -> Json {
        const std::string id = ArgString(args, "id");
        if (id.empty()) {
            task::CancelAll();
            return true;
        }
        return task::Cancel(id);
    };
    table["scan_status"] = [](const Json&) { return scanstate::StatusJson(); };
    table["cancel_scan"] = [](const Json& args) -> Json {
        const std::string kind = ArgString(args, "kind");
        scanstate::Cancel(kind);
        if (kind.empty() || kind == "dup") {
            dups::CancelScan();
        }
        return nullptr;
    };
    table["history_list"] = [](const Json&) { return logs::List(); };
    table["history_read"] = HistoryRead;
    table["reveal_path"] = [](const Json& args) -> Json {
        RevealPath(args);
        return nullptr;
    };
    table["open_path"] = [](const Json& args) -> Json {
        OpenPath(args);
        return nullptr;
    };
    table["check_processes"] = [](const Json& args) {
        return process::CheckRunning(ArgStringList(args, "targets"));
    };
    table["kill_process"] = [](const Json& args) {
        return process::KillProcessByName(ArgString(args, "name"));
    };

    // 扫描与清理命令（已移植）。
    table["scan_categories"] = [](const Json&) { return ScanCategories(); };
    table["scan_big"] = [](const Json& args) {
        return ScanBig(ArgStringList(args, "drives"));
    };
    table["scan_dups"] = [](const Json& args) {
        const DupArgs parsed = ParseDupArgs(args);
        return dups::ScanDups(parsed.root, parsed.drives, parsed.minSize,
                              parsed.fast, parsed.turbo);
    };
    table["dup_cached_result"] = [](const Json& args) {
        const DupArgs parsed = ParseDupArgs(args);
        return dups::CachedResult(parsed.root, parsed.drives, parsed.minSize,
                                  parsed.fast, parsed.turbo);
    };
    table["delete_recycle"] = [](const Json& args) {
        return recycle::DeleteRecycle(ArgStringList(args, "paths"));
    };
    table["delete_dups"] = [](const Json& args) {
        if (!args.is_object()) {
            return dups::DeleteDups(Json::array());
        }
        const auto found = args.find("items");
        return dups::DeleteDups(found == args.end() ? Json::array() : *found);
    };
    table["clean_categories"] = [](const Json& args) {
        return CleanCategories(ArgStringList(args, "keys"));
    };
    table["frag_analyze"] = [](const Json& args) {
        return frag::Analyze(ArgString(args, "drive"));
    };
    table["frag_optimize"] = [](const Json& args) -> Json {
        frag::Optimize(ArgString(args, "drive"));
        return nullptr;
    };
    table["im_roots"] = [](const Json& args) {
        return im::Roots(ArgString(args, "kind"));
    };
    table["im_scan"] = [](const Json& args) {
        const std::string kind = ArgString(args, "kind");
        std::optional<task::Guard> guard;
        try {
            task::Begin("im_scan", "微信/QQ 文件扫描", false, false, 0, "");
            guard.emplace("im_scan");
        } catch (const std::exception&) {
            // 与 Rust 的 `.ok()` 一致：拿不到任务槽位也照常扫描。
        }
        if (guard.has_value()) {
            task::SetStage("im_scan", "扫描聊天文件");
        }
        const std::vector<im::Record> records = im::ListMedia(im::Roots(kind));
        im::RememberScanned(records);
        if (guard.has_value()) {
            guard->Finish("ok", "发现 " + std::to_string(records.size()) +
                                    " 个文件");
        }
        return im::RecordsToJson(records);
    };
    table["im_preview"] = [](const Json& args) {
        const std::string path = ArgString(args, "path");
        if (!im::IsScanned(path)) {
            throw std::runtime_error("文件不在扫描结果里");
        }
        return im::Preview(path);
    };
    table["im_clean"] = [](const Json& args) -> Json {
        const std::string kind = ArgString(args, "kind");
        const std::vector<std::string> kinds = ArgStringList(args, "kinds");
        std::optional<std::vector<std::string>> paths;
        if (args.is_object()) {
            const auto found = args.find("paths");
            if (found != args.end() && found->is_array()) {
                paths = ArgStringList(args, "paths");
            }
        }
        try {
            task::Begin("im_clean", "微信/QQ 清理", true, true, 0, "个文件");
        } catch (const std::exception& error) {
            logs::WriteLog({"开始清理", "清理：微信/QQ",
                            std::string("完成：删除 0 个，释放 0 字节，失败 1 "
                                        "个")});
            return DeleteReportJson(0, 0, {}, {error.what()});
        }
        task::Guard guard("im_clean");
        task::SetUnit("im_clean", "个文件");
        task::SetStage("im_clean", "清理聊天文件");
        const Json report = im::Clean(kind, kinds, paths);
        const uint64_t deleted = report.value("deleted", 0ULL);
        const uint64_t done = deleted + report["failed"].size();
        task::SetProgress("im_clean", done, done);
        guard.Finish(task::IsCancelled("im_clean") ? "cancelled" : "ok",
                     "删除 " + std::to_string(deleted) + " 个，释放 " +
                         FormatMbOne(report.value("freed", 0ULL)) + " MB");
        return report;
    };
    table["im_move_analyze"] = [](const Json& args) {
        std::optional<task::Guard> guard;
        try {
            task::Begin("im_move_analyze", "分析搬家数据", false, false, 0, "");
            guard.emplace("im_move_analyze");
        } catch (const std::exception&) {
        }
        if (guard.has_value()) {
            task::SetStage("im_move_analyze", "统计数据目录");
        }
        const Json analysis = sysclean::AnalyzeImMigration(ArgString(args, "kind"));
        if (guard.has_value()) {
            guard->Finish("ok",
                          std::to_string(analysis.value("file_count", 0ULL)) +
                              " 个文件");
        }
        return analysis;
    };
    table["im_move_execute"] = [](const Json& args) {
        const std::string source = ArgString(args, "source");
        const std::string targetDir = ArgString(args, "targetDir");
        const std::string kind = ArgString(args, "kind");
        task::Begin("im_move", "微信/QQ 数据搬家", true, true, 0, "");
        task::Guard guard("im_move");
        task::SetStage("im_move", "检查运行状态");
        try {
            const std::string message =
                sysclean::ExecuteImMigration(source, targetDir, kind);
            guard.Finish("ok", message);
            return Json(message);
        } catch (const std::exception& error) {
            guard.Finish(task::IsCancelled("im_move") ? "cancelled" : "failed",
                         error.what());
            throw;
        }
    };
    table["privacy_scan"] = [](const Json&) {
        std::optional<task::Guard> guard;
        try {
            task::Begin("privacy_scan", "隐私痕迹扫描", false, false, 0, "");
            guard.emplace("privacy_scan");
        } catch (const std::exception&) {
            // 与 Rust 的 `.ok()` 一致：拿不到任务槽位也照常扫描。
        }
        if (guard.has_value()) {
            task::SetStage("privacy_scan", "扫描浏览器与系统痕迹");
        }
        const Json items = privacy::Scan();
        if (guard.has_value()) {
            guard->Finish("ok",
                          "发现 " + std::to_string(items.size()) + " 项");
        }
        return items;
    };
    table["privacy_clean"] = [](const Json& args) -> Json {
        const std::vector<std::string> ids = ArgStringList(args, "ids");
        try {
            task::Begin("privacy", "隐私痕迹清理", true, true, 0, "项");
        } catch (const std::exception& error) {
            return Json{{"files", 0},
                        {"sql", 0},
                        {"registry", 0},
                        {"failed", Json::array({error.what()})}};
        }
        task::Guard guard("privacy");
        task::SetUnit("privacy", "项");
        task::SetTotal("privacy", ids.size());
        task::SetStage("privacy", "清理隐私痕迹");
        const Json report = privacy::Clean(ids);
        guard.Finish(task::IsCancelled("privacy") ? "cancelled" : "ok",
                     "文件 " + std::to_string(report.value("files", 0LL)) +
                         " 个，数据库 " +
                         std::to_string(report.value("sql", 0LL)) +
                         " 个，注册表 " +
                         std::to_string(report.value("registry", 0LL)) +
                         " 条");
        return report;
    };
    table["shred"] = [](const Json& args) {
        std::optional<uint32_t> passes;
        if (const std::optional<uint64_t> value = ArgUint(args, "passes")) {
            passes = static_cast<uint32_t>(*value);
        }
        return shred::Shred(ArgStringList(args, "paths"), passes);
    };
    table["system_scan"] = [](const Json&) {
        std::optional<task::Guard> guard;
        try {
            task::Begin("system_scan", "系统项扫描", false, false, 0, "");
            guard.emplace("system_scan");
        } catch (const std::exception&) {
        }
        if (guard.has_value()) {
            task::SetStage("system_scan", "扫描系统缓存");
        }
        const Json items = sysclean::ScanSystem();
        if (guard.has_value()) {
            guard->Finish("ok", "发现 " + std::to_string(items.size()) + " 项");
        }
        return items;
    };
    table["system_clean"] = [](const Json& args) {
        const std::string key = ArgString(args, "key");
        const bool cancellable =
            key == "sys_update_dl" || key == "sys_crash_dumps";
        task::Begin("system", "系统深度瘦身", cancellable, true, 0, "");
        task::Guard guard("system");
        task::SetStage("system", "准备清理");
        try {
            const std::string message = sysclean::CleanSystemItem(key);
            guard.Finish("ok", message);
            return Json(message);
        } catch (const std::exception& error) {
            guard.Finish(task::IsCancelled("system") ? "cancelled" : "failed",
                         error.what());
            throw;
        }
    };
    table["hiber_query"] = [](const Json&) { return sysclean::QueryHiber(); };
    table["hiber_set"] = [](const Json& args) {
        return Json(sysclean::SetHiber(ArgString(args, "mode")));
    };
    table["recycle_empty"] = [](const Json&) -> Json {
        task::Begin("recycle", "清空回收站", false, true, 0, "");
        task::Guard guard("recycle");
        task::SetStage("recycle", "清空所有回收站");
        try {
            sysclean::EmptyRecycleBins();
        } catch (const std::exception& error) {
            guard.Finish(task::IsCancelled("recycle") ? "cancelled" : "failed",
                         error.what());
            throw;
        }
        guard.Finish("ok", "回收站已清空");
        return nullptr;
    };
    table["rules_update"] = [](const Json&) -> Json { return RulesUpdate(); };
    table["rules_rollback"] = [](const Json&) -> Json { return RulesRollback(); };
}

Json Dispatch(const std::string& name, const Json& args) {
    const auto found = Table().find(name);
    if (found == Table().end()) {
        throw std::runtime_error("未知命令: " + name);
    }
    return found->second(args);
}

}  // namespace commands
