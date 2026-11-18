#include <windows.h>

#include <cstdio>
#include <filesystem>
#include <string>
#include <unordered_set>
#include <vector>

#include "core/frag.h"
#include "core/logs.h"
#include "core/privacy.h"
#include "core/process.h"
#include "core/shred.h"
#include "core/timeutil.h"
#include "harness.hpp"
#include "util/env.h"
#include "util/path.h"
#include "util/utf.h"

namespace {

std::string TempDir(const std::string& name) {
    std::string base = GetEnv("TEMP");
    if (base.empty()) {
        base = "C:\\Windows\\Temp";
    }
    wchar_t longPath[32768] = {};
    if (GetLongPathNameW(Utf8ToWide(base).c_str(), longPath, 32768) != 0) {
        base = WideToUtf8(longPath);
    }
    const std::string dir = JoinPath(base, name);
    std::error_code error;
    std::filesystem::remove_all(std::filesystem::path(Utf8ToWide(dir)), error);
    CreateDirectoryW(Utf8ToWide(dir).c_str(), nullptr);
    return dir;
}

bool Contains(const std::string& text, const std::string& needle) {
    return text.find(needle) != std::string::npos;
}

}  // namespace

TEST(frag_parses_chinese_report) {
    const std::string text =
        "卷信息:\n  卷大小 = 118.99 GB\n  可用空间 = 15.23 GB\n"
        "  碎片率 = 12%\n  碎片空间 = 0.42 GB\n  碎片文件数 = 1,234\n";
    const frag::ParsedReport parsed = frag::ParseReport(text);
    CHECK(parsed.percent.has_value());
    CHECK_EQ(*parsed.percent, 12.0);
    CHECK(parsed.spaceGb.has_value());
    CHECK_EQ(*parsed.spaceGb, 0.42);
    CHECK(parsed.count.has_value());
    CHECK_EQ(*parsed.count, static_cast<uint64_t>(1234));
    CHECK(parsed.freeGb.has_value());
    CHECK_EQ(*parsed.freeGb, 15.23);
}

TEST(frag_parses_english_report) {
    const std::string text =
        "Percent file fragmentation = 7%\nTotal fragmented space = 0.10 GB\n"
        "Total fragmented files = 42\nFree space = 30.5 GB\n";
    const frag::ParsedReport parsed = frag::ParseReport(text);
    CHECK(parsed.percent.has_value());
    CHECK_EQ(*parsed.percent, 7.0);
    CHECK(parsed.spaceGb.has_value());
    CHECK_EQ(*parsed.spaceGb, 0.10);
    CHECK(parsed.count.has_value());
    CHECK_EQ(*parsed.count, static_cast<uint64_t>(42));
    CHECK(parsed.freeGb.has_value());
    CHECK_EQ(*parsed.freeGb, 30.5);
}

TEST(frag_missing_fields_stay_none) {
    const frag::ParsedReport parsed = frag::ParseReport("没有可用的分析数据");
    CHECK(!parsed.percent.has_value());
    CHECK(!parsed.count.has_value());
    CHECK(!parsed.spaceGb.has_value());
    CHECK(!parsed.freeGb.has_value());
}

TEST(process_lists_running_processes) {
    const std::unordered_set<std::string> processes = process::RunningProcesses();
    CHECK(!processes.empty());
    // 当前测试进程一定在列表里。
    CHECK(processes.find("cdc_tests.exe") != processes.end());
    const std::vector<std::string> found =
        process::CheckRunning({"cdc_tests.exe", "definitely-not-running.exe"});
    CHECK_EQ(found.size(), static_cast<size_t>(1));
    CHECK_EQ(found[0], std::string("cdc_tests.exe"));
}

TEST(shred_shreds_a_temp_file) {
    const std::string dir =
        TempDir("cdc-shred-test-" + std::to_string(NowMs()));
    const std::string file = JoinPath(dir, "secret.bin");
    {
        FILE* handle = nullptr;
        _wfopen_s(&handle, Utf8ToWide(file).c_str(), L"wb");
        CHECK(handle != nullptr);
        const std::vector<uint8_t> data(4096, 7);
        fwrite(data.data(), 1, data.size(), handle);
        fclose(handle);
    }
    const Json report = shred::ShredPaths({file}, 1);
    CHECK_EQ(report["files_ok"].get<uint64_t>(), static_cast<uint64_t>(1));
    CHECK_EQ(report["files_failed"].get<uint64_t>(), static_cast<uint64_t>(0));
    CHECK_EQ(report["freed"].get<uint64_t>(), static_cast<uint64_t>(4096));
    CHECK(!PathExists(file));

    std::error_code error;
    std::filesystem::remove_all(std::filesystem::path(Utf8ToWide(dir)), error);
}

TEST(shred_refuses_protected_paths) {
    const Json report = shred::ShredPaths(
        {"C:\\Windows\\System32\\kernel32.dll"}, 1);
    CHECK_EQ(report["files_ok"].get<uint64_t>(), static_cast<uint64_t>(0));
    CHECK(report["files_failed"].get<uint64_t>() >= 1);
    CHECK(PathExists("C:\\Windows\\System32\\kernel32.dll"));
}

TEST(privacy_registry_backup_writes_reg_file) {
    const std::string keyPath = "Software\\CdcPrivacyBackupTest";
    HKEY key = nullptr;
    CHECK(RegCreateKeyExW(HKEY_CURRENT_USER, L"Software\\CdcPrivacyBackupTest",
                          0, nullptr, 0, KEY_ALL_ACCESS, nullptr, &key,
                          nullptr) == ERROR_SUCCESS);
    const wchar_t probe[] = L"probe";
    CHECK(RegSetValueExW(key, L"CdcProbeValue", 0, REG_SZ,
                         reinterpret_cast<const BYTE*>(probe),
                         sizeof(probe)) == ERROR_SUCCESS);
    RegCloseKey(key);

    CHECK(privacy::BackupRegistryKey("HKCU", keyPath, "cdctest"));

    const std::string regDir =
        JoinPath(JoinPath(logs::LogDir(), "backups"), "reg");
    WIN32_FIND_DATAW data;
    const HANDLE find = FindFirstFileW(
        Utf8ToWide(regDir + "\\cdctest_*.reg").c_str(), &data);
    CHECK(find != INVALID_HANDLE_VALUE);
    uint64_t newest = 0;
    do {
        const uint64_t size =
            (static_cast<uint64_t>(data.nFileSizeHigh) << 32) |
            static_cast<uint64_t>(data.nFileSizeLow);
        if (size > newest) {
            newest = size;
        }
    } while (FindNextFileW(find, &data) != 0);
    FindClose(find);
    CHECK(newest > 0);

    RegDeleteTreeW(HKEY_CURRENT_USER, L"Software\\CdcPrivacyBackupTest");
}

TEST(privacy_sql_statements_cover_both_browsers) {
    const std::vector<std::string> history = privacy::StatementsFor("history");
    bool hasUrls = false;
    bool hasMozPlaces = false;
    for (const std::string& statement : history) {
        hasUrls = hasUrls || Contains(statement, "urls");
        hasMozPlaces = hasMozPlaces || Contains(statement, "moz_places");
    }
    CHECK(hasUrls);
    CHECK(hasMozPlaces);
    bool hasMozCookies = false;
    for (const std::string& statement : privacy::StatementsFor("cookies")) {
        hasMozCookies = hasMozCookies || Contains(statement, "moz_cookies");
    }
    CHECK(hasMozCookies);
}

TEST(privacy_shop_sql_only_targets_fixed_domains) {
    const std::vector<std::string> sql = privacy::StatementsFor("shop");
    CHECK(!sql.empty());
    for (const std::string& statement : sql) {
        CHECK(Contains(statement, "taobao.com"));
    }
    bool hasQuote = false;
    for (const std::string& statement : sql) {
        hasQuote = hasQuote || Contains(statement, "'");
    }
    CHECK(hasQuote);
}
