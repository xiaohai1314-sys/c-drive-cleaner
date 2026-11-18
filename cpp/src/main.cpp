#include <windows.h>
#include <shellapi.h>

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <vector>

#include "app/window.h"
#include "core/commands/registry.h"
#include "core/frag.h"
#include "core/hashcache.h"
#include "core/im.h"
#include "core/mft.h"
#include "core/privacy.h"
#include "core/system.h"
#include "core/timeutil.h"
#include "util/path.h"
#include "util/utf.h"
#include "util/win32.h"

namespace {

bool HasArgument(int argc, wchar_t** argv, const wchar_t* value) {
    for (int index = 1; index < argc; ++index) {
        if (wcscmp(argv[index], value) == 0) {
            return true;
        }
    }
    return false;
}

void AttachConsoleIfNeeded() {
    const HANDLE existing = GetStdHandle(STD_OUTPUT_HANDLE);
    if (existing != nullptr && existing != INVALID_HANDLE_VALUE) {
        // 从终端重定向运行时直接复用继承的 stdout。
        SetConsoleOutputCP(CP_UTF8);
        return;
    }
    if (AttachConsole(ATTACH_PARENT_PROCESS) == 0) {
        AllocConsole();
    }
    FILE* stream = nullptr;
    freopen_s(&stream, "CONOUT$", "w", stdout);
    freopen_s(&stream, "CONOUT$", "w", stderr);
    SetConsoleOutputCP(CP_UTF8);
}

std::string NumberText(const Json& value, const char* format) {
    if (value.is_null()) {
        return "None";
    }
    char buffer[64];
    std::snprintf(buffer, sizeof(buffer), format, value.get<double>());
    return buffer;
}

std::string CountText(const Json& value) {
    if (value.is_null()) {
        return "None";
    }
    return std::to_string(value.get<uint64_t>());
}

std::string BoolText(const Json& value) {
    if (value.is_null()) {
        return "None";
    }
    return value.get<bool>() ? "true" : "false";
}

double SecondsSince(const std::chrono::steady_clock::time_point& start) {
    return std::chrono::duration<double>(std::chrono::steady_clock::now() -
                                         start)
        .count();
}

int RunSelfTest() {
    AttachConsoleIfNeeded();
    const auto totalStart = std::chrono::steady_clock::now();

    std::printf("disk: ");
    try {
        const Json info = commands::Dispatch("disk_info", Json::object());
        std::printf("total=%llu used=%llu free=%llu\n",
                    static_cast<unsigned long long>(info["total"].get<uint64_t>()),
                    static_cast<unsigned long long>(info["used"].get<uint64_t>()),
                    static_cast<unsigned long long>(info["free"].get<uint64_t>()));
    } catch (const std::exception& ex) {
        std::printf("失败 %s\n", ex.what());
    }

    const auto categoriesStart = std::chrono::steady_clock::now();
    const Json categories = commands::Dispatch("scan_categories", Json::object());
    long long categoryTotal = 0;
    for (const Json& category : categories) {
        const long long size = category.value("size", 0LL);
        if (size > 0) {
            categoryTotal += size;
        }
    }
    std::printf("categories: %zu 条, 合计 %lld 字节, 用时 %.1fs\n",
                categories.size(), categoryTotal, SecondsSince(categoriesStart));
    if (const char* dump = std::getenv("CDC_DUMP_CATEGORIES");
        dump != nullptr && *dump != '\0') {
        if (FILE* out = std::fopen(dump, "wb")) {
            for (const Json& category : categories) {
                const std::string line =
                    category.value("key", std::string()) + "\t" +
                    std::to_string(category.value("size", 0LL)) + "\t" +
                    std::to_string(category.value("count", 0ULL)) + "\n";
                std::fwrite(line.data(), 1, line.size(), out);
            }
            std::fclose(out);
        }
    }

    const auto bigStart = std::chrono::steady_clock::now();
    const Json big = commands::Dispatch("scan_big", Json::object());
    std::printf("big: %zu 个, 用时 %.1fs\n", big.size(), SecondsSince(bigStart));
    if (const char* dump = std::getenv("CDC_DUMP_BIG");
        dump != nullptr && *dump != '\0') {
        if (FILE* out = std::fopen(dump, "wb")) {
            for (const Json& item : big) {
                const std::string line = item.value("path", std::string()) + "\t" +
                                         std::to_string(item.value("size", 0ULL)) +
                                         "\t" +
                                         std::to_string(item.value("mtime", 0ULL)) +
                                         "\n";
                std::fwrite(line.data(), 1, line.size(), out);
            }
            std::fclose(out);
        }
    }

    const char* dupsDump = std::getenv("CDC_DUMP_DUPS");
    const bool dumpDups = dupsDump != nullptr && *dupsDump != '\0';
    const auto dupsStart = std::chrono::steady_clock::now();
    Json dupsArgs;
    {
        const char* rootEnv = std::getenv("CDC_DUMP_DUPS_ROOT");
        const char* minEnv = std::getenv("CDC_DUMP_DUPS_MIN");
        dupsArgs["root"] =
            (rootEnv != nullptr && *rootEnv != '\0') ? rootEnv : "C:\\";
        if (minEnv != nullptr && *minEnv != '\0') {
            dupsArgs["minSize"] = std::strtoull(minEnv, nullptr, 10);
        } else {
            // 与 Rust 的 scan_dups(.., None, ..) 一致：默认 1MB。
            dupsArgs["minSize"] = dumpDups ? 1ULL : (1ULL << 20);
        }
    }
    const Json dups = commands::Dispatch("scan_dups", dupsArgs);
    std::printf("dups: %zu 组, 用时 %.1fs\n", dups.size(), SecondsSince(dupsStart));
    if (dumpDups) {
        if (FILE* out = std::fopen(dupsDump, "wb")) {
            for (const Json& group : dups) {
                std::string line = group.value("hash", std::string()) + "\t" +
                                   std::to_string(group.value("size", 0ULL)) + "\t";
                bool first = true;
                for (const Json& file : group["files"]) {
                    if (!first) {
                        line += "|";
                    }
                    line += file.get<std::string>();
                    first = false;
                }
                line += "\n";
                std::fwrite(line.data(), 1, line.size(), out);
            }
            std::fclose(out);
        }
        mft::WaitForBackgroundWrites(120000);
        return 0;
    }
    if (const char* dump = std::getenv("CDC_DUMP_HASHCACHE");
        dump != nullptr && *dump != '\0') {
        const std::vector<std::string> lines =
            hashcache::DumpAll(hashcache::DefaultDbPath());
        std::printf("hashcache: %zu 条\n", lines.size());
        if (FILE* out = std::fopen(dump, "wb")) {
            for (const std::string& line : lines) {
                std::fwrite(line.data(), 1, line.size(), out);
                std::fputc('\n', out);
            }
            std::fclose(out);
        }
        return 0;
    }

    const auto fragStart = std::chrono::steady_clock::now();
    const Json frag = frag::Analyze("C:\\");
    std::printf(
        "frag: 碎片率=%s 碎片数=%s 碎片空间=%s 可用=%s 固态=%s, 用时 %.1fs\n",
        NumberText(frag["percent"], "%.1f").c_str(),
        CountText(frag["count"]).c_str(),
        NumberText(frag["space_gb"], "%.2f").c_str(),
        NumberText(frag["free_gb"], "%.2f").c_str(),
        BoolText(frag["ssd"]).c_str(), SecondsSince(fragStart));

    const auto imStart = std::chrono::steady_clock::now();
    const size_t wechat = im::ListMedia(im::Roots("wechat")).size();
    const size_t qq = im::ListMedia(im::Roots("qq")).size();
    std::printf("im: 微信 %zu 个文件, QQ %zu 个文件, 用时 %.1fs\n", wechat, qq,
                SecondsSince(imStart));

    const auto privacyStart = std::chrono::steady_clock::now();
    const Json privacy = privacy::Scan();
    size_t withData = 0;
    for (const Json& item : privacy) {
        if (item.value("count", 0LL) > 0) {
            ++withData;
        }
    }
    std::printf("privacy: %zu 项, 有数据 %zu 项, 用时 %.1fs\n", privacy.size(),
                withData, SecondsSince(privacyStart));
    for (const Json& item : privacy) {
        const long long count = item.value("count", 0LL);
        if (count > 0) {
            std::printf("  privacy: %s = %lld 条\n",
                        item.value("name", std::string()).c_str(), count);
        }
    }

    std::printf("合计用时 %.1fs\n", SecondsSince(totalStart));
    return 0;
}

int RunInvoke(const std::string& command, const Json& args) {
    AttachConsoleIfNeeded();
    try {
        const Json result = commands::Dispatch(command, args);
        std::printf("%s\n", result.dump(2).c_str());
        // 调试 CLI 立即退出会打断后台快照写入，等它们落盘再返回。
        mft::WaitForBackgroundWrites(120000);
        return 0;
    } catch (const std::exception& error) {
        std::printf("{\"error\": %s}\n", Json(error.what()).dump().c_str());
        return 1;
    }
}

// 返回 true 表示继续运行；false 表示已经以管理员身份重启或无法提权。
bool IsElevated() {
    BOOL isAdmin = FALSE;
    SID_IDENTIFIER_AUTHORITY authority = SECURITY_NT_AUTHORITY;
    PSID adminGroup = nullptr;
    if (AllocateAndInitializeSid(&authority, 2, SECURITY_BUILTIN_DOMAIN_RID,
                                 DOMAIN_ALIAS_RID_ADMINS, 0, 0, 0, 0, 0, 0,
                                 &adminGroup) != 0) {
        CheckTokenMembership(nullptr, adminGroup, &isAdmin);
        FreeSid(adminGroup);
    }
    return isAdmin != FALSE;
}

bool EnsureAdmin() {
    if (IsElevated()) {
        return true;
    }
    const std::wstring exe = ExePathW();
    const HINSTANCE result =
        ShellExecuteW(nullptr, L"runas", exe.c_str(), nullptr, nullptr,
                      SW_SHOWNORMAL);
    if (reinterpret_cast<INT_PTR>(result) > 32) {
        return false;
    }
    MessageBoxW(nullptr, L"需要管理员权限才能读取磁盘信息。", L"磁盘清理助手",
                MB_ICONERROR);
    return false;
}

}  // namespace

int WINAPI wWinMain(HINSTANCE instance, HINSTANCE, PWSTR, int showCommand) {
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    const HRESULT com = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED |
                                                    COINIT_DISABLE_OLE1DDE);
    if (FAILED(com)) {
        MessageBoxW(nullptr, L"COM 初始化失败", L"磁盘清理助手", MB_ICONERROR);
        return 1;
    }

    int argc = 0;
    wchar_t** argv = CommandLineToArgvW(GetCommandLineW(), &argc);
    const bool selfTest = argv != nullptr && HasArgument(argc, argv, L"--selftest");
    std::wstring invokeCommand;
    std::wstring invokeArgs;
    if (argv != nullptr) {
        for (int index = 1; index + 1 < argc; ++index) {
            if (wcscmp(argv[index], L"--invoke") == 0) {
                invokeCommand = argv[index + 1];
                if (index + 2 < argc) {
                    invokeArgs = argv[index + 2];
                }
                break;
            }
        }
        LocalFree(argv);
    }

    commands::RegisterAll();

    if (!invokeCommand.empty()) {
        std::string argsText = WideToUtf8(invokeArgs);
        if (!argsText.empty() && argsText.front() == '@') {
            // 命令行转义容易破坏 JSON，允许从文件读取参数。
            FILE* file = nullptr;
            if (fopen_s(&file, argsText.c_str() + 1, "rb") == 0 &&
                file != nullptr) {
                std::string content;
                char buffer[4096];
                size_t read = 0;
                while ((read = fread(buffer, 1, sizeof(buffer), file)) > 0) {
                    content.append(buffer, read);
                }
                fclose(file);
                argsText = content;
            } else {
                argsText.clear();
            }
        }
        const Json args =
            argsText.empty() ? Json::object()
                             : Json::parse(argsText, nullptr, false);
        if (args.is_discarded()) {
            std::fprintf(stderr, "参数不是合法 JSON: %s\n", argsText.c_str());
            return 2;
        }
        return RunInvoke(WideToUtf8(invokeCommand), args);
    }

    if (selfTest) {
        const int code = RunSelfTest();
        CoUninitialize();
        return code;
    }

    if (!EnsureAdmin()) {
        CoUninitialize();
        return 0;
    }

    sysclean::RecoverPendingMove();

    MainWindow window;
    if (!window.Create(instance, showCommand)) {
        CoUninitialize();
        return 1;
    }
    const int code = window.Run();
    CoUninitialize();
    return code;
}
