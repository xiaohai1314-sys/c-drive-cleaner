#include <windows.h>

#include <cstdio>
#include <filesystem>
#include <string>
#include <vector>

#include "core/mft.h"
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
    // $MFT 里保存的是长路径，短路径（ADMINI~1）无法与目录前缀匹配。
    wchar_t longPath[32768] = {};
    const std::wstring wide = Utf8ToWide(base);
    if (GetLongPathNameW(wide.c_str(), longPath, 32768) != 0) {
        base = WideToUtf8(longPath);
    }
    const std::string dir = JoinPath(base, name);
    RemoveTree(dir);
    CreateDirectoryW(Utf8ToWide(dir).c_str(), nullptr);
    return dir;
}

}  // namespace

TEST(mft_converts_windows_filetime_to_unix) {
    const uint64_t unixSeconds = 1'704'067'200ULL;
    const uint64_t fileTime = (unixSeconds + 11'644'473'600ULL) * 10'000'000ULL;
    CHECK_EQ(mft::FileTimeToUnix(fileTime), unixSeconds);
}

TEST(mft_parses_first_lcn_and_clusters_from_runlist) {
    const uint8_t buffer[] = {0x31, 0x08, 0x45, 0x23, 0x01, 0x00};
    const auto extent = mft::ParseFirstExtent(buffer, sizeof(buffer));
    CHECK_EQ(extent.first, static_cast<uint64_t>(0x012345));
    CHECK_EQ(extent.second, static_cast<uint64_t>(8));

    const uint8_t sparse[] = {0x01, 0x08, 0x00};
    const auto empty = mft::ParseFirstExtent(sparse, sizeof(sparse));
    CHECK_EQ(empty.first, static_cast<uint64_t>(0));
    CHECK_EQ(empty.second, static_cast<uint64_t>(0));
}

TEST(mft_fallback_walk_filters_size_and_skip_top) {
    const std::string dir = TempDir("cdc-mft-fallback-test");
    CreateDirectoryW(Utf8ToWide(JoinPath(dir, "keep")).c_str(), nullptr);
    CreateDirectoryW(Utf8ToWide(JoinPath(dir, "Windows")).c_str(), nullptr);
    CreateDirectoryW(Utf8ToWide(JoinPath(dir, "$Skip")).c_str(), nullptr);

    auto writeFile = [](const std::string& path, size_t size) {
        FILE* file = nullptr;
        fopen_s(&file, path.c_str(), "wb");
        std::vector<char> data(size, 1);
        fwrite(data.data(), 1, data.size(), file);
        fclose(file);
    };
    writeFile(JoinPath(JoinPath(dir, "keep"), "big.bin"), 2048);
    writeFile(JoinPath(JoinPath(dir, "keep"), "small.bin"), 100);
    writeFile(JoinPath(JoinPath(dir, "Windows"), "sys.bin"), 4096);
    writeFile(JoinPath(JoinPath(dir, "$Skip"), "meta.bin"), 4096);

    const std::vector<mft::FileEntry> found =
        mft::FallbackWalkFiles(dir, 1024, {"windows"});
    CHECK_EQ(found.size(), static_cast<size_t>(1));
    CHECK(found[0].path.size() >= 7);
    CHECK(found[0].path.compare(found[0].path.size() - 7, 7, "big.bin") == 0);
    CHECK_EQ(found[0].size, static_cast<uint64_t>(2048));

    RemoveTree(dir);
}

TEST(mft_scan_files_where_drops_rejected_names) {
    const std::string dir = TempDir("cdc-mft-keep-test");
    CreateDirectoryW(Utf8ToWide(JoinPath(dir, "sub")).c_str(), nullptr);
    auto writeFile = [](const std::string& path, size_t size) {
        FILE* file = nullptr;
        fopen_s(&file, path.c_str(), "wb");
        std::vector<char> data(size, 1);
        fwrite(data.data(), 1, data.size(), file);
        fclose(file);
    };
    writeFile(JoinPath(JoinPath(dir, "sub"), "wanted.txt"), 4096);
    writeFile(JoinPath(JoinPath(dir, "sub"), "unwanted.bin"), 4096);

    bool sawBadName = false;
    const std::vector<mft::FileEntry> found = mft::ScanFilesWhere(
        dir, 1024, {},
        [&sawBadName](const std::string& name, uint64_t) {
            if (name.find('\\') != std::string::npos ||
                name.find('/') != std::string::npos) {
                sawBadName = true;
            }
            return name.size() >= 4 &&
                   name.compare(name.size() - 4, 4, ".txt") == 0;
        });
    CHECK(!sawBadName);
    for (const mft::FileEntry& entry : found) {
        CHECK(entry.path.size() >= 10);
        CHECK(entry.path.compare(entry.path.size() - 10, 10, "wanted.txt") == 0);
    }
    CHECK(!found.empty());

    RemoveTree(dir);
}

TEST(mft_snapshot_binary_roundtrip) {
    const std::string dir = TempDir("cdc-mft-snap-test");
    const std::string snapshotFile = JoinPath(dir, "test.bin");

    mft::Snapshot snapshot;
    snapshot.volumeSerial = 0x1234'5678;
    snapshot.journalId = 0xABCD'EF01;
    snapshot.nextUsn = 42'000;
    snapshot.minSize = 1u << 20;
    snapshot.bytesPerCluster = 4096;

    mft::DirNode node;
    node.seq = 3;
    node.parentFrn = 5;
    node.name = "Users";
    node.reparse = false;
    snapshot.dirs[10] = node;

    mft::RawFile raw;
    raw.frn = (1ULL << 48) | 20;
    raw.parentFrn = (3ULL << 48) | 10;
    raw.name = "video.mp4";
    raw.size = 4'194'304;
    raw.mtime = 1'700'000'000;
    raw.firstLcn = 999;
    raw.sortKey = 999;
    raw.canRawRead = true;
    snapshot.files[20] = raw;

    CHECK(mft::SaveSnapshot(snapshotFile, snapshot));
    const std::optional<mft::Snapshot> loaded = mft::LoadSnapshot(snapshotFile);
    CHECK(loaded.has_value());
    CHECK_EQ(loaded->volumeSerial, snapshot.volumeSerial);
    CHECK_EQ(loaded->journalId, snapshot.journalId);
    CHECK_EQ(loaded->nextUsn, snapshot.nextUsn);
    CHECK_EQ(loaded->bytesPerCluster, snapshot.bytesPerCluster);
    CHECK_EQ(loaded->dirs.at(10).name, std::string("Users"));
    CHECK_EQ(loaded->files.at(20).firstLcn, static_cast<uint64_t>(999));
    CHECK(loaded->files.at(20).canRawRead);

    RemoveTree(dir);
}
