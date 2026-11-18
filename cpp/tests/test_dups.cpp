#include <windows.h>

#include <cstdio>
#include <filesystem>
#include <string>
#include <vector>

#include "core/digest.h"
#include "core/dups.h"
#include "core/filehash.h"
#include "core/hashcache.h"
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
    // $MFT 里保存的是长路径，短路径（ADMINI~1）无法与目录前缀匹配。
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

std::vector<uint8_t> Pattern(size_t size, uint32_t seed) {
    std::vector<uint8_t> data(size);
    uint32_t state = seed * 2654435761u + 1;
    for (size_t index = 0; index < size; ++index) {
        state = state * 1103515245u + 12345u;
        data[index] = static_cast<uint8_t>((state >> 16) & 0xFF);
    }
    return data;
}

void WriteBytes(const std::string& path, const std::vector<uint8_t>& data) {
    FILE* file = nullptr;
    _wfopen_s(&file, Utf8ToWide(path).c_str(), L"wb");
    fwrite(data.data(), 1, data.size(), file);
    fclose(file);
}

bool HasReasonContaining(const Json& report, const std::string& needle) {
    const auto reasons = report.find("reasons");
    if (reasons == report.end() || !reasons->is_array()) {
        return false;
    }
    for (const Json& item : *reasons) {
        if (item.is_string() &&
            item.get<std::string>().find(needle) != std::string::npos) {
            return true;
        }
    }
    return false;
}

}  // namespace

TEST(dups_scan_groups_identical_files_only) {
    const std::string dir =
        TempDir("cdc-dups-scan-" + std::to_string(NowMs()));
    const std::vector<uint8_t> big = Pattern(200 * 1024, 1);
    const std::vector<uint8_t> small = Pattern(3000, 2);
    const std::vector<uint8_t> other = Pattern(200 * 1024, 3);

    WriteBytes(JoinPath(dir, "a.bin"), big);
    WriteBytes(JoinPath(dir, "b.bin"), big);
    WriteBytes(JoinPath(dir, "c.bin"), small);
    WriteBytes(JoinPath(dir, "d.bin"), small);
    WriteBytes(JoinPath(dir, "e.bin"), Pattern(5000, 4));
    WriteBytes(JoinPath(dir, "f.bin"), big);
    WriteBytes(JoinPath(dir, "g.bin"), other);

    dups::InvalidateCache();
    const Json groups = dups::ScanDups(dir, {}, 1, false);

    CHECK_EQ(groups.size(), static_cast<size_t>(2));
    // 按 size * 文件数 从大到小排序：200KB 组在前。
    const Json& first = groups[0];
    CHECK_EQ(first["size"].get<uint64_t>(), static_cast<uint64_t>(200 * 1024));
    CHECK_EQ(first["files"].size(), static_cast<size_t>(3));
    const Json& second = groups[1];
    CHECK_EQ(second["size"].get<uint64_t>(), static_cast<uint64_t>(3000));
    CHECK_EQ(second["files"].size(), static_cast<size_t>(2));

    std::vector<std::string> names;
    for (const Json& item : first["files"]) {
        names.push_back(Basename(item.get<std::string>()));
    }
    CHECK_EQ(names[0], std::string("a.bin"));
    CHECK_EQ(names[1], std::string("b.bin"));
    CHECK_EQ(names[2], std::string("f.bin"));

    // 同尺寸但内容不同的 g.bin 不能被并进任何一组。
    for (const Json& group : groups) {
        for (const Json& item : group["files"]) {
            CHECK(Basename(item.get<std::string>()) != "g.bin");
            CHECK(Basename(item.get<std::string>()) != "e.bin");
        }
    }

    std::error_code error;
    std::filesystem::remove_all(std::filesystem::path(Utf8ToWide(dir)), error);
}

TEST(dups_delete_skips_copy_whose_content_changed) {
    const std::string dir =
        TempDir("cdc-dups-delete-" + std::to_string(NowMs()));
    const std::string keeper = JoinPath(dir, "keep.bin");
    const std::string copy = JoinPath(dir, "copy.bin");
    const std::vector<uint8_t> content = Pattern(5000, 7);
    WriteBytes(keeper, content);
    WriteBytes(copy, content);

    dups::InvalidateCache();
    const Json groups = dups::ScanDups(dir, {}, 1, false);
    CHECK_EQ(groups.size(), static_cast<size_t>(1));
    const std::string hash = groups[0]["hash"].get<std::string>();
    const uint64_t size = groups[0]["size"].get<uint64_t>();

    uint64_t mtime = 0;
    {
        WIN32_FILE_ATTRIBUTE_DATA data = {};
        CHECK(GetFileAttributesExW(Utf8ToWide(copy).c_str(), GetFileExInfoStandard,
                                   &data) != 0);
        mtime = FileMtimeSeconds(copy);
        WriteBytes(copy, Pattern(5000, 8));
        // 还原修改时间，让「大小/时间」预检通过，专门验证内容复核。
        const HANDLE handle =
            CreateFileW(Utf8ToWide(copy).c_str(), FILE_WRITE_ATTRIBUTES,
                        FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr,
                        OPEN_EXISTING, 0, nullptr);
        CHECK(handle != INVALID_HANDLE_VALUE);
        CHECK(SetFileTime(handle, nullptr, nullptr, &data.ftLastWriteTime) != 0);
        CloseHandle(handle);
    }
    CHECK_EQ(FileMtimeSeconds(copy), mtime);

    Json items = Json::array();
    items.push_back(Json{{"path", copy},
                         {"hash", hash},
                         {"size", size},
                         {"mtime", mtime},
                         {"keeper", keeper}});
    const Json report = dups::DeleteDups(items);
    CHECK_EQ(report["deleted"].get<uint64_t>(), static_cast<uint64_t>(0));
    CHECK(PathExists(copy));
    CHECK(HasReasonContaining(report, "内容与保留文件不一致，已跳过"));

    std::error_code error;
    std::filesystem::remove_all(std::filesystem::path(Utf8ToWide(dir)), error);
}

TEST(dups_delete_rejects_protected_path) {
    Json items = Json::array();
    items.push_back(Json{{"path", "C:\\Windows\\System32\\kernel32.dll"},
                         {"hash", hashcache::DigestToHex(Blake3Hash("x", 1))},
                         {"size", 1},
                         {"mtime", 0},
                         {"keeper", ""}});
    const Json report = dups::DeleteDups(items);
    CHECK_EQ(report["deleted"].get<uint64_t>(), static_cast<uint64_t>(0));
    CHECK(HasReasonContaining(report, "受保护路径或密码库，已跳过"));
}
