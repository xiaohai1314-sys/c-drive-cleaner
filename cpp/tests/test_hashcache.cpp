#include <windows.h>

#include <cstdio>
#include <filesystem>
#include <string>
#include <vector>

#include "core/hashcache.h"
#include "core/timeutil.h"
#include "harness.hpp"
#include "util/env.h"
#include "util/path.h"
#include "util/utf.h"

namespace {

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
    std::error_code error;
    std::filesystem::remove_all(std::filesystem::path(Utf8ToWide(dir)), error);
    CreateDirectoryW(Utf8ToWide(dir).c_str(), nullptr);
    return dir;
}

Digest FakeDigest(uint8_t tag) {
    Digest digest{};
    digest.fill(tag);
    return digest;
}

}  // namespace

TEST(hashcache_reuses_hash_while_file_is_unchanged) {
    hashcache::HashCache cache;
    const Digest sample = FakeDigest(1);
    const Digest full = FakeDigest(2);
    cache.RememberDigests("C:\\a.bin", 10, 100, std::nullopt, sample,
                          std::nullopt, full, 1);

    const hashcache::Entry* entry = cache.EntryFor("C:\\a.bin", 10, 100);
    CHECK(entry != nullptr);
    CHECK(entry->sample == sample);
    CHECK(entry->full == full);
    CHECK(cache.EntryFor("C:\\a.bin", 11, 100) == nullptr);
    CHECK(cache.EntryFor("C:\\a.bin", 10, 101) == nullptr);
    CHECK_EQ(cache.Size(), static_cast<size_t>(1));
}

TEST(hashcache_changing_file_resets_old_hash) {
    hashcache::HashCache cache;
    cache.RememberDigests("C:\\a.bin", 10, 100, std::nullopt, FakeDigest(1),
                          std::nullopt, FakeDigest(2), 1);
    cache.RememberDigests("C:\\a.bin", 12, 200, std::nullopt, FakeDigest(3),
                          std::nullopt, std::nullopt, 2);

    const hashcache::Entry* entry = cache.EntryFor("C:\\a.bin", 12, 200);
    CHECK(entry != nullptr);
    CHECK(entry->sample == FakeDigest(3));
    CHECK(!entry->full.has_value());
}

TEST(hashcache_keeps_strong_and_full_hash_separately) {
    hashcache::HashCache cache;
    cache.RememberDigests("C:\\a.bin", 10, 100, std::nullopt, std::nullopt,
                          FakeDigest(5), FakeDigest(4), 1);
    const hashcache::Entry* entry = cache.EntryFor("C:\\a.bin", 10, 100);
    CHECK(entry != nullptr);
    CHECK(entry->full == FakeDigest(4));
    CHECK(entry->strong == FakeDigest(5));

    cache.RememberDigests("C:\\a.bin", 10, 100, std::nullopt, std::nullopt,
                          FakeDigest(6), std::nullopt, 2);
    entry = cache.EntryFor("C:\\a.bin", 10, 100);
    CHECK(entry != nullptr);
    CHECK(entry->full == FakeDigest(4));
    CHECK(entry->strong == FakeDigest(6));
}

TEST(hashcache_sqlite_persists_and_loads_only_requested_paths) {
    const std::string dir = TempDir("cdc-hashcache-test");
    const std::string db = JoinPath(dir, "test.db");

    {
        hashcache::HashCache first =
            hashcache::HashCache::LoadForPathsAt(db, {}, false);
        const uint64_t now = NowUnixSeconds();
        first.RememberDigests("D:\\a.bin", 100, 200, FakeDigest(1),
                              FakeDigest(2), std::nullopt, FakeDigest(3), now);
        first.RememberDigests("E:\\b.bin", 300, 400, FakeDigest(4),
                              std::nullopt, std::nullopt, std::nullopt, now);
        CHECK(first.Dirty());
        first.SaveDirty();
        CHECK(!first.Dirty());
    }

    hashcache::HashCache second =
        hashcache::HashCache::LoadForPathsAt(db, {"D:\\a.bin"}, false);
    const hashcache::Entry* entry = second.EntryFor("D:\\a.bin", 100, 200);
    CHECK(entry != nullptr);
    CHECK(entry->head == FakeDigest(1));
    CHECK(entry->sample == FakeDigest(2));
    CHECK(!entry->strong.has_value());
    CHECK(entry->full == FakeDigest(3));
    // 没有请求 E:\b.bin，所以不会预加载。
    CHECK(second.EntryFor("E:\\b.bin", 300, 400) == nullptr);

    std::error_code error;
    std::filesystem::remove_all(std::filesystem::path(Utf8ToWide(dir)), error);
}

TEST(hashcache_hex_roundtrip_matches_rust) {
    const Digest digest = Blake3Hash("hello", 5);
    const std::string hex = hashcache::DigestToHex(digest);
    CHECK_EQ(hex.size(), static_cast<size_t>(64));
    // BLAKE3("hello") 的已知前缀，确认与 Rust 版字节序一致。
    CHECK_EQ(hex.substr(0, 16), std::string("ea8f163db3868292"));
    const std::optional<Digest> parsed = hashcache::HexToDigest(hex);
    CHECK(parsed.has_value());
    CHECK(*parsed == digest);
    CHECK(!hashcache::HexToDigest("zz").has_value());
    CHECK(!hashcache::HexToDigest(std::string(64, 'g')).has_value());
}
