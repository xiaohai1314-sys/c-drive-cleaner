#include "core/im.h"

#include <windows.h>
#include <shlobj.h>

#include <algorithm>
#include <cctype>
#include <cstdint>
#include <cstdio>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <string>
#include <unordered_set>
#include <utility>
#include <vector>

#include "core/dups.h"
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

constexpr uint64_t kMaxImageBytes = 64 * 1024 * 1024;
constexpr uint64_t kPreviewLimit = 64 * 1024;

const char* const kVideoExt[] = {".mp4", ".avi", ".mov", ".mkv",  ".wmv",
                                 ".flv", ".m4v", ".3gp", ".mpeg", ".mpg",
                                 ".ts",  ".webm", ".rmvb", ".vob", ".m2ts"};
const char* const kImageExt[] = {".jpg",  ".jpeg", ".png", ".gif",  ".bmp",
                                 ".webp", ".heic", ".tif", ".tiff", ".ico",
                                 ".raw",  ".psd",  ".svg", ".jfif"};
const char* const kAudioExt[] = {".silk", ".amr", ".mp3",  ".m4a",
                                 ".wav",  ".ogg", ".aac", ".aud"};
const char* const kDocExt[] = {".pdf", ".doc",  ".docx", ".xls",  ".xlsx",
                               ".ppt", ".pptx", ".txt",  ".csv",  ".rtf",
                               ".ofd", ".md",   ".wps",  ".et",   ".dps",
                               ".dwg", ".dxf",  ".xmind"};
const char* const kArchiveExt[] = {".zip", ".rar", ".7z",  ".tar", ".gz",
                                   ".bz2", ".xz",  ".iso", ".cab"};
const char* const kFileExt[] = {".psd", ".ai"};
const char* const kSkipExt[] = {".db",   ".db-wal", ".db-shm", ".sqlite",
                                ".sqlite3", ".ini",  ".key",    ".xml",
                                ".json", ".pfx",   ".crt",     ".exe",
                                ".dll",  ".cfg"};
const char* const kSkipDirParts[] = {"db_storage", "config",  "account",
                                     "apputil",    "login",   "db",
                                     "database",   "databases"};

struct MediaDir {
    const char* kind;
    const char* const* names;
    size_t count;
};

const char* const kSnsDirs[] = {"sns", "moments", "timeline"};
const char* const kVideoDirs[] = {"video", "videos"};
const char* const kImageDirs[] = {"image",  "images", "img",    "attach",
                                  "msgattach", "custom_emotion", "emotion"};
const char* const kAudioDirs[] = {"voice", "audio", "ptt", "sound"};
const char* const kFileDirs[] = {"file", "files", "filerecv", "file_recv",
                                 "fav",  "favorite"};
const char* const kLogDirs[] = {"log", "logs", "xlog", "crash", "crashinfo"};
const char* const kCacheDirs[] = {"cache", "temp",   "emoji",
                                  "emoticon", "cdncomm"};

template <size_t N>
size_t CountOf(const char* const (&)[N]) {
    return N;
}

const MediaDir kMediaDirs[] = {
    {"sns", kSnsDirs, CountOf(kSnsDirs)},
    {"video", kVideoDirs, CountOf(kVideoDirs)},
    {"image", kImageDirs, CountOf(kImageDirs)},
    {"audio", kAudioDirs, CountOf(kAudioDirs)},
    {"file", kFileDirs, CountOf(kFileDirs)},
    {"log", kLogDirs, CountOf(kLogDirs)},
    {"cache", kCacheDirs, CountOf(kCacheDirs)},
};

template <size_t N>
bool InList(const char* const (&values)[N], const std::string& needle) {
    for (size_t index = 0; index < N; ++index) {
        if (needle == values[index]) {
            return true;
        }
    }
    return false;
}

std::string LowerAscii(std::string text) {
    for (char& ch : text) {
        ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    }
    return text;
}

bool EqualsIgnoreCase(const std::string& left, const std::string& right) {
    return LowerAscii(left) == LowerAscii(right);
}

std::string TrimAscii(const std::string& text) {
    size_t begin = 0;
    size_t end = text.size();
    while (begin < end &&
           std::isspace(static_cast<unsigned char>(text[begin])) != 0) {
        ++begin;
    }
    while (end > begin &&
           std::isspace(static_cast<unsigned char>(text[end - 1])) != 0) {
        --end;
    }
    return text.substr(begin, end - begin);
}

std::string TrimEndSlashes(std::string text) {
    while (!text.empty() && (text.back() == '\\' || text.back() == '/')) {
        text.pop_back();
    }
    return text;
}

bool IsDirectory(const std::string& path) {
    const DWORD attributes = GetFileAttributesW(Utf8ToWide(path).c_str());
    return attributes != INVALID_FILE_ATTRIBUTES &&
           (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
}

std::string Utf8Lossy(const std::string& text) {
    if (IsValidUtf8(text)) {
        return text;
    }
    std::string out;
    out.reserve(text.size() + 8);
    size_t index = 0;
    while (index < text.size()) {
        const unsigned char ch = static_cast<unsigned char>(text[index]);
        size_t extra = 0;
        bool valid = true;
        if (ch < 0x80) {
            out.push_back(text[index]);
            ++index;
            continue;
        }
        if ((ch & 0xE0) == 0xC0) {
            extra = 1;
            valid = ch >= 0xC2;
        } else if ((ch & 0xF0) == 0xE0) {
            extra = 2;
        } else if ((ch & 0xF8) == 0xF0) {
            extra = 3;
            valid = ch <= 0xF4;
        } else {
            valid = false;
        }
        if (valid && index + extra < text.size()) {
            for (size_t offset = 1; offset <= extra; ++offset) {
                if ((static_cast<unsigned char>(text[index + offset]) & 0xC0) !=
                    0x80) {
                    valid = false;
                    break;
                }
            }
        } else {
            valid = false;
        }
        if (valid) {
            out.append(text, index, extra + 1);
            index += extra + 1;
        } else {
            out.append("\xEF\xBF\xBD");
            ++index;
        }
    }
    return out;
}

std::string DocumentsDir() {
    wchar_t buffer[260] = {};
    const HRESULT result = SHGetFolderPathW(nullptr, CSIDL_PERSONAL, nullptr, 0,
                                            buffer);
    if (SUCCEEDED(result)) {
        const std::string path = WideToUtf8(buffer);
        if (!path.empty() && PathExists(path)) {
            return path;
        }
    }
    return GetEnv("USERPROFILE") + "\\Documents";
}

std::vector<std::string> SplitWhitespace(const std::string& text) {
    std::vector<std::string> parts;
    size_t index = 0;
    while (index < text.size()) {
        while (index < text.size() &&
               std::isspace(static_cast<unsigned char>(text[index])) != 0) {
            ++index;
        }
        const size_t start = index;
        while (index < text.size() &&
               std::isspace(static_cast<unsigned char>(text[index])) == 0) {
            ++index;
        }
        if (index > start) {
            parts.push_back(text.substr(start, index - start));
        }
    }
    return parts;
}

std::optional<std::string> WechatRegistryPathValue() {
    const ProcessResult result =
        RunCapture(Utf8ToWide(System32Path("reg.exe")),
                   {L"query", L"HKCU\\Software\\Tencent\\WeChat", L"/v",
                    L"FileSavePath"});
    if (!result.Success()) {
        return std::nullopt;
    }
    const std::string text = Utf8Lossy(result.out);
    size_t begin = 0;
    while (begin <= text.size()) {
        const size_t end = text.find('\n', begin);
        const std::string line =
            text.substr(begin, end == std::string::npos ? std::string::npos
                                                        : end - begin);
        if (line.find("FileSavePath") != std::string::npos) {
            const std::vector<std::string> parts = SplitWhitespace(line);
            if (parts.size() >= 3) {
                std::string path = parts[2];
                for (size_t index = 3; index < parts.size(); ++index) {
                    path += " " + parts[index];
                }
                if (path != "MyDocument:") {
                    return path;
                }
            }
        }
        if (end == std::string::npos) {
            break;
        }
        begin = end + 1;
    }
    return std::nullopt;
}

std::optional<std::string> WechatIniPathValue() {
    const std::string ini = GetEnv("APPDATA") +
                            "\\Tencent\\WeChat\\All Users\\config\\"
                            "3ebffe94.ini";
    FILE* file = nullptr;
    if (_wfopen_s(&file, Utf8ToWide(ini).c_str(), L"rb") != 0 ||
        file == nullptr) {
        return std::nullopt;
    }
    std::string content;
    char buffer[4096];
    size_t read = 0;
    while ((read = fread(buffer, 1, sizeof(buffer), file)) > 0) {
        content.append(buffer, read);
    }
    fclose(file);
    if (!IsValidUtf8(content)) {
        return std::nullopt;
    }
    size_t begin = 0;
    while (begin <= content.size()) {
        const size_t end = content.find('\n', begin);
        const std::string line =
            content.substr(begin, end == std::string::npos ? std::string::npos
                                                           : end - begin);
        const size_t equals = line.find('=');
        if (equals != std::string::npos) {
            if (EqualsIgnoreCase(TrimAscii(line.substr(0, equals)),
                                 "FileSavePath")) {
                const std::string value = TrimAscii(line.substr(equals + 1));
                if (value != "MyDocument:") {
                    return value;
                }
            }
        }
        if (end == std::string::npos) {
            break;
        }
        begin = end + 1;
    }
    return std::nullopt;
}

std::optional<std::string> ResolveWechatRoot(std::optional<std::string> value) {
    if (!value.has_value()) {
        return std::nullopt;
    }
    if (IsDirectory(*value)) {
        return value;
    }
    const std::string sub = *value + "\\WeChat Files";
    if (IsDirectory(sub)) {
        return sub;
    }
    return std::nullopt;
}

std::vector<std::string> DefaultRootCandidates(const std::string& kind) {
    const std::string roaming = GetEnv("APPDATA");
    const std::string documents = DocumentsDir();
    if (kind == "wechat") {
        return {roaming + "\\Tencent\\WeChat", roaming + "\\Tencent\\xwechat",
                documents + "\\WeChat Files", documents + "\\xwechat_files"};
    }
    return {roaming + "\\Tencent\\QQ", roaming + "\\Tencent\\QQNT",
            documents + "\\Tencent Files"};
}

void AppendWechatMigrationCandidates(std::vector<std::string>& candidates,
                                     const std::string& value) {
    candidates.push_back(value);
    candidates.push_back(value + "\\WeChat Files");
}

std::vector<std::string> PartsOf(const std::string& path) {
    std::string normalized = LowerAscii(path);
    for (char& ch : normalized) {
        if (ch == '/') {
            ch = '\\';
        }
    }
    std::vector<std::string> parts;
    size_t begin = 0;
    while (begin <= normalized.size()) {
        const size_t end = normalized.find('\\', begin);
        const std::string part =
            normalized.substr(begin, end == std::string::npos
                                         ? std::string::npos
                                         : end - begin);
        if (!part.empty()) {
            parts.push_back(part);
        }
        if (end == std::string::npos) {
            break;
        }
        begin = end + 1;
    }
    return parts;
}

std::vector<std::string> RelativeParts(const std::string& path,
                                       const std::string& root) {
    std::vector<std::string> full = PartsOf(path);
    if (!root.empty()) {
        const std::vector<std::string> base = PartsOf(root);
        if (full.size() > base.size() &&
            std::equal(base.begin(), base.end(), full.begin())) {
            return std::vector<std::string>(full.begin() + base.size(),
                                            full.end());
        }
    }
    return full;
}

std::string ExtOf(const std::string& path) {
    const size_t slash = path.find_last_of("\\/");
    const std::string name =
        slash == std::string::npos ? path : path.substr(slash + 1);
    const size_t dot = name.find_last_of('.');
    if (dot == std::string::npos) {
        return {};
    }
    return LowerAscii(name.substr(dot));
}

std::optional<std::string> DirKind(const std::vector<std::string>& dirs,
                                   const std::string& ext) {
    for (const MediaDir& media : kMediaDirs) {
        bool matched = false;
        for (const std::string& dir : dirs) {
            for (size_t index = 0; index < media.count; ++index) {
                if (dir == media.names[index]) {
                    matched = true;
                    break;
                }
            }
            if (matched) {
                break;
            }
        }
        if (!matched) {
            continue;
        }
        // 微信把视频封面图（.jpg）和视频放在同一个 Video 目录里。
        if (std::string(media.kind) == "video" && !InList(kVideoExt, ext)) {
            continue;
        }
        return std::string(media.kind);
    }
    return std::nullopt;
}

std::string ReceivedSubkind(const std::string& path) {
    const std::string ext = ExtOf(path);
    if (InList(kDocExt, ext)) {
        return "doc";
    }
    if (InList(kVideoExt, ext)) {
        return "video";
    }
    if (InList(kImageExt, ext)) {
        return "image";
    }
    if (InList(kAudioExt, ext)) {
        return "music";
    }
    if (InList(kArchiveExt, ext)) {
        return "archive";
    }
    return "other";
}

std::optional<std::pair<std::string, std::string>> ClassifyPath(
    const std::string& path, const std::string& root) {
    const std::vector<std::string> parts = RelativeParts(path, root);
    if (parts.empty()) {
        return std::nullopt;
    }
    const std::string ext = ExtOf(parts.back());
    if (InList(kSkipExt, ext)) {
        return std::nullopt;
    }
    std::vector<std::string> dirs(parts.begin(), parts.end() - 1);
    for (const std::string& dir : dirs) {
        if (InList(kSkipDirParts, dir)) {
            return std::nullopt;
        }
    }
    if (im::IsProgramRoot(root)) {
        const bool isLog = ext == ".log" || ext == ".xlog" || ext == ".dmp";
        return std::make_pair(isLog ? std::string("log") : std::string("cache"),
                              std::string());
    }
    std::string kind;
    if (const std::optional<std::string> byDir = DirKind(dirs, ext);
        byDir.has_value()) {
        kind = *byDir;
    } else if (ext.empty() || ext == ".dat" || ext == ".tmp") {
        return std::nullopt;
    } else if (InList(kVideoExt, ext)) {
        kind = "video";
    } else if (InList(kImageExt, ext)) {
        kind = "image";
    } else if (InList(kAudioExt, ext)) {
        kind = "audio";
    } else if (InList(kFileExt, ext) || InList(kArchiveExt, ext) ||
               InList(kDocExt, ext)) {
        kind = "file";
    } else if (ext == ".log" || ext == ".xlog" || ext == ".dmp") {
        kind = "log";
    } else {
        return std::nullopt;
    }
    const std::string sub =
        kind == "file" ? ReceivedSubkind(path) : std::string();
    return std::make_pair(kind, sub);
}

uint64_t FileTimeToUnix(const FILETIME& time) {
    const uint64_t fileTime =
        (static_cast<uint64_t>(time.dwHighDateTime) << 32) |
        static_cast<uint64_t>(time.dwLowDateTime);
    constexpr uint64_t kWindowsEpochDiffSecs = 11'644'473'600ULL;
    const uint64_t seconds = fileTime / 10'000'000ULL;
    return seconds > kWindowsEpochDiffSecs ? seconds - kWindowsEpochDiffSecs : 0;
}

template <typename OnFile>
void WalkFiles(const std::string& root, OnFile onFile) {
    std::vector<std::string> pending{root};
    while (!pending.empty()) {
        const std::string current = std::move(pending.back());
        pending.pop_back();
        WIN32_FIND_DATAW data;
        const HANDLE find =
            FindFirstFileW((Utf8ToWide(current) + L"\\*").c_str(), &data);
        if (find == INVALID_HANDLE_VALUE) {
            continue;
        }
        do {
            const std::wstring name(data.cFileName);
            if (name.empty() || name == L"." || name == L"..") {
                continue;
            }
            if ((data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0) {
                continue;
            }
            const std::string child = current + "\\" + WideToUtf8(name);
            if ((data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
                pending.push_back(child);
            } else {
                onFile(child, data);
            }
        } while (FindNextFileW(find, &data) != 0);
        FindClose(find);
    }
}

std::string BasenameOf(const std::string& path) {
    const size_t slash = path.find_last_of("\\/");
    return slash == std::string::npos ? path : path.substr(slash + 1);
}

std::mutex g_scannedMutex;
std::unordered_set<std::string> g_scanned;

std::optional<const char*> ImageMime(const std::vector<uint8_t>& head) {
    auto startsWith = [&head](const char* prefix, size_t length) {
        if (head.size() < length) {
            return false;
        }
        for (size_t index = 0; index < length; ++index) {
            if (head[index] != static_cast<uint8_t>(prefix[index])) {
                return false;
            }
        }
        return true;
    };
    if (head.size() >= 3 && head[0] == 0xFF && head[1] == 0xD8 &&
        head[2] == 0xFF) {
        return "image/jpeg";
    }
    if (startsWith("\x89PNG\r\n\x1a\n", 8)) {
        return "image/png";
    }
    if (startsWith("GIF87a", 6) || startsWith("GIF89a", 6)) {
        return "image/gif";
    }
    if (head.size() >= 12 && startsWith("RIFF", 4) &&
        head[8] == 'W' && head[9] == 'E' && head[10] == 'B' && head[11] == 'P') {
        return "image/webp";
    }
    if (startsWith("BM", 2)) {
        return "image/bmp";
    }
    return std::nullopt;
}

std::optional<std::pair<std::string, std::vector<uint8_t>>> ReadImage(
    const std::string& path) {
    WIN32_FILE_ATTRIBUTE_DATA info = {};
    if (GetFileAttributesExW(Utf8ToWide(path).c_str(), GetFileExInfoStandard,
                             &info) == 0) {
        return std::nullopt;
    }
    if ((info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
        return std::nullopt;
    }
    const uint64_t size = (static_cast<uint64_t>(info.nFileSizeHigh) << 32) |
                          static_cast<uint64_t>(info.nFileSizeLow);
    if (size > kMaxImageBytes) {
        return std::nullopt;
    }
    const HANDLE raw = CreateFileW(
        Utf8ToWide(path).c_str(), GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr,
        OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (raw == INVALID_HANDLE_VALUE) {
        return std::nullopt;
    }
    UniqueHandle file(raw);

    std::vector<uint8_t> head(16);
    DWORD read = 0;
    if (ReadFile(file.get(), head.data(), 16, &read, nullptr) == 0 ||
        read < 16) {
        return std::nullopt;
    }
    bool encrypted = false;
    if (!ImageMime(head).has_value()) {
        std::vector<uint8_t> probe = head;
        for (uint8_t& byte : probe) {
            byte ^= 0x05;
        }
        if (!ImageMime(probe).has_value()) {
            return std::nullopt;
        }
        encrypted = true;
    }
    LARGE_INTEGER start;
    start.QuadPart = 0;
    if (SetFilePointerEx(file.get(), start, nullptr, FILE_BEGIN) == 0) {
        return std::nullopt;
    }
    std::vector<uint8_t> data;
    data.reserve(static_cast<size_t>(size));
    uint8_t buffer[65536];
    while (true) {
        DWORD chunk = 0;
        if (ReadFile(file.get(), buffer, sizeof(buffer), &chunk, nullptr) == 0) {
            return std::nullopt;
        }
        if (chunk == 0) {
            break;
        }
        data.insert(data.end(), buffer, buffer + chunk);
    }
    if (encrypted) {
        for (uint8_t& byte : data) {
            byte ^= 0x05;
        }
    }
    const std::optional<const char*> mime = ImageMime(data);
    if (!mime.has_value()) {
        return std::nullopt;
    }
    return std::make_pair(std::string(*mime), std::move(data));
}

bool LooksBinary(const std::vector<uint8_t>& data) {
    if (data.empty()) {
        return false;
    }
    const size_t sample = std::min<size_t>(data.size(), 4096);
    size_t odd = 0;
    for (size_t index = 0; index < sample; ++index) {
        const uint8_t byte = data[index];
        if (byte < 0x09 || (byte > 0x0D && byte < 0x20)) {
            ++odd;
        }
    }
    return odd * 100 / sample > 10;
}

std::string HexPreview(const std::vector<uint8_t>& data) {
    std::string out;
    const size_t limit = std::min<size_t>(data.size(), 16 * 32);
    for (size_t offset = 0; offset < limit; offset += 16) {
        const size_t end = std::min<size_t>(offset + 16, limit);
        std::string hex;
        std::string ascii;
        for (size_t index = offset; index < end; ++index) {
            char buffer[4];
            std::snprintf(buffer, sizeof(buffer), "%02x", data[index]);
            if (!hex.empty()) {
                hex += " ";
            }
            hex += buffer;
            const uint8_t byte = data[index];
            ascii += (byte >= 32 && byte < 127) ? static_cast<char>(byte) : '.';
        }
        while (hex.size() < 47) {
            hex += ' ';
        }
        if (!out.empty()) {
            out += "\n";
        }
        out += hex + "  " + ascii;
    }
    return out;
}

}  // namespace

namespace im {

std::optional<std::pair<std::string, std::string>> Classify(
    const std::string& path, const std::string& root) {
    return ClassifyPath(path, root);
}

std::vector<std::string> MigrationRoots(const std::string& kind) {
    std::vector<std::string> candidates = DefaultRootCandidates(kind);
    if (kind == "wechat") {
        for (const std::optional<std::string>& value :
             {WechatRegistryPathValue(), WechatIniPathValue()}) {
            if (value.has_value()) {
                AppendWechatMigrationCandidates(candidates, *value);
            }
        }
    }
    std::sort(candidates.begin(), candidates.end());
    candidates.erase(std::unique(candidates.begin(), candidates.end()),
                     candidates.end());
    return candidates;
}

std::vector<std::string> Roots(const std::string& kind) {
    std::vector<std::string> candidates = DefaultRootCandidates(kind);
    if (kind == "wechat") {
        if (const std::optional<std::string> custom =
                ResolveWechatRoot(WechatRegistryPathValue())) {
            candidates.push_back(*custom);
        }
        if (const std::optional<std::string> custom =
                ResolveWechatRoot(WechatIniPathValue())) {
            candidates.push_back(*custom);
        }
    }
    std::sort(candidates.begin(), candidates.end());
    candidates.erase(std::unique(candidates.begin(), candidates.end()),
                     candidates.end());
    std::vector<std::string> existing;
    for (const std::string& path : candidates) {
        if (IsDirectory(path)) {
            existing.push_back(path);
        }
    }
    return existing;
}

bool IsProgramRoot(const std::string& root) {
    const std::string trimmed = LowerAscii(TrimEndSlashes(root));
    if (trimmed.empty()) {
        return false;
    }
    for (const char* key : {"APPDATA", "LOCALAPPDATA"}) {
        const std::string base = LowerAscii(TrimEndSlashes(GetEnv(key)));
        if (base.empty()) {
            continue;
        }
        if (trimmed == base ||
            trimmed.rfind(base + "\\", 0) == 0) {
            return true;
        }
    }
    return false;
}

std::vector<Record> ListMedia(const std::vector<std::string>& roots) {
    std::vector<Record> found;
    for (const std::string& root : roots) {
        if (!IsDirectory(root)) {
            continue;
        }
        WalkFiles(root, [&found, &root](const std::string& path,
                                        const WIN32_FIND_DATAW& data) {
            const uint64_t mtime = FileTimeToUnix(data.ftLastWriteTime);
            const std::optional<std::pair<std::string, std::string>> classified =
                ClassifyPath(path, root);
            if (!classified.has_value()) {
                return;
            }
            Record record;
            record.path = path;
            record.size = (static_cast<uint64_t>(data.nFileSizeHigh) << 32) |
                          static_cast<uint64_t>(data.nFileSizeLow);
            record.mtime = mtime;
            record.kind = classified->first;
            record.sub = classified->second;
            record.month = MonthLabel(mtime);
            record.name = BasenameOf(path);
            found.push_back(std::move(record));
        });
    }
    std::stable_sort(found.begin(), found.end(),
                     [](const Record& left, const Record& right) {
                         return left.mtime > right.mtime;
                     });
    return found;
}

Json RecordsToJson(const std::vector<Record>& records) {
    Json out = Json::array();
    for (const Record& record : records) {
        out.push_back(Json{{"path", record.path},
                           {"size", record.size},
                           {"mtime", record.mtime},
                           {"kind", record.kind},
                           {"sub", record.sub},
                           {"month", record.month},
                           {"name", record.name}});
    }
    return out;
}

std::optional<std::pair<std::string, std::vector<uint8_t>>> Thumbnail(
    const std::string& path) {
    if (std::optional<std::pair<std::string, std::vector<uint8_t>>> found =
            ReadImage(path)) {
        return found;
    }
    // 朋友圈的视频没有封面扩展名，同名的 `_t` 文件就是它的封面图。
    return ReadImage(path + "_t");
}

Json Preview(const std::string& path) {
    WIN32_FILE_ATTRIBUTE_DATA info = {};
    if (GetFileAttributesExW(Utf8ToWide(path).c_str(), GetFileExInfoStandard,
                             &info) == 0) {
        throw std::runtime_error("读取失败：" +
                                 FormatWin32Error(GetLastError()));
    }
    if ((info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
        throw std::runtime_error("不是文件");
    }
    const uint64_t size = (static_cast<uint64_t>(info.nFileSizeHigh) << 32) |
                          static_cast<uint64_t>(info.nFileSizeLow);
    const HANDLE raw = CreateFileW(
        Utf8ToWide(path).c_str(), GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr,
        OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (raw == INVALID_HANDLE_VALUE) {
        throw std::runtime_error("打开失败：" + FormatWin32Error(GetLastError()));
    }
    UniqueHandle file(raw);
    std::vector<uint8_t> data;
    uint8_t buffer[8192];
    while (data.size() < kPreviewLimit) {
        DWORD read = 0;
        if (ReadFile(file.get(), buffer, sizeof(buffer), &read, nullptr) == 0) {
            throw std::runtime_error("读取失败：" +
                                     FormatWin32Error(GetLastError()));
        }
        if (read == 0) {
            break;
        }
        data.insert(data.end(), buffer, buffer + read);
    }
    if (data.size() > kPreviewLimit) {
        data.resize(static_cast<size_t>(kPreviewLimit));
    }
    const bool binary = LooksBinary(data);
    std::string text;
    if (binary) {
        text = HexPreview(data);
    } else {
        text = Utf8Lossy(std::string(data.begin(), data.end()));
    }
    return Json{{"text", text},
                {"binary", binary},
                {"truncated", size > kPreviewLimit},
                {"size", size}};
}

Json Clean(const std::string& kind, const std::vector<std::string>& kinds,
           const std::optional<std::vector<std::string>>& paths) {
    const std::vector<std::string> roots = Roots(kind);
    std::vector<Record> chosen;
    auto wanted = [&kinds](const std::string& value) {
        return std::find(kinds.begin(), kinds.end(), value) != kinds.end();
    };
    if (!paths.has_value()) {
        for (const Record& record : ListMedia(roots)) {
            if (wanted(record.kind)) {
                chosen.push_back(record);
            }
        }
    } else {
        for (const std::string& path : *paths) {
            if (path.find_first_not_of(" \t\r\n") == std::string::npos ||
                !safety::IsWithin(path, roots)) {
                continue;
            }
            if (safety::IsPasswordStore(path) || safety::IsProtectedPath(path)) {
                continue;
            }
            std::string root;
            for (const std::string& candidate : roots) {
                if (safety::IsWithin(path, {candidate}) &&
                    candidate.size() > root.size()) {
                    root = candidate;
                }
            }
            const std::optional<std::pair<std::string, std::string>> classified =
                ClassifyPath(path, root);
            if (!classified.has_value() || !wanted(classified->first)) {
                continue;
            }
            WIN32_FILE_ATTRIBUTE_DATA info = {};
            if (GetFileAttributesExW(Utf8ToWide(path).c_str(),
                                     GetFileExInfoStandard, &info) == 0) {
                continue;
            }
            Record record;
            record.name = BasenameOf(path);
            record.path = path;
            record.size = (static_cast<uint64_t>(info.nFileSizeHigh) << 32) |
                          static_cast<uint64_t>(info.nFileSizeLow);
            record.mtime = FileTimeToUnix(info.ftLastWriteTime);
            record.kind = classified->first;
            record.sub = classified->second;
            record.month = MonthLabel(record.mtime);
            chosen.push_back(std::move(record));
        }
    }

    Json report{{"freed", 0},
                {"deleted", 0},
                {"failed", Json::array()},
                {"reasons", Json::array()}};
    if (chosen.empty()) {
        return report;
    }
    task::SetProgress("im_clean", 0, chosen.size());
    if (task::IsCancelled("im_clean")) {
        report["reasons"] = Json::array({"已取消"});
        return report;
    }

    std::vector<std::string> batch;
    batch.reserve(chosen.size());
    for (const Record& record : chosen) {
        batch.push_back(record.path);
    }
    recycle::SendToRecycle(batch);
    task::SetProgress("im_clean", chosen.size(), chosen.size());

    uint64_t freed = 0;
    uint64_t deleted = 0;
    std::vector<std::string> failed;
    std::vector<std::string> reasons;
    for (const Record& record : chosen) {
        if (PathExists(record.path)) {
            failed.push_back(record.path);
            reasons.push_back(record.path + "：送回收站失败（可能被占用）");
        } else {
            freed += record.size;
            ++deleted;
        }
    }
    dups::InvalidateCache();
    logs::WriteLog({"开始清理",
                    std::string("清理：") + (kind == "wechat" ? "微信" : "QQ"),
                    "完成：移到回收站 " + std::to_string(deleted) +
                        " 个，释放 " + std::to_string(freed) + " 字节"});
    report["freed"] = freed;
    report["deleted"] = deleted;
    report["failed"] = failed;
    report["reasons"] = reasons;
    return report;
}

void RememberScanned(const std::vector<Record>& records) {
    std::lock_guard<std::mutex> lock(g_scannedMutex);
    for (const Record& record : records) {
        g_scanned.insert(record.path);
    }
}

bool IsScanned(const std::string& path) {
    std::lock_guard<std::mutex> lock(g_scannedMutex);
    return g_scanned.find(path) != g_scanned.end();
}

}  // namespace im
