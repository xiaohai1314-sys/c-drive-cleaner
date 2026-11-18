#include "core/rules.h"

#include <windows.h>
#include <winhttp.h>

#include <algorithm>
#include <cctype>
#include <cstdint>
#include <optional>
#include <stdexcept>
#include <string>
#include <string_view>
#include <unordered_set>
#include <vector>

#include "blake3.h"
#include "core/safety.h"
#include "core/task.h"
#include "util/env.h"
#include "util/path.h"
#include "util/resource_ids.h"
#include "util/utf.h"
#include "util/win32.h"

namespace {

constexpr size_t kMaxBytes = 1000000;
constexpr size_t kMaxRules = 200;

const char* const kTiers[] = {"conservative", "standard", "deep"};
const char* const kLibraryParts[] = {"documents", "desktop", "downloads",
                                     "my documents", "文档", "桌面"};

std::string ToLower(std::string_view text) {
    std::string out(text);
    for (char& ch : out) {
        ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    }
    return out;
}

std::string TrimSpace(std::string_view text) {
    size_t start = 0;
    size_t end = text.size();
    while (start < end &&
           std::isspace(static_cast<unsigned char>(text[start])) != 0) {
        ++start;
    }
    while (end > start &&
           std::isspace(static_cast<unsigned char>(text[end - 1])) != 0) {
        --end;
    }
    return std::string(text.substr(start, end - start));
}

bool StartsWith(const std::string& text, std::string_view prefix) {
    return text.size() >= prefix.size() &&
           text.compare(0, prefix.size(), prefix) == 0;
}

bool FileExists(const std::string& path) {
    return GetFileAttributesW(Utf8ToWide(path).c_str()) !=
           INVALID_FILE_ATTRIBUTES;
}

void EnsureDirectory(const std::string& path) {
    if (!path.empty()) {
        CreateDirectoryW(Utf8ToWide(path).c_str(), nullptr);
    }
}

std::optional<std::string> ReadFileText(const std::string& path) {
    HANDLE handle = CreateFileW(Utf8ToWide(path).c_str(), GENERIC_READ,
                                FILE_SHARE_READ, nullptr, OPEN_EXISTING,
                                FILE_ATTRIBUTE_NORMAL, nullptr);
    if (handle == INVALID_HANDLE_VALUE) {
        return std::nullopt;
    }
    LARGE_INTEGER size = {};
    if (GetFileSizeEx(handle, &size) == 0 || size.QuadPart > (16 << 20)) {
        CloseHandle(handle);
        return std::nullopt;
    }
    std::string data(static_cast<size_t>(size.QuadPart), '\0');
    DWORD read = 0;
    const bool ok =
        ReadFile(handle, data.data(), static_cast<DWORD>(data.size()), &read,
                 nullptr) != 0;
    CloseHandle(handle);
    if (!ok) {
        return std::nullopt;
    }
    data.resize(read);
    return data;
}

bool WriteFileText(const std::string& path, const std::string& text) {
    HANDLE handle = CreateFileW(Utf8ToWide(path).c_str(), GENERIC_WRITE, 0,
                                nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL,
                                nullptr);
    if (handle == INVALID_HANDLE_VALUE) {
        return false;
    }
    DWORD written = 0;
    const bool ok = WriteFile(handle, text.data(),
                              static_cast<DWORD>(text.size()), &written,
                              nullptr) != 0 &&
                    written == text.size();
    CloseHandle(handle);
    return ok;
}

std::string Blake3Hex(std::string_view data) {
    blake3_hasher hasher;
    blake3_hasher_init(&hasher);
    blake3_hasher_update(&hasher, data.data(), data.size());
    uint8_t digest[BLAKE3_OUT_LEN];
    blake3_hasher_finalize(&hasher, digest, sizeof(digest));
    static const char* hex = "0123456789abcdef";
    std::string result;
    result.reserve(sizeof(digest) * 2);
    for (uint8_t byte : digest) {
        result.push_back(hex[byte >> 4]);
        result.push_back(hex[byte & 0x0F]);
    }
    return result;
}

struct InternetHandle {
    HINTERNET handle = nullptr;
    InternetHandle() = default;
    ~InternetHandle() {
        if (handle != nullptr) {
            WinHttpCloseHandle(handle);
        }
    }
    InternetHandle(const InternetHandle&) = delete;
    InternetHandle& operator=(const InternetHandle&) = delete;
};

std::string HttpGet(const std::string& url, DWORD timeoutMs, size_t maxBytes) {
    const std::wstring wideUrl = Utf8ToWide(url);
    URL_COMPONENTS components = {};
    components.dwStructSize = sizeof(components);
    components.dwSchemeLength = static_cast<DWORD>(-1);
    components.dwHostNameLength = static_cast<DWORD>(-1);
    components.dwUrlPathLength = static_cast<DWORD>(-1);
    components.dwExtraInfoLength = static_cast<DWORD>(-1);
    if (WinHttpCrackUrl(wideUrl.c_str(), 0, 0, &components) == 0) {
        throw std::runtime_error("下载失败：地址格式不正确");
    }
    if (components.nScheme != INTERNET_SCHEME_HTTPS) {
        throw std::runtime_error("下载失败：只支持 https");
    }
    const std::wstring host(components.lpszHostName,
                            components.dwHostNameLength);
    std::wstring path(components.lpszUrlPath, components.dwUrlPathLength);
    if (components.dwExtraInfoLength > 0) {
        path.append(components.lpszExtraInfo, components.dwExtraInfoLength);
    }

    InternetHandle session;
    session.handle = WinHttpOpen(
        L"CDriveCleanerCpp/0.1", WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
        WINHTTP_NO_PROXY_NAME, WINHTTP_NO_PROXY_BYPASS, 0);
    if (session.handle == nullptr) {
        throw std::runtime_error("下载失败：" + FormatWin32Error(GetLastError()));
    }
    WinHttpSetTimeouts(session.handle, static_cast<int>(timeoutMs),
                       static_cast<int>(timeoutMs),
                       static_cast<int>(timeoutMs),
                       static_cast<int>(timeoutMs));

    InternetHandle connect;
    connect.handle =
        WinHttpConnect(session.handle, host.c_str(), components.nPort, 0);
    if (connect.handle == nullptr) {
        throw std::runtime_error("下载失败：" + FormatWin32Error(GetLastError()));
    }

    InternetHandle request;
    request.handle = WinHttpOpenRequest(
        connect.handle, L"GET", path.c_str(), nullptr, WINHTTP_NO_REFERER,
        WINHTTP_DEFAULT_ACCEPT_TYPES, WINHTTP_FLAG_SECURE);
    if (request.handle == nullptr) {
        throw std::runtime_error("下载失败：" + FormatWin32Error(GetLastError()));
    }
    if (WinHttpSendRequest(request.handle, WINHTTP_NO_ADDITIONAL_HEADERS, 0,
                           WINHTTP_NO_REQUEST_DATA, 0, 0, 0) == 0 ||
        WinHttpReceiveResponse(request.handle, nullptr) == 0) {
        throw std::runtime_error("下载失败：" + FormatWin32Error(GetLastError()));
    }

    DWORD status = 0;
    DWORD statusSize = sizeof(status);
    if (WinHttpQueryHeaders(
            request.handle,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            WINHTTP_HEADER_NAME_BY_INDEX, &status, &statusSize,
            WINHTTP_NO_HEADER_INDEX) == 0) {
        throw std::runtime_error("下载失败：" + FormatWin32Error(GetLastError()));
    }
    if (status < 200 || status >= 300) {
        throw std::runtime_error("下载失败：HTTP " + std::to_string(status));
    }

    std::string body;
    for (;;) {
        DWORD available = 0;
        if (WinHttpQueryDataAvailable(request.handle, &available) == 0) {
            throw std::runtime_error("读取失败：" +
                                     FormatWin32Error(GetLastError()));
        }
        if (available == 0) {
            break;
        }
        std::string chunk(available, '\0');
        DWORD read = 0;
        if (WinHttpReadData(request.handle, chunk.data(), available, &read) ==
            0) {
            throw std::runtime_error("读取失败：" +
                                     FormatWin32Error(GetLastError()));
        }
        body.append(chunk.data(), read);
        if (body.size() > maxBytes) {
            throw std::runtime_error("规则文件过大");
        }
    }
    return body;
}

std::vector<std::string> JsonStringList(const Json& object, const char* key) {
    std::vector<std::string> out;
    const auto found = object.find(key);
    if (found == object.end() || !found->is_array()) {
        return out;
    }
    for (const Json& item : *found) {
        if (item.is_string()) {
            out.push_back(item.get<std::string>());
        }
    }
    return out;
}

Rule ParseRule(const Json& item) {
    Rule rule;
    if (!item.is_object()) {
        return rule;
    }
    auto readString = [&](const char* key) {
        const auto found = item.find(key);
        return (found != item.end() && found->is_string())
                   ? found->get<std::string>()
                   : std::string();
    };
    rule.key = readString("key");
    rule.name = readString("name");
    rule.tier = readString("tier");
    rule.kind = readString("kind");
    rule.describe = readString("describe");
    rule.warning = readString("warning");
    rule.note = readString("note");
    rule.paths = JsonStringList(item, "paths");
    rule.glob_dirs = JsonStringList(item, "glob_dirs");
    rule.patterns = JsonStringList(item, "patterns");
    rule.exclude = JsonStringList(item, "exclude");
    const auto age = item.find("min_age_minutes");
    if (age != item.end() && age->is_number_integer()) {
        rule.min_age_minutes = age->get<uint64_t>();
    }
    const auto confirm = item.find("requires_confirm");
    if (confirm != item.end() && confirm->is_boolean()) {
        rule.requires_confirm = confirm->get<bool>();
    }
    const auto prune = item.find("prune_dirs");
    if (prune != item.end() && prune->is_boolean()) {
        rule.prune_dirs = prune->get<bool>();
    }
    return rule;
}

std::string ConfigPath() {
    const std::string exeDir = ExeDirUtf8();
    const std::string direct = JoinPath(exeDir, "rules_update.json");
    if (FileExists(direct)) {
        return direct;
    }
    const std::string resources =
        JoinPath(JoinPath(exeDir, "resources"), "rules_update.json");
    return FileExists(resources) ? resources : std::string();
}

std::string ConfigUrl() {
    const std::string path = ConfigPath();
    if (path.empty()) {
        return {};
    }
    const auto text = ReadFileText(path);
    if (!text.has_value()) {
        return {};
    }
    const Json value = Json::parse(*text, nullptr, false);
    if (!value.is_object()) {
        return {};
    }
    const auto url = value.find("url");
    if (url == value.end() || !url->is_string()) {
        return {};
    }
    return TrimSpace(url->get<std::string>());
}

std::optional<std::string> ConfigHash() {
    const std::string path = ConfigPath();
    if (path.empty()) {
        return std::nullopt;
    }
    const auto text = ReadFileText(path);
    if (!text.has_value()) {
        return std::nullopt;
    }
    const Json value = Json::parse(*text, nullptr, false);
    if (!value.is_object()) {
        return std::nullopt;
    }
    const auto hash = value.find("blake3");
    if (hash == value.end() || !hash->is_string()) {
        return std::nullopt;
    }
    std::string normalized = ToLower(TrimSpace(hash->get<std::string>()));
    if (normalized.size() != 64) {
        return std::nullopt;
    }
    return normalized;
}

bool KeyOk(const std::string& key) {
    if (key.empty() || key.size() > 80) {
        return false;
    }
    for (char ch : key) {
        const unsigned char byte = static_cast<unsigned char>(ch);
        if (std::isalnum(byte) == 0 && ch != '_' && ch != '-') {
            return false;
        }
    }
    return true;
}

void PathAllowed(const std::string& raw) {
    if (TrimSpace(raw).empty()) {
        throw std::runtime_error("路径为空");
    }
    if (raw.size() > 500) {
        throw std::runtime_error("路径含有非法字符");
    }
    for (char ch : raw) {
        if (static_cast<unsigned char>(ch) < 32) {
            throw std::runtime_error("路径含有非法字符");
        }
    }
    const std::string expanded = ExpandEnv(raw);
    if (safety::PathSyntaxError(expanded)) {
        throw std::runtime_error("路径格式不合法");
    }
    std::string lower = ToLower(expanded);
    for (char& ch : lower) {
        if (ch == '/') {
            ch = '\\';
        }
    }
    size_t start = 0;
    while (start <= lower.size()) {
        const size_t slash = lower.find('\\', start);
        const std::string part =
            slash == std::string::npos ? lower.substr(start)
                                       : lower.substr(start, slash - start);
        if (!part.empty()) {
            for (const char* library : kLibraryParts) {
                if (part == library) {
                    throw std::runtime_error("远程规则不能指向文档、桌面或下载");
                }
            }
        }
        if (slash == std::string::npos) {
            break;
        }
        start = slash + 1;
    }
    const std::string name = ToLower(safety::Basename(expanded));
    if (safety::IsPasswordStore(name) ||
        name.find("login data") != std::string::npos) {
        throw std::runtime_error("远程规则不能删除浏览器密码库");
    }
    if (!safety::UnderRemoteAllowlist(expanded)) {
        throw std::runtime_error("远程规则只能指向临时目录或固定 catalog 中的目录");
    }
}

void ValidateRule(const Json& item) {
    if (!item.is_object()) {
        throw std::runtime_error("规则格式不正确");
    }
    const auto keyIt = item.find("key");
    const std::string key =
        (keyIt != item.end() && keyIt->is_string())
            ? keyIt->get<std::string>()
            : std::string();
    if (!KeyOk(key)) {
        throw std::runtime_error("规则 key 不合法");
    }
    const auto kindIt = item.find("kind");
    const std::string kind =
        (kindIt != item.end() && kindIt->is_string())
            ? kindIt->get<std::string>()
            : std::string("files");
    if (kind != "files") {
        throw std::runtime_error("规则 " + key + " 不是文件清理类型");
    }
    const auto tierIt = item.find("tier");
    const std::string tier =
        (tierIt != item.end() && tierIt->is_string())
            ? tierIt->get<std::string>()
            : std::string("standard");
    bool tierOk = false;
    for (const char* allowed : kTiers) {
        if (tier == allowed) {
            tierOk = true;
            break;
        }
    }
    if (!tierOk) {
        throw std::runtime_error("规则 " + key + " 的档位不合法");
    }
    const auto command = item.find("command");
    if (command != item.end()) {
        if (!command->is_array() || !command->empty()) {
            throw std::runtime_error("规则 " + key + " 不能带命令");
        }
    }
    for (const char* field : {"paths", "glob_dirs"}) {
        const auto list = item.find(field);
        if (list == item.end()) {
            continue;
        }
        if (!list->is_array()) {
            throw std::runtime_error("规则 " + key + " 的 " + field +
                                     " 格式不正确");
        }
        for (const Json& raw : *list) {
            if (!raw.is_string()) {
                throw std::runtime_error("规则 " + key + " 的 " + field +
                                         " 含有非字符串");
            }
            try {
                PathAllowed(raw.get<std::string>());
            } catch (const std::runtime_error& error) {
                throw std::runtime_error("规则 " + key + "：" + error.what());
            }
        }
    }
}

std::string BackupPath() {
    std::string base = LocalAppDataDir();
    if (base.empty()) {
        base = ".";
    }
    return JoinPath(base, "rules_remote.previous.json");
}

}  // namespace

std::vector<Rule> AllRules() {
    std::vector<Rule> rules;
    std::unordered_set<std::string> known;
    const std::string rulesJson = LoadResourceText(IDR_RULES_JSON);
    const Json data = Json::parse(rulesJson, nullptr, false);
    if (data.is_object()) {
        const auto categories = data.find("categories");
        if (categories != data.end() && categories->is_array()) {
            for (const Json& item : *categories) {
                Rule rule = ParseRule(item);
                known.insert(rule.key);
                rules.push_back(std::move(rule));
            }
        }
    }
    const auto remote = ReadFileText(RemoteRulesPath());
    if (remote.has_value()) {
        const Json extra = Json::parse(*remote, nullptr, false);
        if (extra.is_object()) {
            const auto categories = extra.find("categories");
            if (categories != extra.end() && categories->is_array()) {
                for (const Json& item : *categories) {
                    Rule rule = ParseRule(item);
                    if (!rule.key.empty() && known.insert(rule.key).second) {
                        rules.push_back(std::move(rule));
                    }
                }
            }
        }
    }
    return rules;
}

std::vector<std::string> ExpandPaths(const std::vector<std::string>& raw) {
    std::vector<std::string> out;
    std::unordered_set<std::string> seen;
    for (const std::string& item : raw) {
        const std::string expanded = ExpandEnv(item);
        std::vector<std::string> matches;
        if (expanded.find('*') != std::string::npos ||
            expanded.find('?') != std::string::npos) {
            matches = GlobFilesystem(expanded);
        } else if (PathExists(expanded)) {
            matches.push_back(expanded);
        }
        for (const std::string& path : matches) {
            if (!PathExists(path)) {
                continue;
            }
            const std::string key = ToLower(NormalizePath(path));
            if (seen.insert(key).second) {
                out.push_back(path);
            }
        }
    }
    return out;
}

std::vector<GlobPattern> BuildMatcher(
    const std::vector<std::string>& patterns) {
    std::vector<std::string> source = patterns;
    if (source.empty()) {
        source.push_back("*");
    }
    std::vector<GlobPattern> matcher;
    matcher.reserve(source.size());
    for (const std::string& pattern : source) {
        matcher.emplace_back(ToLower(pattern));
    }
    return matcher;
}

bool IncludeMatches(const std::string& lowerName,
                    const std::vector<GlobPattern>& include,
                    const std::vector<GlobPattern>& exclude) {
    bool included = false;
    for (const GlobPattern& pattern : include) {
        if (pattern.matches(lowerName)) {
            included = true;
            break;
        }
    }
    if (!included) {
        return false;
    }
    for (const GlobPattern& pattern : exclude) {
        if (pattern.matches(lowerName)) {
            return false;
        }
    }
    return true;
}

bool AgeOk(uint64_t mtimeUnix, uint64_t minAgeSeconds, uint64_t nowUnix) {
    if (minAgeSeconds == 0 || mtimeUnix == 0) {
        return true;
    }
    if (nowUnix < mtimeUnix) {
        return true;
    }
    return nowUnix - mtimeUnix >= minAgeSeconds;
}

std::string RemoteRulesPath() {
    std::string base = LocalAppDataDir();
    if (base.empty()) {
        base = ".";
    }
    EnsureDirectory(base);
    return JoinPath(base, "rules_remote.json");
}

std::string RulesUpdate() {
    task::Begin("rules", "规则更新", false, false, 0, "");
    task::SetStage("rules", "下载规则");
    try {
        const std::string url = ConfigUrl();
        if (!StartsWith(url, "https://")) {
            throw std::runtime_error(
                "未配置规则更新地址（在 rules_update.json 里填写 https 地址）");
        }
        const std::string body = HttpGet(url, 20000, kMaxBytes);
        const auto expected = ConfigHash();
        if (expected.has_value() && Blake3Hex(body) != *expected) {
            throw std::runtime_error("规则完整性校验失败，未保存更新");
        }
        const Json parsed = Json::parse(body, nullptr, false);
        if (!parsed.is_object()) {
            throw std::runtime_error("规则格式不正确");
        }
        const auto categories = parsed.find("categories");
        if (categories == parsed.end() || !categories->is_array()) {
            throw std::runtime_error("规则文件缺少 categories");
        }
        if (categories->size() > kMaxRules) {
            throw std::runtime_error("远程规则过多（上限 200 条）");
        }
        Json accepted = Json::array();
        for (const Json& item : *categories) {
            ValidateRule(item);
            accepted.push_back(item);
        }
        const std::string path = RemoteRulesPath();
        uint64_t oldVersion = 0;
        const auto oldText = ReadFileText(path);
        if (oldText.has_value()) {
            const Json old = Json::parse(*oldText, nullptr, false);
            if (old.is_object()) {
                const auto version = old.find("version");
                if (version != old.end() && version->is_number_integer()) {
                    oldVersion = version->get<uint64_t>();
                }
            }
        }
        uint64_t incomingVersion = 1;
        const auto version = parsed.find("version");
        if (version != parsed.end() && version->is_number_integer()) {
            incomingVersion = version->get<uint64_t>();
        }
        if (incomingVersion < oldVersion) {
            throw std::runtime_error("规则版本过旧：当前 " +
                                     std::to_string(oldVersion) + "，收到 " +
                                     std::to_string(incomingVersion));
        }
        Json payload;
        payload["version"] = incomingVersion;
        payload["categories"] = accepted;
        const std::string encoded =
            payload.dump(2, ' ', false, Json::error_handler_t::replace);
        const std::string temp = path + ".tmp";
        if (!WriteFileText(temp, encoded)) {
            throw std::runtime_error("写入临时文件失败：" +
                                     FormatWin32Error(GetLastError()));
        }
        if (FileExists(path)) {
            CopyFileW(Utf8ToWide(path).c_str(),
                      Utf8ToWide(BackupPath()).c_str(), FALSE);
        }
        if (MoveFileExW(Utf8ToWide(temp).c_str(), Utf8ToWide(path).c_str(),
                        MOVEFILE_REPLACE_EXISTING) == 0) {
            throw std::runtime_error("替换规则文件失败：" +
                                     FormatWin32Error(GetLastError()));
        }
        const size_t total = AllRules().size();
        const std::string message =
            "新增 " + std::to_string(accepted.size()) + " 条，共 " +
            std::to_string(total) + " 条。已保存到 " + path + "，版本 " +
            std::to_string(incomingVersion) + "\n下次扫描生效。";
        task::Finish("rules", "ok", message);
        return message;
    } catch (const std::exception& error) {
        task::Finish("rules", "failed", error.what());
        throw;
    }
}

std::string RulesRollback() {
    const std::string backup = BackupPath();
    const auto text = ReadFileText(backup);
    if (!text.has_value()) {
        throw std::runtime_error("没有可回滚的规则版本");
    }
    const Json value = Json::parse(*text, nullptr, false);
    if (!value.is_object()) {
        throw std::runtime_error("备份规则损坏");
    }
    const auto categories = value.find("categories");
    if (categories == value.end() || !categories->is_array()) {
        throw std::runtime_error("备份规则缺少 categories");
    }
    if (categories->size() > kMaxRules) {
        throw std::runtime_error("备份规则数量超限");
    }
    for (const Json& item : *categories) {
        ValidateRule(item);
    }
    const std::string path = RemoteRulesPath();
    if (CopyFileW(Utf8ToWide(backup).c_str(), Utf8ToWide(path).c_str(),
                  FALSE) == 0) {
        throw std::runtime_error("回滚失败：" +
                                 FormatWin32Error(GetLastError()));
    }
    uint64_t version = 0;
    const auto versionIt = value.find("version");
    if (versionIt != value.end() && versionIt->is_number_integer()) {
        version = versionIt->get<uint64_t>();
    }
    return "已回滚到规则版本 " + std::to_string(version);
}
