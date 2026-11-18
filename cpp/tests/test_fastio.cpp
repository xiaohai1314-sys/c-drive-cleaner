#include <windows.h>

#include <atomic>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <string>
#include <vector>

#include "core/digest.h"
#include "core/fastio.h"
#include "harness.hpp"
#include "util/env.h"
#include "util/path.h"
#include "util/utf.h"

namespace {

void RemoveTree(const std::string& path) {
    std::error_code error;
    std::filesystem::remove_all(std::filesystem::path(Utf8ToWide(path)), error);
}

std::string TempDir(const char* name) {
    std::string base = GetEnv("TEMP");
    if (base.empty()) {
        base = "C:\\Windows\\Temp";
    }
    wchar_t longPath[32768] = {};
    if (GetLongPathNameW(Utf8ToWide(base).c_str(), longPath, 32768) != 0) {
        base = WideToUtf8(longPath);
    }
    const std::string dir = JoinPath(base, name);
    RemoveTree(dir);
    CreateDirectoryW(Utf8ToWide(dir).c_str(), nullptr);
    return dir;
}

void WriteBytes(const std::string& path, const std::vector<uint8_t>& data) {
    FILE* file = nullptr;
    _wfopen_s(&file, Utf8ToWide(path).c_str(), L"wb");
    fwrite(data.data(), 1, data.size(), file);
    fclose(file);
}

}  // namespace

TEST(fastio_open_by_id_and_iocp_match_direct_read) {
    const std::string dir = TempDir("cdc-fastio-test");
    const std::string path1 = JoinPath(dir, "a.bin");
    const std::string path2 = JoinPath(dir, "b.bin");

    std::vector<uint8_t> data1(8192);
    std::vector<uint8_t> data2(8192);
    for (size_t index = 0; index < 8192; ++index) {
        data1[index] = static_cast<uint8_t>(index % 251);
        data2[index] = static_cast<uint8_t>((index * 7) % 241);
    }
    WriteBytes(path1, data1);
    WriteBytes(path2, data2);

    const std::optional<fastio::OpenedFile> direct = fastio::OpenCheckedFileFast(
        nullptr, 0, path1, FILE_FLAG_RANDOM_ACCESS);
    CHECK(direct.has_value());
    CHECK_EQ(direct->size, static_cast<uint64_t>(8192));

    uint64_t frn = 0;
    {
        HANDLE handle =
            CreateFileW(Utf8ToWide(path1).c_str(), FILE_READ_ATTRIBUTES,
                        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                        nullptr, OPEN_EXISTING, 0, nullptr);
        CHECK(handle != INVALID_HANDLE_VALUE);
        BY_HANDLE_FILE_INFORMATION info = {};
        CHECK(GetFileInformationByHandle(handle, &info) != 0);
        frn = (static_cast<uint64_t>(info.nFileIndexHigh) << 32) |
              static_cast<uint64_t>(info.nFileIndexLow);
        CloseHandle(handle);
    }
    CHECK(frn != 0);

    const char drive = path1.empty() ? 'C' : path1[0];
    UniqueHandle hint = fastio::OpenDriveHint(drive);
    CHECK(hint.get() != nullptr && hint.get() != INVALID_HANDLE_VALUE);

    const std::optional<fastio::OpenedFile> byId = fastio::OpenCheckedFileFast(
        hint.get(), frn, path1, FILE_FLAG_RANDOM_ACCESS);
    CHECK(byId.has_value());
    std::vector<uint8_t> head(4096);
    DWORD read = 0;
    CHECK(ReadFile(byId->handle.get(), head.data(), 4096, &read, nullptr) != 0);
    CHECK_EQ(read, static_cast<DWORD>(4096));
    CHECK(std::memcmp(head.data(), data1.data(), 4096) == 0);

    std::atomic<bool> cancel{false};
    std::atomic<size_t> ticks{0};
    std::vector<fastio::IocpHeadRequest> requests;
    requests.push_back({0, path1, frn, 4096});
    requests.push_back({1, path2, 0, 4096});
    const std::vector<fastio::IocpHeadResult> results = fastio::IocpReadHeads(
        std::move(requests), 4, cancel, [&ticks] { ticks.fetch_add(1); });

    CHECK_EQ(results.size(), static_cast<size_t>(2));
    CHECK_EQ(ticks.load(), static_cast<size_t>(2));
    for (const fastio::IocpHeadResult& result : results) {
        const std::vector<uint8_t>& expected = result.idx == 0 ? data1 : data2;
        CHECK(result.digest == Blake3Hash(expected.data(), 4096));
    }

    RemoveTree(dir);
}
