#include <windows.h>
#include <shlobj.h>

#include <cstdio>
#include <cstdint>
#include <filesystem>
#include <string>
#include <vector>

#include "core/im.h"
#include "core/system.h"
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

void WriteBytes(const std::string& path, const std::string& data) {
    FILE* file = nullptr;
    _wfopen_s(&file, Utf8ToWide(path).c_str(), L"wb");
    CHECK(file != nullptr);
    fwrite(data.data(), 1, data.size(), file);
    fclose(file);
}

std::string DocumentsDir() {
    wchar_t buffer[260] = {};
    if (SUCCEEDED(SHGetFolderPathW(nullptr, CSIDL_PERSONAL, nullptr, 0,
                                   buffer))) {
        const std::string path = WideToUtf8(buffer);
        if (!path.empty()) {
            return path;
        }
    }
    return GetEnv("USERPROFILE") + "\\Documents";
}

bool HasRoot(const std::vector<std::string>& roots, const std::string& value) {
    for (const std::string& root : roots) {
        if (root == value) {
            return true;
        }
    }
    return false;
}

}  // namespace

TEST(im_classifies_by_directory_before_extension) {
    const std::string root = "C:\\Users\\u\\Documents\\WeChat Files";
    // 接收文件目录里的视频仍算「接收文件」。
    const auto file = im::Classify(
        "C:\\Users\\u\\Documents\\WeChat Files\\wxid\\FileStorage\\File\\2024-01\\a.mp4",
        root);
    CHECK(file.has_value());
    CHECK_EQ(file->first, std::string("file"));
    // 缓存目录里的图片算缓存。
    const auto cache = im::Classify(
        "C:\\Users\\u\\Documents\\WeChat Files\\wxid\\FileStorage\\Cache\\a.jpg",
        root);
    CHECK(cache.has_value());
    CHECK_EQ(cache->first, std::string("cache"));
}

TEST(im_video_cover_images_are_not_counted_as_video) {
    const std::string root = "C:\\Users\\u\\Documents\\WeChat Files";
    const auto cover = im::Classify(
        "C:\\Users\\u\\Documents\\WeChat Files\\wxid\\FileStorage\\Video\\2025-09\\0483e1f2.jpg",
        root);
    CHECK(cover.has_value());
    CHECK_EQ(cover->first, std::string("image"));
    const auto video = im::Classify(
        "C:\\Users\\u\\Documents\\WeChat Files\\wxid\\FileStorage\\Video\\2025-09\\0483e1f2.mp4",
        root);
    CHECK(video.has_value());
    CHECK_EQ(video->first, std::string("video"));
}

TEST(im_skips_databases_and_config) {
    const std::string root = "C:\\Users\\u\\Documents\\WeChat Files";
    CHECK(!im::Classify(
               "C:\\Users\\u\\Documents\\WeChat Files\\wxid\\Msg\\a.db", root)
               .has_value());
    CHECK(!im::Classify(
               "C:\\Users\\u\\Documents\\WeChat Files\\wxid\\db_storage\\a.dat",
               root)
               .has_value());
}

TEST(im_classifies_miniprogram_assets_as_cache) {
    const std::string root = GetEnv("APPDATA") + "\\Tencent\\xwechat";
    const auto result = im::Classify(
        root +
            "\\radium\\users\\h\\xworker\\liteapp\\local\\packages\\p\\main\\174"
            "\\dist\\pkg\\res\\img\\lucky-bag-card-0.png",
        root);
    CHECK(result.has_value());
    CHECK_EQ(result->first, std::string("cache"));
}

TEST(im_classifies_client_icons_as_cache_not_chat_images) {
    const std::string root = GetEnv("APPDATA") + "\\Tencent\\xwechat";
    const auto result = im::Classify(
        root +
            "\\xplugin\\Plugins\\WinWeixinShare\\2004\\extracted\\Assets\\"
            "WeChat150.png",
        root);
    CHECK(result.has_value());
    CHECK_EQ(result->first, std::string("cache"));
}

TEST(im_classifies_plain_media_without_directory_hint) {
    const std::string root = "C:\\Users\\u\\Documents\\WeChat Files";
    const auto document =
        im::Classify(root + "\\x\\a.pdf", root);
    CHECK(document.has_value());
    CHECK_EQ(document->first, std::string("file"));
    CHECK_EQ(document->second, std::string("doc"));
    CHECK(!im::Classify(root + "\\x\\a.unknownext", root).has_value());
}

TEST(im_migration_roots_include_default_candidates) {
    const std::string expected = DocumentsDir() + "\\Tencent Files";
    CHECK(HasRoot(im::MigrationRoots("qq"), expected));
    CHECK(HasRoot(im::MigrationRoots("qq"), GetEnv("APPDATA") + "\\Tencent\\QQ"));
    CHECK(HasRoot(im::MigrationRoots("wechat"),
                  GetEnv("APPDATA") + "\\Tencent\\WeChat"));
}

TEST(im_preview_reads_text_and_flags_binary) {
    const std::string dir =
        TempDir("cdc-preview-" + std::to_string(NowMs()));
    const std::string textPath = JoinPath(dir, "note.txt");
    WriteBytes(textPath, "hello 世界\n");
    const Json text = im::Preview(textPath);
    CHECK_EQ(text.value("text", std::string()), std::string("hello 世界\n"));
    CHECK(!text.value("binary", true));
    CHECK(!text.value("truncated", true));
    CHECK_EQ(text.value("size", 0ULL), static_cast<uint64_t>(13));

    const std::string binaryPath = JoinPath(dir, "blob.bin");
    WriteBytes(binaryPath, std::string(64, '\x01'));
    const Json binary = im::Preview(binaryPath);
    CHECK(binary.value("binary", false));
    CHECK(binary.value("text", std::string()).find("01 01 01") !=
          std::string::npos);

    const std::string bigPath = JoinPath(dir, "big.log");
    WriteBytes(bigPath, std::string(100 * 1024, 'a'));
    const Json big = im::Preview(bigPath);
    CHECK(big.value("truncated", false));
    CHECK_EQ(big.value("size", 0ULL), static_cast<uint64_t>(100 * 1024));
    CHECK_EQ(big.value("text", std::string()).size(),
             static_cast<size_t>(64 * 1024));

    bool threw = false;
    try {
        im::Preview(JoinPath(dir, "missing.txt"));
    } catch (const std::exception&) {
        threw = true;
    }
    CHECK(threw);

    std::error_code removeError;
    std::filesystem::remove_all(std::filesystem::path(Utf8ToWide(dir)),
                                removeError);
}

TEST(im_thumbnail_decodes_wechat_dat) {
    const std::string dir =
        TempDir("cdc-thumb-" + std::to_string(NowMs()));
    std::vector<uint8_t> jpeg = {0xFF, 0xD8, 0xFF, 0xE0};
    for (int index = 0; index < 28; ++index) {
        jpeg.push_back(static_cast<uint8_t>(index));
    }
    const std::string plain = JoinPath(dir, "plain.jpg");
    {
        FILE* file = nullptr;
        _wfopen_s(&file, Utf8ToWide(plain).c_str(), L"wb");
        CHECK(file != nullptr);
        fwrite(jpeg.data(), 1, jpeg.size(), file);
        fclose(file);
    }
    const auto direct = im::Thumbnail(plain);
    CHECK(direct.has_value());
    CHECK_EQ(direct->first, std::string("image/jpeg"));
    CHECK(direct->second == jpeg);

    std::vector<uint8_t> encrypted = jpeg;
    for (uint8_t& byte : encrypted) {
        byte ^= 0x05;
    }
    const std::string dat = JoinPath(dir, "secret.dat");
    {
        FILE* file = nullptr;
        _wfopen_s(&file, Utf8ToWide(dat).c_str(), L"wb");
        CHECK(file != nullptr);
        fwrite(encrypted.data(), 1, encrypted.size(), file);
        fclose(file);
    }
    const auto decoded = im::Thumbnail(dat);
    CHECK(decoded.has_value());
    CHECK_EQ(decoded->first, std::string("image/jpeg"));
    CHECK(decoded->second == jpeg);

    // 非图片返回 nullopt；扫描结果白名单只放行 remember 过的路径。
    CHECK(!im::Thumbnail(JoinPath(dir, "missing.jpg")).has_value());
    CHECK(!im::IsScanned(plain));
    im::Record record;
    record.path = plain;
    im::RememberScanned({record});
    CHECK(im::IsScanned(plain));

    std::error_code removeError;
    std::filesystem::remove_all(std::filesystem::path(Utf8ToWide(dir)),
                                removeError);
}

TEST(sys_query_hiber_returns_valid_struct) {
    const Json info = sysclean::QueryHiber();
    const std::string path = info.value("path", std::string());
    CHECK(path.size() > 12);
    CHECK_EQ(path.substr(path.size() - 12), std::string("hiberfil.sys"));
    CHECK(info.contains("enabled"));
    CHECK(info.contains("description"));
}

TEST(sys_scan_system_deep_returns_items) {
    const Json items = sysclean::ScanSystem();
    CHECK(!items.empty());
    bool hasDism = false;
    for (const Json& item : items) {
        if (item.value("key", std::string()) == "sys_dism_winsxs") {
            hasDism = true;
        }
    }
    CHECK(hasDism);
}

TEST(sys_migration_rejects_non_im_source) {
    bool threw = false;
    try {
        sysclean::ExecuteImMigration("D:\\cdc_should_not_exist", "D:\\cdc-target",
                                     "wechat");
    } catch (const std::exception&) {
        threw = true;
    }
    CHECK(threw);
    CHECK(!PathExists("D:\\cdc_should_not_exist"));
}

TEST(sys_copy_dir_recursive_matches_source_size) {
    const std::string base =
        TempDir("cdc-copy-" + std::to_string(NowMs()));
    const std::string src = JoinPath(base, "src");
    const std::string dst = JoinPath(base, "dst");
    CreateDirectoryW(Utf8ToWide(src).c_str(), nullptr);
    CreateDirectoryW(Utf8ToWide(JoinPath(src, "sub")).c_str(), nullptr);
    WriteBytes(JoinPath(src, "a.txt"), "hello");
    WriteBytes(JoinPath(JoinPath(src, "sub"), "b.txt"), "world!");

    std::string error;
    CHECK(sysclean::CopyDirectory(src, dst, error));
    CHECK(sysclean::DirectorySizeCount(src) == sysclean::DirectorySizeCount(dst));
    std::string detail;
    CHECK(sysclean::VerifyDirectoryCopy(src, dst, detail));
    WriteBytes(JoinPath(JoinPath(dst, "sub"), "b.txt"), "w");
    CHECK(!sysclean::VerifyDirectoryCopy(src, dst, detail));

    std::error_code removeError;
    std::filesystem::remove_all(std::filesystem::path(Utf8ToWide(base)),
                                removeError);
}

TEST(sys_clean_dir_contents_empties_dir_but_keeps_root) {
    const std::string base =
        TempDir("cdc-clean-" + std::to_string(NowMs()));
    CreateDirectoryW(Utf8ToWide(JoinPath(base, "nested")).c_str(), nullptr);
    WriteBytes(JoinPath(base, "a.bin"), "12345");
    WriteBytes(JoinPath(JoinPath(base, "nested"), "b.bin"), "678");

    const auto [freed, count] = sysclean::CleanDirectoryContents(base);
    CHECK_EQ(freed, static_cast<uint64_t>(8));
    CHECK_EQ(count, static_cast<uint64_t>(2));
    CHECK(PathExists(base));
    CHECK(!PathExists(JoinPath(base, "nested")));

    std::error_code removeError;
    std::filesystem::remove_all(std::filesystem::path(Utf8ToWide(base)),
                                removeError);
}

TEST(sys_recovery_paths_require_exact_allowed_source) {
    const std::string source = "C:\\Users\\u\\Documents\\WeChat Files";
    const std::vector<std::string> roots{source};
    CHECK(sysclean::ValidRecoveryPaths(
        source, "C:\\Users\\u\\Documents\\WeChat Files_backup_20261116_200000",
        roots));
    CHECK(!sysclean::ValidRecoveryPaths(
        "C:\\Users\\u\\Documents\\WeChat Files\\nested",
        "C:\\Users\\u\\Documents\\WeChat Files\\nested_backup_20261116_200000",
        roots));
    CHECK(!sysclean::ValidRecoveryPaths(
        source, "D:\\elsewhere\\WeChat Files_backup_20261116_200000", roots));
    CHECK(!sysclean::ValidRecoveryPaths(
        source, "C:\\Users\\u\\Documents\\WeChat Files_backup_20261116-200000",
        roots));
    CHECK(!sysclean::ValidRecoveryPaths(
        "C:\\Windows\\System32", "C:\\Windows\\System32_backup_20261116_200000",
        roots));
}

TEST(sys_cmd_quoted_paths_reject_expansion) {
    CHECK(sysclean::SafeForQuotedCmdPath("D:\\Data folder\\WeChat"));
    CHECK(sysclean::SafeForQuotedCmdPath("D:\\Data & files\\WeChat"));
    CHECK(!sysclean::SafeForQuotedCmdPath("D:\\Data%TEMP%\\WeChat"));
    CHECK(!sysclean::SafeForQuotedCmdPath("D:\\Data\\\"quoted\""));
    CHECK(!sysclean::SafeForQuotedCmdPath("D:\\Data\\line\nnext"));
}
