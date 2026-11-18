#include "core/hashcache.h"

#include <sqlite3.h>
#include <windows.h>

#include <filesystem>
#include <cstring>
#include <string>
#include <utility>

#include "core/timeutil.h"
#include "util/json.h"
#include "util/path.h"
#include "util/utf.h"

namespace {

constexpr uint64_t kRetentionSeconds = 30 * 86400;

const char* const kSchema =
    "PRAGMA journal_mode = WAL;"
    "PRAGMA synchronous = NORMAL;"
    "PRAGMA temp_store = MEMORY;"
    "CREATE TABLE IF NOT EXISTS file_hashes ("
    "    path TEXT PRIMARY KEY,"
    "    size INTEGER NOT NULL,"
    "    mtime INTEGER NOT NULL,"
    "    head BLOB,"
    "    sample BLOB,"
    "    strong BLOB,"
    "    full BLOB,"
    "    last_seen INTEGER NOT NULL"
    ") WITHOUT ROWID;"
    "CREATE INDEX IF NOT EXISTS idx_hashes_last_seen ON file_hashes(last_seen);";

class Connection {
public:
    Connection() = default;
    ~Connection() {
        if (db_ != nullptr) {
            sqlite3_close(db_);
        }
    }
    Connection(const Connection&) = delete;
    Connection& operator=(const Connection&) = delete;

    bool Open(const std::string& path) {
        const std::filesystem::path widePath(Utf8ToWide(path));
        std::error_code error;
        std::filesystem::create_directories(widePath.parent_path(), error);
        if (sqlite3_open_v2(path.c_str(), &db_,
                            SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
                            nullptr) != SQLITE_OK) {
            return false;
        }
        return sqlite3_exec(db_, kSchema, nullptr, nullptr, nullptr) ==
               SQLITE_OK;
    }

    sqlite3* get() const { return db_; }

private:
    sqlite3* db_ = nullptr;
};

class Statement {
public:
    Statement(sqlite3* db, const char* sql) {
        if (sqlite3_prepare_v2(db, sql, -1, &stmt_, nullptr) != SQLITE_OK) {
            stmt_ = nullptr;
        }
    }
    ~Statement() {
        if (stmt_ != nullptr) {
            sqlite3_finalize(stmt_);
        }
    }
    Statement(const Statement&) = delete;
    Statement& operator=(const Statement&) = delete;

    sqlite3_stmt* get() const { return stmt_; }
    explicit operator bool() const { return stmt_ != nullptr; }

private:
    sqlite3_stmt* stmt_ = nullptr;
};

void BindPath(sqlite3_stmt* stmt, int index, const std::string& path) {
    sqlite3_bind_text(stmt, index, path.data(),
                      static_cast<int>(path.size()), SQLITE_TRANSIENT);
}

void BindDigest(sqlite3_stmt* stmt, int index,
                const std::optional<Digest>& digest) {
    if (digest.has_value()) {
        sqlite3_bind_blob(stmt, index, digest->data(),
                          static_cast<int>(digest->size()), SQLITE_TRANSIENT);
    } else {
        sqlite3_bind_null(stmt, index);
    }
}

std::optional<Digest> ColumnDigest(sqlite3_stmt* stmt, int index) {
    if (sqlite3_column_type(stmt, index) == SQLITE_NULL) {
        return std::nullopt;
    }
    const void* blob = sqlite3_column_blob(stmt, index);
    const int bytes = sqlite3_column_bytes(stmt, index);
    if (blob == nullptr || bytes != 32) {
        return std::nullopt;
    }
    Digest digest{};
    std::memcpy(digest.data(), blob, digest.size());
    return digest;
}

uint64_t NonNegativeColumn(sqlite3_stmt* stmt, int index) {
    const sqlite3_int64 value = sqlite3_column_int64(stmt, index);
    return value > 0 ? static_cast<uint64_t>(value) : 0;
}

std::optional<std::string> JsonString(const Json& item, const char* key) {
    if (!item.is_object()) {
        return std::nullopt;
    }
    const auto found = item.find(key);
    if (found == item.end() || !found->is_string()) {
        return std::nullopt;
    }
    return found->get<std::string>();
}

void MigrateLegacy(sqlite3* db) {
    sqlite3_stmt* countStmt = nullptr;
    if (sqlite3_prepare_v2(db, "SELECT COUNT(*) FROM file_hashes", -1,
                           &countStmt, nullptr) != SQLITE_OK) {
        return;
    }
    int64_t count = 1;
    if (sqlite3_step(countStmt) == SQLITE_ROW) {
        count = sqlite3_column_int64(countStmt, 0);
    }
    sqlite3_finalize(countStmt);
    if (count > 0) {
        return;
    }

    const std::string legacyPath = JoinPath(CacheDir(), "hash-cache-v2.json");
    FILE* file = nullptr;
    if (_wfopen_s(&file, Utf8ToWide(legacyPath).c_str(), L"rb") != 0 ||
        file == nullptr) {
        return;
    }
    std::string text;
    char buffer[65536];
    size_t read = 0;
    while ((read = fread(buffer, 1, sizeof(buffer), file)) > 0) {
        text.append(buffer, read);
    }
    fclose(file);

    const Json root = Json::parse(text, nullptr, false);
    if (root.is_discarded() || !root.is_object()) {
        return;
    }
    const auto entries = root.find("entries");
    if (entries == root.end() || !entries->is_object()) {
        return;
    }

    struct LegacyEntry {
        std::string path;
        int64_t size = 0;
        int64_t mtime = 0;
        std::optional<Digest> sample;
        std::optional<Digest> strong;
        std::optional<Digest> full;
        int64_t lastSeen = 0;
    };
    std::vector<LegacyEntry> legacy;
    legacy.reserve(entries->size());
    for (auto item = entries->begin(); item != entries->end(); ++item) {
        const Json& value = item.value();
        if (!value.is_object()) {
            return;
        }
        const auto size = value.find("size");
        const auto mtime = value.find("mtime");
        if (size == value.end() || !size->is_number_integer() ||
            mtime == value.end() || !mtime->is_number_integer()) {
            return;
        }
        LegacyEntry entry;
        entry.path = item.key();
        entry.size = size->get<int64_t>();
        entry.mtime = mtime->get<int64_t>();
        if (const auto sample = JsonString(value, "sample")) {
            entry.sample = hashcache::HexToDigest(*sample);
        }
        if (const auto strong = JsonString(value, "strong")) {
            entry.strong = hashcache::HexToDigest(*strong);
        }
        if (const auto full = JsonString(value, "full")) {
            entry.full = hashcache::HexToDigest(*full);
        }
        if (const auto lastSeen = value.find("last_seen");
            lastSeen != value.end() && lastSeen->is_number_integer()) {
            entry.lastSeen = lastSeen->get<int64_t>();
        }
        if (!entry.sample.has_value() && !entry.strong.has_value() &&
            !entry.full.has_value()) {
            continue;
        }
        legacy.push_back(std::move(entry));
    }

    Statement insert(db,
                     "INSERT OR IGNORE INTO file_hashes"
                     " (path, size, mtime, head, sample, strong, full, last_seen)"
                     " VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7)");
    if (!insert) {
        return;
    }
    sqlite3_exec(db, "BEGIN", nullptr, nullptr, nullptr);
    for (const LegacyEntry& entry : legacy) {
        sqlite3_reset(insert.get());
        sqlite3_clear_bindings(insert.get());
        BindPath(insert.get(), 1, entry.path);
        sqlite3_bind_int64(insert.get(), 2, entry.size);
        sqlite3_bind_int64(insert.get(), 3, entry.mtime);
        BindDigest(insert.get(), 4, entry.sample);
        BindDigest(insert.get(), 5, entry.strong);
        BindDigest(insert.get(), 6, entry.full);
        sqlite3_bind_int64(insert.get(), 7, entry.lastSeen);
        sqlite3_step(insert.get());
    }
    sqlite3_exec(db, "COMMIT", nullptr, nullptr, nullptr);
}

}  // namespace

namespace hashcache {

std::string DigestToHex(const Digest& digest) {
    constexpr char kHex[] = "0123456789abcdef";
    std::string out;
    out.reserve(64);
    for (const uint8_t byte : digest) {
        out.push_back(kHex[byte >> 4]);
        out.push_back(kHex[byte & 0x0F]);
    }
    return out;
}

std::optional<Digest> HexToDigest(const std::string& hex) {
    if (hex.size() != 64) {
        return std::nullopt;
    }
    auto value = [](char ch) -> int {
        if (ch >= '0' && ch <= '9') {
            return ch - '0';
        }
        if (ch >= 'a' && ch <= 'f') {
            return ch - 'a' + 10;
        }
        if (ch >= 'A' && ch <= 'F') {
            return ch - 'A' + 10;
        }
        return -1;
    };
    Digest out{};
    for (size_t index = 0; index < out.size(); ++index) {
        const int hi = value(hex[index * 2]);
        const int lo = value(hex[index * 2 + 1]);
        if (hi < 0 || lo < 0) {
            return std::nullopt;
        }
        out[index] = static_cast<uint8_t>((hi << 4) | lo);
    }
    return out;
}

std::string DefaultDbPath() {
    return JoinPath(CacheDir(), "hash-cache-v3.db");
}

std::vector<std::string> DumpAll(const std::string& dbPath) {
    std::vector<std::string> lines;
    Connection conn;
    if (!conn.Open(dbPath)) {
        return lines;
    }
    Statement select(conn.get(),
                     "SELECT path, size, mtime, head, sample, strong, full,"
                     " last_seen FROM file_hashes ORDER BY path");
    if (!select) {
        return lines;
    }
    auto hex = [](const std::optional<Digest>& digest) -> std::string {
        return digest.has_value() ? DigestToHex(*digest) : std::string("-");
    };
    while (sqlite3_step(select.get()) == SQLITE_ROW) {
        const unsigned char* text = sqlite3_column_text(select.get(), 0);
        const int bytes = sqlite3_column_bytes(select.get(), 0);
        std::string line =
            text == nullptr ? std::string()
                            : std::string(reinterpret_cast<const char*>(text),
                                          static_cast<size_t>(bytes));
        line += "\t" + std::to_string(sqlite3_column_int64(select.get(), 1));
        line += "\t" + std::to_string(sqlite3_column_int64(select.get(), 2));
        line += "\t" + hex(ColumnDigest(select.get(), 3));
        line += "\t" + hex(ColumnDigest(select.get(), 4));
        line += "\t" + hex(ColumnDigest(select.get(), 5));
        line += "\t" + hex(ColumnDigest(select.get(), 6));
        line += "\t" + std::to_string(sqlite3_column_int64(select.get(), 7));
        lines.push_back(std::move(line));
    }
    return lines;
}

HashCache HashCache::LoadForPaths(const std::vector<std::string>& paths) {
    return LoadForPathsAt(DefaultDbPath(), paths, true);
}

HashCache HashCache::LoadForPathsAt(const std::string& dbPath,
                                    const std::vector<std::string>& paths,
                                    bool migrateLegacy) {
    HashCache cache;
    cache.dbPath_ = dbPath;
    Connection conn;
    if (!conn.Open(dbPath)) {
        return cache;
    }
    if (migrateLegacy) {
        MigrateLegacy(conn.get());
    }
    Statement select(conn.get(),
                     "SELECT size, mtime, head, sample, strong, full, last_seen"
                     " FROM file_hashes WHERE path = ?1");
    if (!select) {
        return cache;
    }
    sqlite3_exec(conn.get(), "BEGIN", nullptr, nullptr, nullptr);
    for (const std::string& path : paths) {
        if (cache.entries_.find(path) != cache.entries_.end()) {
            continue;
        }
        sqlite3_reset(select.get());
        sqlite3_clear_bindings(select.get());
        BindPath(select.get(), 1, path);
        if (sqlite3_step(select.get()) != SQLITE_ROW) {
            continue;
        }
        Entry entry;
        entry.size = NonNegativeColumn(select.get(), 0);
        entry.mtime = NonNegativeColumn(select.get(), 1);
        entry.head = ColumnDigest(select.get(), 2);
        entry.sample = ColumnDigest(select.get(), 3);
        entry.strong = ColumnDigest(select.get(), 4);
        entry.full = ColumnDigest(select.get(), 5);
        entry.lastSeen = NonNegativeColumn(select.get(), 6);
        cache.entries_[path] = entry;
    }
    sqlite3_exec(conn.get(), "COMMIT", nullptr, nullptr, nullptr);
    return cache;
}

const Entry* HashCache::EntryFor(const std::string& path, uint64_t size,
                                 uint64_t mtime) const {
    const auto found = entries_.find(path);
    if (found == entries_.end()) {
        return nullptr;
    }
    if (found->second.size != size || found->second.mtime != mtime) {
        return nullptr;
    }
    return &found->second;
}

void HashCache::RememberDigests(const std::string& path, uint64_t size,
                                uint64_t mtime, std::optional<Digest> head,
                                std::optional<Digest> sample,
                                std::optional<Digest> strong,
                                std::optional<Digest> full, uint64_t now) {
    bool changed = false;
    auto inserted = entries_.emplace(path, Entry{});
    Entry& entry = inserted.first->second;
    if (inserted.second) {
        changed = true;
        entry.size = size;
        entry.mtime = mtime;
        entry.lastSeen = now;
    }
    if (entry.size != size || entry.mtime != mtime) {
        entry.size = size;
        entry.mtime = mtime;
        entry.head.reset();
        entry.sample.reset();
        entry.strong.reset();
        entry.full.reset();
        changed = true;
    }
    auto apply = [&changed](std::optional<Digest>& slot,
                            std::optional<Digest> value) {
        if (value.has_value() && slot != value) {
            slot = value;
            changed = true;
        }
    };
    apply(entry.head, head);
    apply(entry.sample, sample);
    apply(entry.strong, strong);
    apply(entry.full, full);
    if (changed) {
        entry.lastSeen = now;
        dirty_.insert(path);
    }
}

void HashCache::SaveDirty() {
    if (dirty_.empty()) {
        return;
    }
    if (dbPath_.empty()) {
        dirty_.clear();
        return;
    }
    Connection conn;
    if (!conn.Open(dbPath_)) {
        return;
    }
    Statement insert(
        conn.get(),
        "INSERT INTO file_hashes"
        " (path, size, mtime, head, sample, strong, full, last_seen)"
        " VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        " ON CONFLICT(path) DO UPDATE SET"
        "     size = excluded.size,"
        "     mtime = excluded.mtime,"
        "     head = COALESCE(excluded.head, file_hashes.head),"
        "     sample = COALESCE(excluded.sample, file_hashes.sample),"
        "     strong = COALESCE(excluded.strong, file_hashes.strong),"
        "     full = COALESCE(excluded.full, file_hashes.full),"
        "     last_seen = excluded.last_seen");
    if (!insert) {
        return;
    }
    sqlite3_exec(conn.get(), "BEGIN", nullptr, nullptr, nullptr);
    for (const std::string& path : dirty_) {
        const auto found = entries_.find(path);
        if (found == entries_.end()) {
            continue;
        }
        const Entry& entry = found->second;
        sqlite3_reset(insert.get());
        sqlite3_clear_bindings(insert.get());
        BindPath(insert.get(), 1, path);
        sqlite3_bind_int64(insert.get(), 2, static_cast<int64_t>(entry.size));
        sqlite3_bind_int64(insert.get(), 3, static_cast<int64_t>(entry.mtime));
        BindDigest(insert.get(), 4, entry.head);
        BindDigest(insert.get(), 5, entry.sample);
        BindDigest(insert.get(), 6, entry.strong);
        BindDigest(insert.get(), 7, entry.full);
        sqlite3_bind_int64(insert.get(), 8,
                           static_cast<int64_t>(entry.lastSeen));
        sqlite3_step(insert.get());
    }

    const uint64_t now = NowUnixSeconds();
    const uint64_t cutoff =
        now > kRetentionSeconds ? now - kRetentionSeconds : 0;
    Statement remove(conn.get(),
                     "DELETE FROM file_hashes WHERE last_seen < ?1");
    if (remove) {
        sqlite3_bind_int64(remove.get(), 1, static_cast<int64_t>(cutoff));
        sqlite3_step(remove.get());
    }
    if (sqlite3_exec(conn.get(), "COMMIT", nullptr, nullptr, nullptr) ==
        SQLITE_OK) {
        dirty_.clear();
    }
}

}  // namespace hashcache
