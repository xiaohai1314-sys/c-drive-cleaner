#include "core/privacy.h"

#include <windows.h>
#include <sqlite3.h>

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "core/logs.h"
#include "core/recycle.h"
#include "core/safety.h"
#include "core/task.h"
#include "core/timeutil.h"
#include "util/env.h"
#include "util/path.h"
#include "util/utf.h"
#include "util/win32.h"

namespace {

constexpr size_t kRegNameBuffer = 512;

const char* const kShopDomains[] = {
    "taobao.com", "tmall.com",  "jd.com",    "yangkeduo.com", "pinduoduo.com",
    "amazon.com", "amazon.cn",  "smzdm.com", "suning.com",    "dangdang.com"};
const char* const kFinanceDomains[] = {"alipay.com", "paypal.com", "icbc.com.cn",
                                       "ccb.com",    "cmbchina.com",
                                       "unionpay.com", "abchina.com"};
const char* const kMedicalDomains[] = {"haodf.com", "dxy.cn", "xywy.com",
                                       "chunyuyisheng.com", "guahao.com"};
const char* const kSensitiveWords[] = {"身份证", "密码", "银行卡",
                                       "合同",   "护照", "工资"};
const char* const kRecentMediaExt[] = {".mp4", ".mkv", ".avi", ".mp3",
                                       ".flac", ".wav", ".mov"};

constexpr const char* kRecentDocs =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\RecentDocs";
constexpr const char* kUserAssist =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\UserAssist";
constexpr const char* kWmpRecent =
    "Software\\Microsoft\\MediaPlayer\\Player\\RecentFileList";
constexpr const char* kUsbStorage = "SYSTEM\\CurrentControlSet\\Enum\\USBSTOR";

template <size_t N>
std::vector<std::string> ToList(const char* const (&values)[N]) {
    return std::vector<std::string>(values, values + N);
}

std::string LowerAscii(std::string text) {
    for (char& ch : text) {
        ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    }
    return text;
}

std::string TempDir() {
    wchar_t buffer[MAX_PATH + 1] = {};
    const DWORD length = GetTempPathW(MAX_PATH + 1, buffer);
    if (length == 0) {
        return {};
    }
    return WideToUtf8(std::wstring(buffer, length));
}

std::string SystemRoot() {
    std::string root = GetEnv("SystemRoot");
    if (root.empty()) {
        root = "C:\\Windows";
    }
    while (!root.empty() && root.back() == '\\') {
        root.pop_back();
    }
    return root;
}

struct DirEntry {
    std::string name;
    std::string path;
    DWORD attributes = 0;
};

std::vector<DirEntry> ListDir(const std::string& dir) {
    std::vector<DirEntry> entries;
    if (dir.empty()) {
        return entries;
    }
    WIN32_FIND_DATAW data;
    const HANDLE find =
        FindFirstFileW((Utf8ToWide(dir) + L"\\*").c_str(), &data);
    if (find == INVALID_HANDLE_VALUE) {
        return entries;
    }
    do {
        const std::wstring name(data.cFileName);
        if (name.empty() || name == L"." || name == L"..") {
            continue;
        }
        DirEntry entry;
        entry.name = WideToUtf8(name);
        entry.path = dir + "\\" + entry.name;
        entry.attributes = data.dwFileAttributes;
        entries.push_back(std::move(entry));
    } while (FindNextFileW(find, &data) != 0);
    FindClose(find);
    return entries;
}

bool IsFile(const std::string& path) {
    const DWORD attributes = GetFileAttributesW(Utf8ToWide(path).c_str());
    return attributes != INVALID_FILE_ATTRIBUTES &&
           (attributes & FILE_ATTRIBUTE_DIRECTORY) == 0;
}

bool IsDir(const std::string& path) {
    const DWORD attributes = GetFileAttributesW(Utf8ToWide(path).c_str());
    return attributes != INVALID_FILE_ATTRIBUTES &&
           (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
}

std::string FileStem(const std::string& name) {
    const size_t dot = name.find_last_of('.');
    if (dot == std::string::npos || dot == 0) {
        return name;
    }
    return name.substr(0, dot);
}

std::string ExtensionOf(const std::string& name) {
    const size_t dot = name.find_last_of('.');
    if (dot == std::string::npos || dot == 0) {
        return {};
    }
    return name.substr(dot);
}

// ---------- SQLite ----------

class Database {
public:
    Database() = default;
    ~Database() {
        if (db_ != nullptr) {
            sqlite3_close(db_);
        }
    }
    Database(const Database&) = delete;
    Database& operator=(const Database&) = delete;
    Database(Database&& other) noexcept : db_(other.db_) {
        other.db_ = nullptr;
    }
    Database& operator=(Database&&) = delete;

    bool Open(const std::string& path) {
        return sqlite3_open_v2(path.c_str(), &db_, SQLITE_OPEN_READWRITE,
                               nullptr) == SQLITE_OK;
    }

    void Close() {
        if (db_ != nullptr) {
            sqlite3_close(db_);
            db_ = nullptr;
        }
    }

    bool Exec(const std::string& sql) {
        return db_ != nullptr &&
               sqlite3_exec(db_, sql.c_str(), nullptr, nullptr, nullptr) ==
                   SQLITE_OK;
    }

    /// 单值查询；任何错误都返回 nullopt（与 Rust 的 `.ok()` 一致）。
    std::optional<int64_t> Scalar(const std::string& sql,
                                  const std::vector<std::string>& params) {
        if (db_ == nullptr) {
            return std::nullopt;
        }
        sqlite3_stmt* statement = nullptr;
        if (sqlite3_prepare_v2(db_, sql.c_str(), -1, &statement, nullptr) !=
            SQLITE_OK) {
            return std::nullopt;
        }
        for (size_t index = 0; index < params.size(); ++index) {
            sqlite3_bind_text(statement, static_cast<int>(index + 1),
                              params[index].data(),
                              static_cast<int>(params[index].size()),
                              SQLITE_TRANSIENT);
        }
        std::optional<int64_t> value;
        if (sqlite3_step(statement) == SQLITE_ROW) {
            value = sqlite3_column_int64(statement, 0);
        }
        sqlite3_finalize(statement);
        return value;
    }

private:
    sqlite3* db_ = nullptr;
};

std::optional<Database> ConnectCopy(const std::string& path,
                                    std::string& tempPath) {
    if (!IsFile(path) || safety::IsPasswordStore(path)) {
        return std::nullopt;
    }
    const uint64_t nonce = static_cast<uint64_t>(NowMs());
    tempPath = TempDir() + "cdc-privacy-" + Stamp() + "-" +
               std::to_string(GetCurrentProcessId()) + "-" +
               std::to_string(nonce) + ".db";
    if (CopyFileW(Utf8ToWide(path).c_str(), Utf8ToWide(tempPath).c_str(),
                  FALSE) == 0) {
        tempPath.clear();
        return std::nullopt;
    }
    Database database;
    if (!database.Open(tempPath)) {
        tempPath.clear();
        return std::nullopt;
    }
    return database;
}

std::optional<int64_t> DomainCount(Database& database,
                                   const std::string& table,
                                   const std::vector<std::string>& domains) {
    std::string clause;
    std::vector<std::string> patterns;
    for (const std::string& domain : domains) {
        if (!clause.empty()) {
            clause += " OR ";
        }
        clause += "url LIKE ?";
        patterns.push_back("%" + domain + "%");
    }
    return database.Scalar(
        "SELECT COUNT(*) FROM " + table + " WHERE " + clause, patterns);
}

template <size_t N>
std::vector<std::string> DomainSql(const char* const (&domains)[N]) {
    std::string likes;
    for (size_t index = 0; index < N; ++index) {
        if (!likes.empty()) {
            likes += " OR ";
        }
        likes += "url LIKE '%" + std::string(domains[index]) + "%'";
    }
    return {"DELETE FROM visits WHERE url IN (SELECT id FROM urls WHERE " +
                likes + ")",
            "DELETE FROM urls WHERE " + likes,
            "DELETE FROM moz_places WHERE " + likes};
}

std::vector<std::string> SqlStatements(const std::string& kind) {
    if (kind == "history") {
        return {"DELETE FROM visits", "DELETE FROM urls",
                "DELETE FROM moz_historyvisits", "DELETE FROM moz_places"};
    }
    if (kind == "cookies") {
        return {"DELETE FROM cookies", "DELETE FROM moz_cookies"};
    }
    if (kind == "downloads") {
        return {"DELETE FROM downloads_url_chains", "DELETE FROM downloads",
                "DELETE FROM moz_annos WHERE anno_attribute_id IN "
                "(SELECT id FROM moz_anno_attributes WHERE name LIKE "
                "'downloads/%')"};
    }
    if (kind == "shop") {
        return DomainSql(kShopDomains);
    }
    if (kind == "money") {
        return DomainSql(kFinanceDomains);
    }
    if (kind == "medical") {
        return DomainSql(kMedicalDomains);
    }
    return {};
}

bool ApplySql(const std::string& path,
              const std::vector<std::string>& statements) {
    if (!IsFile(path) || safety::IsPasswordStore(path)) {
        return false;
    }
    std::string tempPath;
    std::optional<Database> database = ConnectCopy(path, tempPath);
    if (!database.has_value()) {
        return false;
    }
    bool applied = false;
    for (const std::string& statement : statements) {
        if (database->Exec(statement)) {
            applied = true;
        }
    }
    if (!applied) {
        database->Close();
        DeleteFileW(Utf8ToWide(tempPath).c_str());
        return false;
    }
    // 关闭连接会把 WAL 回写进主文件，之后复制回去才是完整的。
    database->Close();
    const bool copied =
        CopyFileW(Utf8ToWide(tempPath).c_str(), Utf8ToWide(path).c_str(),
                  FALSE) != 0;
    DeleteFileW(Utf8ToWide(tempPath).c_str());
    return copied;
}

// ---------- 注册表 ----------

class RegKey {
public:
    RegKey() = default;
    explicit RegKey(HKEY key) : key_(key) {}
    ~RegKey() {
        if (key_ != nullptr) {
            RegCloseKey(key_);
        }
    }
    RegKey(const RegKey&) = delete;
    RegKey& operator=(const RegKey&) = delete;
    RegKey(RegKey&& other) noexcept : key_(other.key_) { other.key_ = nullptr; }
    RegKey& operator=(RegKey&& other) noexcept {
        if (this != &other) {
            if (key_ != nullptr) {
                RegCloseKey(key_);
            }
            key_ = other.key_;
            other.key_ = nullptr;
        }
        return *this;
    }

    HKEY get() const { return key_; }
    explicit operator bool() const { return key_ != nullptr; }

private:
    HKEY key_ = nullptr;
};

std::optional<RegKey> OpenRegKey(HKEY root, const std::string& path,
                                 REGSAM access) {
    HKEY key = nullptr;
    if (RegOpenKeyExW(root, Utf8ToWide(path).c_str(), 0, access, &key) != 0 ||
        key == nullptr) {
        return std::nullopt;
    }
    return RegKey(key);
}

std::vector<std::string> ListValues(HKEY key) {
    std::vector<std::string> names;
    for (DWORD index = 0;; ++index) {
        wchar_t buffer[kRegNameBuffer] = {};
        DWORD length = kRegNameBuffer;
        if (RegEnumValueW(key, index, buffer, &length, nullptr, nullptr,
                          nullptr, nullptr) != 0) {
            break;
        }
        names.push_back(WideToUtf8(std::wstring(buffer, length)));
    }
    return names;
}

std::vector<std::string> ListSubkeys(HKEY key) {
    std::vector<std::string> names;
    for (DWORD index = 0;; ++index) {
        wchar_t buffer[kRegNameBuffer] = {};
        DWORD length = kRegNameBuffer;
        if (RegEnumKeyExW(key, index, buffer, &length, nullptr, nullptr,
                          nullptr, nullptr) != 0) {
            break;
        }
        names.push_back(WideToUtf8(std::wstring(buffer, length)));
    }
    return names;
}

HKEY HiveRoot(const std::string& hive) {
    return hive == "HKLM" ? HKEY_LOCAL_MACHINE : HKEY_CURRENT_USER;
}

int64_t ClearValues(const std::string& hive, const std::string& path,
                    const std::string& label) {
    if (!privacy::BackupRegistryKey(hive, path, label)) {
        return 0;
    }
    std::optional<RegKey> key =
        OpenRegKey(HiveRoot(hive), path, KEY_ALL_ACCESS);
    if (!key.has_value()) {
        return 0;
    }
    int64_t deleted = 0;
    for (const std::string& name : ListValues(key->get())) {
        if (RegDeleteValueW(key->get(), Utf8ToWide(name).c_str()) == 0) {
            ++deleted;
        }
    }
    return deleted;
}

std::optional<int64_t> CountValues(const std::string& hive,
                                   const std::string& path) {
    std::optional<RegKey> key = OpenRegKey(HiveRoot(hive), path, KEY_READ);
    if (!key.has_value()) {
        return std::nullopt;
    }
    return static_cast<int64_t>(ListValues(key->get()).size());
}

std::optional<int64_t> CountSubkeys(const std::string& hive,
                                    const std::string& path) {
    std::optional<RegKey> key = OpenRegKey(HiveRoot(hive), path, KEY_READ);
    if (!key.has_value()) {
        return std::nullopt;
    }
    return static_cast<int64_t>(ListSubkeys(key->get()).size());
}

std::vector<std::string> RecentDocsPaths() {
    std::vector<std::string> paths{kRecentDocs};
    std::optional<RegKey> key =
        OpenRegKey(HKEY_CURRENT_USER, kRecentDocs, KEY_READ);
    if (key.has_value()) {
        for (const std::string& subkey : ListSubkeys(key->get())) {
            paths.push_back(std::string(kRecentDocs) + "\\" + subkey);
        }
    }
    return paths;
}

std::vector<std::string> UserAssistCountKeys() {
    std::vector<std::string> paths;
    std::optional<RegKey> key =
        OpenRegKey(HKEY_CURRENT_USER, kUserAssist, KEY_READ);
    if (key.has_value()) {
        for (const std::string& guid : ListSubkeys(key->get())) {
            paths.push_back(std::string(kUserAssist) + "\\" + guid +
                            "\\Count");
        }
    }
    return paths;
}

// ---------- 浏览器 ----------

struct BrowserSpec {
    std::string id;
    std::string name;
    std::string root;
    bool firefox = false;
};

std::vector<BrowserSpec> BrowserSpecs() {
    const std::string local = GetEnv("LOCALAPPDATA");
    const std::string roaming = GetEnv("APPDATA");
    std::vector<BrowserSpec> specs;
    struct ChromiumSpec {
        const char* id;
        const char* name;
        const char* suffix;
    };
    const ChromiumSpec chromium[] = {
        {"chrome", "Chrome", "\\Google\\Chrome\\User Data"},
        {"edge", "Edge", "\\Microsoft\\Edge\\User Data"},
        {"qqbrowser", "QQ浏览器", "\\Tencent\\QQBrowser\\User Data"},
        {"quark", "夸克浏览器", "\\Quark\\User Data"},
        {"brave", "Brave", "\\BraveSoftware\\Brave-Browser\\User Data"},
    };
    for (const ChromiumSpec& item : chromium) {
        BrowserSpec spec;
        spec.id = item.id;
        spec.name = item.name;
        spec.root = local + item.suffix;
        if (IsDir(spec.root)) {
            specs.push_back(std::move(spec));
        }
    }
    BrowserSpec firefox;
    firefox.id = "firefox";
    firefox.name = "Firefox";
    firefox.root = roaming + "\\Mozilla\\Firefox\\Profiles";
    firefox.firefox = true;
    if (IsDir(firefox.root)) {
        specs.push_back(std::move(firefox));
    }
    return specs;
}

std::vector<std::string> Profiles(const BrowserSpec& spec) {
    std::vector<std::string> found;
    for (const DirEntry& entry : ListDir(spec.root)) {
        if ((entry.attributes & FILE_ATTRIBUTE_DIRECTORY) == 0) {
            continue;
        }
        if (spec.firefox) {
            found.push_back(entry.path);
            continue;
        }
        if (entry.name == "Default" ||
            entry.name.rfind("Profile", 0) == 0 ||
            entry.name == "Guest Profile") {
            found.push_back(entry.path);
        }
    }
    return found;
}

std::optional<std::string> HistoryFile(const BrowserSpec& spec,
                                       const std::string& profile) {
    const std::string path =
        profile + (spec.firefox ? "\\places.sqlite" : "\\History");
    return IsFile(path) ? std::optional<std::string>(path) : std::nullopt;
}

std::vector<std::string> CookieFiles(const BrowserSpec& spec,
                                     const std::string& profile) {
    std::vector<std::string> files;
    if (spec.firefox) {
        const std::string path = profile + "\\cookies.sqlite";
        if (IsFile(path)) {
            files.push_back(path);
        }
        return files;
    }
    for (const std::string& path :
         {profile + "\\Cookies", profile + "\\Network\\Cookies"}) {
        if (IsFile(path) && !safety::IsPasswordStore(path)) {
            files.push_back(path);
        }
    }
    return files;
}

struct HistoryStats {
    int64_t urls = 0;
    int64_t downloads = 0;
    int64_t shop = 0;
    int64_t money = 0;
    int64_t medical = 0;
    bool urlsAvailable = false;
};

HistoryStats HistoryStatsOf(const std::string& path) {
    HistoryStats stats;
    std::string tempPath;
    std::optional<Database> database = ConnectCopy(path, tempPath);
    if (!database.has_value()) {
        return stats;
    }
    std::optional<int64_t> urls =
        database->Scalar("SELECT COUNT(*) FROM urls", {});
    if (!urls.has_value()) {
        urls = database->Scalar("SELECT COUNT(*) FROM moz_places", {});
    }
    if (urls.has_value()) {
        stats.urls = *urls;
        stats.urlsAvailable = true;
    }
    std::optional<int64_t> downloads =
        database->Scalar("SELECT COUNT(*) FROM downloads", {});
    if (!downloads.has_value()) {
        downloads = database->Scalar(
            "SELECT COUNT(*) FROM moz_annos a JOIN moz_anno_attributes n ON "
            "a.anno_attribute_id = n.id WHERE n.name LIKE 'downloads/%'",
            {});
    }
    stats.downloads = downloads.value_or(0);
    const std::vector<std::string> shop = ToList(kShopDomains);
    const std::vector<std::string> money = ToList(kFinanceDomains);
    const std::vector<std::string> medical = ToList(kMedicalDomains);
    std::optional<int64_t> value = DomainCount(*database, "urls", shop);
    if (!value.has_value()) {
        value = DomainCount(*database, "moz_places", shop);
    }
    stats.shop = value.value_or(0);
    value = DomainCount(*database, "urls", money);
    if (!value.has_value()) {
        value = DomainCount(*database, "moz_places", money);
    }
    stats.money = value.value_or(0);
    value = DomainCount(*database, "urls", medical);
    if (!value.has_value()) {
        value = DomainCount(*database, "moz_places", medical);
    }
    stats.medical = value.value_or(0);
    database->Close();
    DeleteFileW(Utf8ToWide(tempPath).c_str());
    return stats;
}

int64_t CookieCount(const std::string& path) {
    std::string tempPath;
    std::optional<Database> database = ConnectCopy(path, tempPath);
    if (!database.has_value()) {
        return 0;
    }
    std::optional<int64_t> value =
        database->Scalar("SELECT COUNT(*) FROM cookies", {});
    if (!value.has_value()) {
        value = database->Scalar("SELECT COUNT(*) FROM moz_cookies", {});
    }
    database->Close();
    DeleteFileW(Utf8ToWide(tempPath).c_str());
    return value.value_or(0);
}

// ---------- 最近文件 ----------

std::string RecentDir() {
    return GetEnv("APPDATA") + "\\Microsoft\\Windows\\Recent";
}

std::vector<std::string> RecentFiles() {
    std::vector<std::string> files;
    for (const DirEntry& entry : ListDir(RecentDir())) {
        if ((entry.attributes & FILE_ATTRIBUTE_DIRECTORY) == 0) {
            files.push_back(entry.path);
        }
    }
    return files;
}

bool ContainsAny(const std::string& text,
                 const std::vector<std::string>& needles) {
    for (const std::string& needle : needles) {
        if (text.find(needle) != std::string::npos) {
            return true;
        }
    }
    return false;
}

Json MakeItem(const std::string& id, const std::string& section,
              const std::string& name, int64_t count, bool countAvailable,
              const std::vector<std::string>& files, const std::string& action,
              const std::string& note) {
    return Json{{"id", id},       {"section", section},
                {"name", name},   {"count", count},
                {"count_available", countAvailable},
                {"files", files}, {"action", action},
                {"note", note}};
}

}  // namespace

namespace privacy {

bool BackupRegistryKey(const std::string& hive, const std::string& path,
                       const std::string& label) {
    const std::string dir = JoinPath(JoinPath(logs::LogDir(), "backups"), "reg");
    CreateDirectoryW(Utf8ToWide(JoinPath(logs::LogDir(), "backups")).c_str(),
                     nullptr);
    CreateDirectoryW(Utf8ToWide(dir).c_str(), nullptr);
    const std::string file = JoinPath(dir, label + "_" + Stamp() + ".reg");
    const std::string full = hive + "\\" + path;
    const ProcessResult result = RunCapture(
        Utf8ToWide(SystemRoot() + "\\System32\\reg.exe"),
        {L"export", Utf8ToWide(full), Utf8ToWide(file), L"/y"});
    return result.Success();
}

std::vector<std::string> StatementsFor(const std::string& kind) {
    return SqlStatements(kind);
}

Json Scan() {
    const std::vector<BrowserSpec> specs = BrowserSpecs();

    std::vector<Json> historyItems;
    std::vector<Json> downloadItems;
    std::vector<Json> shopItems;
    int64_t moneyTotal = 0;
    int64_t medicalTotal = 0;
    int64_t shopTotal = 0;
    bool moneyAvailable = false;
    bool medicalAvailable = false;
    bool shopAvailable = false;
    int64_t cookieTotal = 0;
    bool cookieAvailable = false;
    std::vector<std::string> cookieFilesAll;

    for (const BrowserSpec& spec : specs) {
        int64_t urls = 0;
        int64_t downloads = 0;
        int64_t shop = 0;
        int64_t money = 0;
        int64_t medical = 0;
        bool urlsAvailable = false;
        for (const std::string& profile : Profiles(spec)) {
            if (const std::optional<std::string> history =
                    HistoryFile(spec, profile)) {
                const HistoryStats stats = HistoryStatsOf(*history);
                urls += stats.urls;
                downloads += stats.downloads;
                shop += stats.shop;
                money += stats.money;
                medical += stats.medical;
                urlsAvailable = urlsAvailable || stats.urlsAvailable;
            }
            for (const std::string& cookie : CookieFiles(spec, profile)) {
                const int64_t value = CookieCount(cookie);
                if (value > 0 || cookieAvailable) {
                    cookieAvailable = true;
                }
                cookieTotal += value;
                cookieFilesAll.push_back(cookie);
            }
        }
        moneyTotal += money;
        medicalTotal += medical;
        shopTotal += shop;
        moneyAvailable = moneyAvailable || money > 0;
        medicalAvailable = medicalAvailable || medical > 0;
        shopAvailable = shopAvailable || urlsAvailable;
        historyItems.push_back(MakeItem("hist_" + spec.id, "history", spec.name,
                                        urls, urlsAvailable, {}, "sql", ""));
        downloadItems.push_back(
            MakeItem("down_" + spec.id, "download", spec.name, downloads,
                     urlsAvailable, {}, "sql",
                     "只删浏览记录，不删除已下载的文件"));
        shopItems.push_back(MakeItem("shop_" + spec.id, "shop", spec.name, shop,
                                     urlsAvailable, {}, "sql", ""));
    }

    const std::vector<std::string> recent = RecentFiles();
    const std::vector<std::string> sensitiveWords = ToList(kSensitiveWords);
    std::vector<std::string> sensitive;
    for (const std::string& path : recent) {
        if (ContainsAny(Basename(path), sensitiveWords)) {
            sensitive.push_back(path);
        }
    }

    std::vector<Json> high;
    high.push_back(MakeItem(
        "risk_file", "high", "文件敏感隐私",
        static_cast<int64_t>(sensitive.size()), !recent.empty(), sensitive,
        "files", "最近打开的文件名里含身份证、密码、合同等字样"));
    high.push_back(MakeItem("risk_money", "high", "财产安全隐私", moneyTotal,
                            moneyAvailable, {}, "sql", ""));
    high.push_back(MakeItem("risk_shop", "high", "网络购物痕迹", shopTotal,
                            shopAvailable, {}, "sql", ""));
    high.push_back(MakeItem("risk_medical", "high", "医疗信息隐私",
                            medicalTotal, medicalAvailable, {}, "sql", ""));
    high.push_back(MakeItem("cookies", "high", "Cookie", cookieTotal,
                            cookieAvailable, cookieFilesAll, "sql",
                            "清理后部分网站需要重新登录"));
    for (Json& item : historyItems) {
        high.push_back(std::move(item));
    }
    for (Json& item : downloadItems) {
        high.push_back(std::move(item));
    }
    for (Json& item : shopItems) {
        high.push_back(std::move(item));
    }

    std::vector<Json> computer;
    int64_t recentCount = 0;
    bool recentAvailable = false;
    for (const std::string& path : RecentDocsPaths()) {
        if (const std::optional<int64_t> value = CountValues("HKCU", path)) {
            recentAvailable = true;
            recentCount += *value;
        }
    }
    computer.push_back(MakeItem("reg_file", "computer", "文件访问记录",
                                recentCount, recentAvailable, {}, "registry",
                                "最近文档列表，删除前会导出备份"));
    const int64_t usb = CountSubkeys("HKLM", kUsbStorage).value_or(0);
    computer.push_back(MakeItem("reg_usb", "computer", "USB 使用痕迹", usb,
                                usb > 0, {}, "count",
                                "只统计；清理 USB 记录需要改系统设备记录，"
                                "Rust 版暂不执行"));

    int64_t userassist = 0;
    for (const std::string& path : UserAssistCountKeys()) {
        if (const std::optional<int64_t> value = CountValues("HKCU", path)) {
            userassist += *value;
        }
    }
    int64_t prefetchCount = 0;
    for (const DirEntry& entry : ListDir(SystemRoot() + "\\Prefetch")) {
        if ((entry.attributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
            continue;
        }
        const std::string lowered = LowerAscii(entry.name);
        if (lowered.size() >= 3 &&
            lowered.compare(lowered.size() - 3, 3, ".pf") == 0) {
            ++prefetchCount;
        }
    }
    std::vector<Json> software;
    software.push_back(MakeItem("software", "software", "软件使用痕迹",
                                userassist + prefetchCount, true, {},
                                "userassist",
                                "清理 UserAssist 记录；Prefetch 文件保留"));

    const std::vector<std::string> mediaExt = ToList(kRecentMediaExt);
    std::vector<std::string> mediaFiles;
    for (const std::string& path : recent) {
        const std::string stem = FileStem(Basename(path));
        const std::string ext = LowerAscii(ExtensionOf(stem));
        if (std::find(mediaExt.begin(), mediaExt.end(), ext) !=
            mediaExt.end()) {
            mediaFiles.push_back(path);
        }
    }
    const int64_t wmp = CountValues("HKCU", kWmpRecent).value_or(0);
    int64_t vlc = 0;
    {
        const std::string ini = GetEnv("APPDATA") + "\\vlc\\vlc-qt-interface.ini";
        FILE* file = nullptr;
        if (_wfopen_s(&file, Utf8ToWide(ini).c_str(), L"rb") == 0 &&
            file != nullptr) {
            std::string text;
            char buffer[4096];
            size_t read = 0;
            while ((read = fread(buffer, 1, sizeof(buffer), file)) > 0) {
                text.append(buffer, read);
            }
            fclose(file);
            size_t begin = 0;
            while (begin <= text.size()) {
                const size_t end = text.find('\n', begin);
                const std::string line =
                    text.substr(begin, end == std::string::npos
                                           ? std::string::npos
                                           : end - begin);
                if (LowerAscii(line).find("file:///") != std::string::npos) {
                    ++vlc;
                }
                if (end == std::string::npos) {
                    break;
                }
                begin = end + 1;
            }
        }
    }
    std::vector<Json> media;
    media.push_back(MakeItem(
        "media", "media", "影音播放痕迹",
        static_cast<int64_t>(mediaFiles.size()) + wmp + vlc, true, mediaFiles,
        "media", "清理最近播放列表和最近打开的音视频快捷方式"));

    Json all = Json::array();
    for (Json& item : high) {
        all.push_back(std::move(item));
    }
    for (Json& item : computer) {
        all.push_back(std::move(item));
    }
    for (Json& item : software) {
        all.push_back(std::move(item));
    }
    for (Json& item : media) {
        all.push_back(std::move(item));
    }
    return all;
}

Json Clean(const std::vector<std::string>& ids) {
    const Json all = Scan();
    std::vector<Json> selected;
    for (const Json& item : all) {
        const std::string id = item.value("id", std::string());
        if (item.value("action", std::string()) == "count") {
            continue;
        }
        if (std::find(ids.begin(), ids.end(), id) != ids.end()) {
            selected.push_back(item);
        }
    }

    int64_t filesDone = 0;
    int64_t sqlDone = 0;
    int64_t registryDone = 0;
    std::vector<std::string> failed;
    std::vector<std::string> files;
    std::vector<std::pair<std::string, std::vector<std::string>>> sqlJobs;
    std::vector<std::string> registryJobs;
    bool userassist = false;
    bool media = false;

    for (const Json& item : selected) {
        const std::string id = item.value("id", std::string());
        const std::string action = item.value("action", std::string());
        if (action == "sql") {
            std::vector<std::string> statements;
            if (id == "risk_money") {
                statements = SqlStatements("money");
            } else if (id == "risk_shop") {
                statements = SqlStatements("shop");
            } else if (id == "risk_medical") {
                statements = SqlStatements("medical");
            } else if (id.rfind("hist_", 0) == 0) {
                statements = SqlStatements("history");
            } else if (id.rfind("down_", 0) == 0) {
                statements = SqlStatements("downloads");
            } else if (id.rfind("shop_", 0) == 0) {
                statements = SqlStatements("shop");
            } else if (id == "cookies") {
                statements = SqlStatements("cookies");
            }
            if (statements.empty()) {
                continue;
            }
            for (const BrowserSpec& spec : BrowserSpecs()) {
                const bool global = id == "cookies" || id == "risk_money" ||
                                    id == "risk_shop" || id == "risk_medical";
                const bool matchesSpec =
                    id.size() >= spec.id.size() &&
                    id.compare(id.size() - spec.id.size(), spec.id.size(),
                               spec.id) == 0;
                if (!global && !matchesSpec) {
                    continue;
                }
                for (const std::string& profile : Profiles(spec)) {
                    if (id == "cookies") {
                        for (const std::string& cookie :
                             CookieFiles(spec, profile)) {
                            sqlJobs.emplace_back(cookie, statements);
                        }
                        continue;
                    }
                    if (const std::optional<std::string> history =
                            HistoryFile(spec, profile)) {
                        sqlJobs.emplace_back(*history, statements);
                    }
                }
            }
        } else if (action == "registry") {
            registryJobs.push_back(id);
        } else if (action == "userassist") {
            userassist = true;
        } else if (action == "media") {
            for (const Json& path : item["files"]) {
                if (path.is_string()) {
                    files.push_back(path.get<std::string>());
                }
            }
            media = true;
        } else {
            for (const Json& path : item["files"]) {
                if (path.is_string()) {
                    files.push_back(path.get<std::string>());
                }
            }
        }
    }

    std::vector<std::string> cleaned;
    for (const std::string& path : files) {
        if (path.find_first_not_of(" \t\r\n") == std::string::npos ||
            safety::IsPasswordStore(path)) {
            continue;
        }
        cleaned.push_back(path);
    }
    std::sort(cleaned.begin(), cleaned.end());
    cleaned.erase(std::unique(cleaned.begin(), cleaned.end()), cleaned.end());
    files = std::move(cleaned);

    const uint64_t totalUnits = static_cast<uint64_t>(files.size()) +
                                static_cast<uint64_t>(sqlJobs.size()) +
                                static_cast<uint64_t>(registryJobs.size()) +
                                (userassist ? 1u : 0u) + (media ? 1u : 0u);
    task::SetProgress("privacy", 0, totalUnits);
    if (task::IsCancelled("privacy")) {
        failed.push_back("已取消");
        return Json{{"files", 0}, {"sql", 0}, {"registry", 0}, {"failed", failed}};
    }

    if (!files.empty()) {
        const std::vector<std::string> recycleFailed = recycle::SendToRecycle(files);
        for (const std::string& path : files) {
            task::AddDone("privacy", 1);
            const bool inFailed =
                std::find(recycleFailed.begin(), recycleFailed.end(), path) !=
                recycleFailed.end();
            if (inFailed || PathExists(path)) {
                failed.push_back("文件未能移入回收站：" + path);
            } else {
                ++filesDone;
            }
        }
    }

    if (task::IsCancelled("privacy")) {
        failed.push_back("已取消");
        return Json{{"files", filesDone},
                    {"sql", sqlDone},
                    {"registry", registryDone},
                    {"failed", failed}};
    }
    for (const auto& [path, statements] : sqlJobs) {
        task::AddDone("privacy", 1);
        if (ApplySql(path, statements)) {
            ++sqlDone;
        } else {
            failed.push_back("数据库清理失败或数据源不可用：" + path);
        }
    }

    if (task::IsCancelled("privacy")) {
        failed.push_back("已取消");
        return Json{{"files", filesDone},
                    {"sql", sqlDone},
                    {"registry", registryDone},
                    {"failed", failed}};
    }
    for (const std::string& job : registryJobs) {
        task::AddDone("privacy", 1);
        if (job == "reg_file") {
            int64_t deleted = 0;
            for (const std::string& path : RecentDocsPaths()) {
                deleted += ClearValues("HKCU", path, "recentdocs");
            }
            registryDone += deleted;
            if (deleted == 0) {
                failed.push_back("文件访问记录清理未完成：没有记录被清除");
            }
        }
    }

    if (userassist) {
        task::AddDone("privacy", 1);
        int64_t deleted = 0;
        for (const std::string& path : UserAssistCountKeys()) {
            deleted += ClearValues("HKCU", path, "userassist");
        }
        registryDone += deleted;
        if (deleted == 0) {
            failed.push_back("软件使用痕迹清理未完成：没有记录被清除");
        }
    }

    if (media) {
        task::AddDone("privacy", 1);
        registryDone += ClearValues("HKCU", kWmpRecent, "wmp");
    }

    logs::WriteLog({"开始清理", "清理：隐私痕迹",
                    "完成：文件 " + std::to_string(filesDone) + " 个，数据库 " +
                        std::to_string(sqlDone) + " 个，注册表记录 " +
                        std::to_string(registryDone) + " 条"});
    return Json{{"files", filesDone},
                {"sql", sqlDone},
                {"registry", registryDone},
                {"failed", failed}};
}

}  // namespace privacy
