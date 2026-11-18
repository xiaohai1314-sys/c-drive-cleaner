#include "core/safety.h"

#include <winternl.h>

#include <algorithm>
#include <cctype>
#include <cstddef>
#include <cstring>
#include <filesystem>
#include <optional>
#include <string>
#include <unordered_map>
#include <vector>

#include "util/env.h"
#include "util/glob.h"
#include "util/json.h"
#include "util/path.h"
#include "util/resource_ids.h"
#include "util/utf.h"

extern "C" {
NTSTATUS NTAPI NtCreateFile(PHANDLE fileHandle, ACCESS_MASK desiredAccess,
                            POBJECT_ATTRIBUTES objectAttributes,
                            PIO_STATUS_BLOCK ioStatusBlock,
                            PLARGE_INTEGER allocationSize,
                            ULONG fileAttributes, ULONG shareAccess,
                            ULONG createDisposition, ULONG createOptions,
                            PVOID eaBuffer, ULONG eaLength);NTSTATUS NTAPI NtSetInformationFile(HANDLE fileHandle,
                                    PIO_STATUS_BLOCK ioStatusBlock,
                                    PVOID fileInformation, ULONG length,
                                    ULONG fileInformationClass);
ULONG NTAPI RtlNtStatusToDosError(NTSTATUS status);
}

namespace {

constexpr DWORD kFileShareAll =
    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
constexpr DWORD kBaseDirectoryAccess =
    FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE;
constexpr ULONG kFileRenameInformation = 10;

const char* const kPasswordFilenames[] = {
    "login data",
    "login data-journal",
    "login data for account",
    "login data for account-journal",
    "logins.json",
    "logins-backup.json",
    "key4.db",
    "key3.db",
};

const char* const kCriticalFiles[] = {
    "pagefile.sys", "hiberfil.sys", "swapfile.sys", "bootmgr",
    "ntldr",        "ntuser.dat",   "ntuser.dat.log",
};

const char* const kLibraryNames[] = {
    "documents", "desktop", "downloads", "my documents", "文档",
    "桌面",      "下载",     "pictures",  "music",        "videos",
};

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

std::string TrimSpaceAndQuotes(std::string_view text) {
    std::string out = TrimSpace(text);
    while (!out.empty() && out.front() == '"') {
        out.erase(out.begin());
    }
    while (!out.empty() && out.back() == '"') {
        out.pop_back();
    }
    return out;
}

std::string TrimTrailingSeparators(std::string text) {
    while (!text.empty() && (text.back() == '\\' || text.back() == '/')) {
        text.pop_back();
    }
    return text;
}

bool StartsWith(const std::string& text, const std::string& prefix) {
    return text.size() >= prefix.size() &&
           text.compare(0, prefix.size(), prefix) == 0;
}

bool IsUnderTail(const std::string& tail, std::string_view marker) {
    std::string normalized(marker);
    if (normalized.empty() || normalized.front() != '\\') {
        normalized.insert(normalized.begin(), '\\');
    }
    normalized = ToLower(normalized);
    if (tail == normalized) {
        return true;
    }
    return tail.size() > normalized.size() &&
           StartsWith(tail, normalized) && tail[normalized.size()] == '\\';
}

std::string TailOf(std::string_view path) {
    const std::string norm = safety::Normalize(path);
    std::string rest =
        (norm.size() >= 2 && norm[1] == ':') ? norm.substr(2) : norm;
    for (char& ch : rest) {
        if (ch == '/') {
            ch = '\\';
        }
        ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    }
    if (rest.empty() || rest.front() != '\\') {
        rest.insert(rest.begin(), '\\');
    }
    return rest;
}

std::vector<std::string> SplitParts(std::string_view relative) {
    std::vector<std::string> parts;
    size_t start = 0;
    while (start <= relative.size()) {
        const size_t slash = relative.find_first_of("\\/", start);
        const std::string part =
            slash == std::string_view::npos
                ? std::string(relative.substr(start))
                : std::string(relative.substr(start, slash - start));
        if (!part.empty()) {
            parts.push_back(part);
        }
        if (slash == std::string_view::npos) {
            break;
        }
        start = slash + 1;
    }
    return parts;
}

bool ContainsWildcard(std::string_view text) {
    return text.find_first_of("*?[") != std::string_view::npos;
}

bool ContainsString(const char* const* list, size_t count,
                    const std::string& value) {
    for (size_t index = 0; index < count; ++index) {
        if (value == list[index]) {
            return true;
        }
    }
    return false;
}

std::vector<std::string> EnvRoots() {
    std::vector<std::string> roots;
    for (const char* name :
         {"SystemRoot", "WINDIR", "ProgramFiles", "ProgramFiles(x86)",
          "ProgramData"}) {
        const std::string value = GetEnv(name);
        if (!value.empty()) {
            roots.push_back(ToLower(safety::Normalize(value)));
        }
    }
    return roots;
}

// ---------- AppData 白名单 catalog ----------

using PatternList = std::vector<std::vector<GlobPattern>>;
using CatalogMap = std::unordered_map<std::string, PatternList>;
using Catalog = std::unordered_map<std::string, CatalogMap>;

Catalog BuildCatalog() {
    Catalog catalog;
    catalog.emplace("localappdata", CatalogMap{});
    catalog.emplace("appdata", CatalogMap{});

    const std::string raw = LoadResourceText(IDR_ALLOWLIST_JSON);
    const Json data = Json::parse(raw, nullptr, false);
    if (data.is_discarded() || !data.is_object()) {
        return catalog;
    }
    const auto paths = data.find("paths");
    if (paths == data.end() || !paths->is_array()) {
        return catalog;
    }
    for (const Json& item : *paths) {
        if (!item.is_string()) {
            continue;
        }
        std::string folded = item.get<std::string>();
        for (char& ch : folded) {
            if (ch == '/') {
                ch = '\\';
            }
        }
        const std::string lower = ToLower(folded);
        for (const char* key : {"localappdata", "appdata"}) {
            const std::string marker = std::string("%") + key + "%\\";
            if (!StartsWith(lower, marker)) {
                continue;
            }
            const std::vector<std::string> parts =
                SplitParts(std::string_view(folded).substr(marker.size()));
            if (parts.empty() || ContainsWildcard(parts[0])) {
                continue;
            }
            PatternList patterns;
            std::vector<GlobPattern> row;
            for (size_t index = 1; index < parts.size(); ++index) {
                row.emplace_back(ToLower(parts[index]));
            }
            patterns.push_back(std::move(row));
            break;
        }
    }
    return catalog;
}

const Catalog& GetCatalog() {
    static const Catalog catalog = BuildCatalog();
    return catalog;
}

bool CatalogMatches(const std::vector<std::string>& parts,
                    const std::string& key) {
    if (parts.empty()) {
        return false;
    }
    const auto mapIt = GetCatalog().find(key);
    if (mapIt == GetCatalog().end()) {
        return false;
    }
    const auto listIt = mapIt->second.find(ToLower(parts[0]));
    if (listIt == mapIt->second.end()) {
        return false;
    }
    for (const auto& patterns : listIt->second) {
        if (parts.size() < patterns.size() + 1) {
            continue;
        }
        bool matched = true;
        for (size_t index = 0; index < patterns.size(); ++index) {
            if (!patterns[index].matches(ToLower(parts[index + 1]))) {
                matched = false;
                break;
            }
        }
        if (matched) {
            return true;
        }
    }
    return false;
}

bool KnownAppdataTarget(std::string_view path) {
    std::string raw = TrimSpace(path);
    for (char& ch : raw) {
        if (ch == '/') {
            ch = '\\';
        }
    }
    const std::string lower = ToLower(raw);
    for (const char* key : {"localappdata", "appdata"}) {
        const std::string marker = std::string("%") + key + "%\\";
        if (StartsWith(lower, marker)) {
            return CatalogMatches(
                SplitParts(std::string_view(raw).substr(marker.size())), key);
        }
    }
    struct EnvTarget {
        const char* key;
        const char* env;
    };
    static const EnvTarget targets[] = {
        {"localappdata", "LOCALAPPDATA"},
        {"appdata", "APPDATA"},
    };
    for (const EnvTarget& target : targets) {
        const std::string rootValue = GetEnv(target.env);
        if (rootValue.empty()) {
            continue;
        }
        const std::string root = ToLower(safety::Normalize(rootValue));
        const std::string normalizedTarget = ToLower(safety::Normalize(raw));
        if (normalizedTarget != root &&
            !(StartsWith(normalizedTarget, root) &&
              normalizedTarget.size() > root.size() &&
              normalizedTarget[root.size()] == '\\')) {
            continue;
        }
        std::string relative = normalizedTarget.substr(root.size());
        while (!relative.empty() &&
               (relative.front() == '\\' || relative.front() == '/')) {
            relative.erase(relative.begin());
        }
        if (CatalogMatches(SplitParts(relative), target.key)) {
            return true;
        }
    }
    return false;
}

// ---------- 句柄相对原语 ----------

struct Win32Parts {
    std::string root;
    std::vector<std::string> components;
};

bool ParseWin32Parts(const std::string& path, Win32Parts& parts) {
    std::string raw = path;
    for (char& ch : raw) {
        if (ch == '/') {
            ch = '\\';
        }
    }
    if (raw.size() < 3 ||
        std::isalpha(static_cast<unsigned char>(raw[0])) == 0 ||
        raw[1] != ':' || raw[2] != '\\' || safety::PathSyntaxError(raw)) {
        return false;
    }
    const std::string normalized = safety::Normalize(raw);
    if (normalized.size() < 3) {
        return false;
    }
    parts.root = normalized.substr(0, 3);
    parts.components.clear();
    const std::string rest = normalized.substr(3);
    size_t start = 0;
    while (start <= rest.size()) {
        const size_t slash = rest.find('\\', start);
        const std::string part =
            slash == std::string::npos ? rest.substr(start)
                                       : rest.substr(start, slash - start);
        if (!part.empty()) {
            parts.components.push_back(part);
        }
        if (slash == std::string::npos) {
            break;
        }
        start = slash + 1;
    }
    return true;
}

std::string ParentPath(const std::string& path) {
    const std::string normalized = safety::Normalize(path);
    if (normalized.size() <= 3) {
        return {};
    }
    const size_t slash = normalized.find_last_of('\\');
    if (slash == std::string::npos || slash < 2) {
        return {};
    }
    if (slash == 2) {
        return normalized.substr(0, 3);
    }
    return normalized.substr(0, slash);
}

bool GetHandleAttributes(HANDLE handle, DWORD& attributes) {
    BY_HANDLE_FILE_INFORMATION info = {};
    if (GetFileInformationByHandle(handle, &info) == 0) {
        return false;
    }
    attributes = info.dwFileAttributes;
    return true;
}

bool OpenDriveRootWithAccess(const std::string& root, DWORD access,
                             HANDLE& out, DWORD& error) {
    const std::wstring wide = Utf8ToWide(root);
    HANDLE handle = CreateFileW(
        wide.c_str(), access, kFileShareAll, nullptr, OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
    if (handle == INVALID_HANDLE_VALUE) {
        error = GetLastError();
        return false;
    }
    DWORD attributes = 0;
    if (!GetHandleAttributes(handle, attributes) ||
        (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 ||
        (attributes & FILE_ATTRIBUTE_DIRECTORY) == 0) {
        CloseHandle(handle);
        error = ERROR_ACCESS_DENIED;
        return false;
    }
    out = handle;
    return true;
}

bool OpenRelative(HANDLE parent, const std::string& name, DWORD access,
                  DWORD options, DWORD disposition, DWORD share,
                  HANDLE& out, DWORD& error) {
    if (name.empty() || name == "." || name == ".." ||
        name.find_first_of("\\/:") != std::string::npos ||
        name.find('\0') != std::string::npos) {
        error = ERROR_INVALID_NAME;
        return false;
    }
    std::wstring wide = Utf8ToWide(name);
    if (wide.empty() || wide.size() * 2 > 0xFFFE) {
        error = ERROR_INVALID_NAME;
        return false;
    }
    UNICODE_STRING objectName = {};
    objectName.Length = static_cast<USHORT>(wide.size() * 2);
    objectName.MaximumLength = static_cast<USHORT>(objectName.Length + 2);
    objectName.Buffer = wide.data();

    OBJECT_ATTRIBUTES attributes = {};
    attributes.Length = sizeof(attributes);
    attributes.RootDirectory = parent;
    attributes.ObjectName = &objectName;
    attributes.Attributes = OBJ_CASE_INSENSITIVE;

    IO_STATUS_BLOCK statusBlock = {};
    HANDLE handle = nullptr;
    const NTSTATUS status = NtCreateFile(
        &handle, access, &attributes, &statusBlock, nullptr, 0, share,
        disposition,
        options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        nullptr, 0);
    if (status < 0) {
        error = RtlNtStatusToDosError(status);
        return false;
    }
    out = handle;
    return true;
}

bool OpenDirectoryChainWithFinalAccess(const std::string& path,
                                       DWORD finalAccess,
                                       safety::HandleChain& chain,
                                       DWORD& error) {
    chain.handles.clear();
    Win32Parts parts;
    if (!ParseWin32Parts(path, parts)) {
        error = ERROR_INVALID_NAME;
        return false;
    }
    const DWORD rootAccess =
        kBaseDirectoryAccess |
        (parts.components.empty() ? finalAccess : 0);
    HANDLE rootHandle = INVALID_HANDLE_VALUE;
    if (!OpenDriveRootWithAccess(parts.root, rootAccess, rootHandle, error)) {
        return false;
    }
    chain.handles.emplace_back(rootHandle);
    for (size_t index = 0; index < parts.components.size(); ++index) {
        const bool last = index + 1 == parts.components.size();
        const DWORD access =
            kBaseDirectoryAccess | (last ? finalAccess : 0);
        HANDLE child = INVALID_HANDLE_VALUE;
        if (!OpenRelative(chain.handles.back().get(), parts.components[index],
                          access, FILE_DIRECTORY_FILE, FILE_OPEN,
                          FILE_SHARE_READ | FILE_SHARE_WRITE, child, error)) {
            chain.handles.clear();
            return false;
        }
        DWORD attributes = 0;
        if (!GetHandleAttributes(child, attributes) ||
            (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 ||
            (attributes & FILE_ATTRIBUTE_DIRECTORY) == 0) {
            CloseHandle(child);
            chain.handles.clear();
            error = ERROR_ACCESS_DENIED;
            return false;
        }
        chain.handles.emplace_back(child);
    }
    return true;
}

bool OpenDirectoryChain(const std::string& path, safety::HandleChain& chain,
                        DWORD& error) {
    return OpenDirectoryChainWithFinalAccess(path, 0, chain, error);
}

bool OpenParentChainWithRights(const std::string& path,
                               const std::vector<std::string>& roots,
                               DWORD parentRights,
                               safety::HandleChain& chain, std::string& name,
                               DWORD& error) {
    if (!safety::IsWithin(path, roots)) {
        error = ERROR_ACCESS_DENIED;
        return false;
    }
    name = safety::Basename(path);
    if (name.empty()) {
        error = ERROR_INVALID_NAME;
        return false;
    }
    const std::string parent = ParentPath(path);
    if (parent.empty()) {
        error = ERROR_INVALID_NAME;
        return false;
    }
    return OpenDirectoryChainWithFinalAccess(parent, parentRights, chain,
                                             error);
}

bool OpenParentChain(const std::string& path,
                     const std::vector<std::string>& roots,
                     safety::HandleChain& chain, std::string& name,
                     DWORD& error) {
    return OpenParentChainWithRights(path, roots, 0, chain, name, error);
}

bool OpenPlainFile(const std::string& path,
                   const std::vector<std::string>& roots,
                   safety::HandleChain& chain, UniqueHandle& file,
                   DWORD& error) {
    std::string name;
    if (!OpenParentChain(path, roots, chain, name, error)) {
        return false;
    }
    HANDLE handle = INVALID_HANDLE_VALUE;
    if (!OpenRelative(chain.handles.back().get(), name,
                      DELETE | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES |
                          SYNCHRONIZE,
                      FILE_NON_DIRECTORY_FILE, FILE_OPEN,
                      FILE_SHARE_READ | FILE_SHARE_WRITE, handle, error)) {
        return false;
    }
    DWORD attributes = 0;
    if (!GetHandleAttributes(handle, attributes) ||
        (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 ||
        (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
        CloseHandle(handle);
        error = ERROR_ACCESS_DENIED;
        return false;
    }
    file.reset(handle);
    return true;
}

bool ClearReadonlyAndDelete(HANDLE handle) {
    FILE_BASIC_INFO info = {};
    if (GetFileInformationByHandleEx(handle, FileBasicInfo, &info,
                                     sizeof(info)) == 0 ||
        (info.FileAttributes & FILE_ATTRIBUTE_READONLY) == 0) {
        return false;
    }
    info.FileAttributes &= ~static_cast<DWORD>(FILE_ATTRIBUTE_READONLY);
    if (SetFileInformationByHandle(handle, FileBasicInfo, &info,
                                   sizeof(info)) == 0) {
        return false;
    }
    return safety::MarkDelete(handle);
}

std::optional<bool> DeletePlainFile(const std::string& path,
                                    const std::vector<std::string>& roots,
                                    DWORD& error) {
    safety::HandleChain chain;
    UniqueHandle file;
    if (!OpenPlainFile(path, roots, chain, file, error)) {
        return std::nullopt;
    }
    if (safety::MarkDelete(file.get())) {
        return true;
    }
    if (ClearReadonlyAndDelete(file.get())) {
        return true;
    }
    return false;
}

bool RemoveContents(const std::string& dir,
                    const std::vector<std::string>& roots, uint64_t& removed,
                    DWORD& error) {
    safety::HandleChain chain;
    if (!OpenDirectoryChain(dir, chain, error)) {
        return false;
    }
    DWORD attributes = 0;
    if (!GetHandleAttributes(chain.handles.back().get(), attributes) ||
        (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 ||
        (attributes & FILE_ATTRIBUTE_DIRECTORY) == 0) {
        error = ERROR_ACCESS_DENIED;
        return false;
    }

    std::error_code iterateError;
    std::filesystem::directory_iterator iterator(
        std::filesystem::path(Utf8ToWide(dir)), iterateError);
    if (iterateError) {
        error = static_cast<DWORD>(iterateError.value());
        return false;
    }
    for (const auto& entry : iterator) {
        std::error_code statusError;
        const std::filesystem::file_status status =
            entry.symlink_status(statusError);
        if (statusError) {
            continue;
        }
        const std::string child = WideToUtf8(entry.path().wstring());
        const DWORD childAttributes = GetFileAttributesW(
            entry.path().c_str());
        if (childAttributes != INVALID_FILE_ATTRIBUTES &&
            (childAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0) {
            continue;
        }
        if (std::filesystem::is_directory(status)) {
            if (!RemoveContents(child, roots, removed, error)) {
                return false;
            }
            bool removedChild = false;
            if (!safety::RemoveDirectoryChecked(child, roots, removedChild,
                                                error)) {
                return false;
            }
            if (removedChild) {
                ++removed;
            }
        } else if (std::filesystem::is_regular_file(status)) {
            DWORD deleteError = 0;
            const std::optional<bool> outcome =
                DeletePlainFile(child, roots, deleteError);
            if (!outcome.has_value()) {
                error = deleteError;
                return false;
            }
            if (*outcome) {
                ++removed;
            }
        }
    }
    return true;
}

}  // namespace

namespace safety {

std::string Basename(std::string_view path) {
    std::string text(path);
    for (char& ch : text) {
        if (ch == '/') {
            ch = '\\';
        }
    }
    while (!text.empty() && text.back() == '\\') {
        text.pop_back();
    }
    const size_t slash = text.find_last_of('\\');
    if (slash == std::string::npos) {
        return text;
    }
    return text.substr(slash + 1);
}

std::string Normalize(std::string_view path) {
    std::string text = TrimSpaceAndQuotes(path);
    for (char& ch : text) {
        if (ch == '/') {
            ch = '\\';
        }
    }
    if (text.size() >= 2 && text[1] == ':') {
        std::string drive = text.substr(0, 2);
        drive[0] =
            static_cast<char>(std::toupper(static_cast<unsigned char>(drive[0])));
        std::string rest;
        for (char ch : text.substr(2)) {
            if (ch == '\\' && !rest.empty() && rest.back() == '\\') {
                continue;
            }
            rest.push_back(ch);
        }
        if (rest.empty() || rest == "\\") {
            return drive + "\\";
        }
        if (rest.front() != '\\') {
            rest.insert(rest.begin(), '\\');
        }
        std::string out = drive + rest;
        while (!out.empty() && out.back() == '\\') {
            out.pop_back();
        }
        return out;
    }
    while (!text.empty() && (text.back() == '\\' || text.back() == '/')) {
        text.pop_back();
    }
    return text;
}

bool UnderMarker(std::string_view path,
                 const std::vector<std::string>& markers) {
    const std::string tail = TailOf(path);
    for (const std::string& marker : markers) {
        if (IsUnderTail(tail, marker)) {
            return true;
        }
    }
    return false;
}

bool IsDriveRoot(std::string_view path) {
    const std::string norm = Normalize(path);
    return (norm.size() == 2 && norm[1] == ':') ||
           (norm.size() == 3 && norm[1] == ':' &&
            (norm[2] == '\\' || norm[2] == '/'));
}

bool IsCriticalFile(std::string_view path) {
    const std::string name = ToLower(Basename(path));
    return ContainsString(kCriticalFiles,
                          sizeof(kCriticalFiles) / sizeof(kCriticalFiles[0]),
                          name);
}

bool IsNeverClean(std::string_view path) {
    if (TrimSpace(path).empty()) {
        return true;
    }
    if (ToLower(Basename(path)) == "usmt.ppkg") {
        return true;
    }
    return UnderMarker(path, {"\\recovery", "\\system volume information"});
}

bool IsPasswordStore(std::string_view path) {
    const std::string name = ToLower(Basename(path));
    return ContainsString(kPasswordFilenames,
                          sizeof(kPasswordFilenames) /
                              sizeof(kPasswordFilenames[0]),
                          name);
}

bool PathSyntaxError(std::string_view path) {
    const std::string text = TrimSpace(path);
    if (text.empty()) {
        return true;
    }
    if (StartsWith(text, "\\\\") || StartsWith(text, "//")) {
        return true;
    }
    const std::string body =
        (text.size() >= 2 && text[1] == ':') ? text.substr(2) : text;
    if (body.find(':') != std::string::npos) {
        return true;
    }
    const std::vector<std::string> parts = SplitParts(text);
    return std::any_of(parts.begin(), parts.end(),
                       [](const std::string& part) { return part == ".."; });
}

bool IsSystemPath(std::string_view path) {
    if (TrimSpace(path).empty() || IsDriveRoot(path) || IsCriticalFile(path)) {
        return true;
    }
    if (UnderMarker(path, {"\\windows", "\\system volume information",
                           "\\recovery", "\\boot", "\\$recycle.bin"})) {
        return true;
    }
    const std::string target = TrimTrailingSeparators(ToLower(Normalize(path)));
    for (const char* key : {"SystemRoot", "WINDIR"}) {
        const std::string value = GetEnv(key);
        if (value.empty()) {
            continue;
        }
        const std::string root =
            TrimTrailingSeparators(ToLower(Normalize(value)));
        if (!root.empty() &&
            (target == root ||
             (StartsWith(target, root) && target.size() > root.size() &&
              target[root.size()] == '\\'))) {
            return true;
        }
    }
    return false;
}

bool IsProtectedPath(std::string_view path) {
    if (IsSystemPath(path)) {
        return true;
    }
    if (UnderMarker(path, {"\\program files", "\\program files (x86)",
                           "\\programdata", "\\perflogs"})) {
        return true;
    }
    const std::string target = TrimTrailingSeparators(ToLower(Normalize(path)));
    for (const std::string& rootValue : EnvRoots()) {
        const std::string root = TrimTrailingSeparators(rootValue);
        if (!root.empty() &&
            (target == root ||
             (StartsWith(target, root) && target.size() > root.size() &&
              target[root.size()] == '\\'))) {
            return true;
        }
    }
    const std::string profile = GetEnv("USERPROFILE");
    if (!profile.empty() &&
        target == TrimTrailingSeparators(ToLower(Normalize(profile)))) {
        return true;
    }
    return false;
}

std::vector<std::string> UserLibraryRoots() {
    std::vector<std::string> bases;
    for (const char* key :
         {"USERPROFILE", "HOME", "OneDrive", "OneDriveConsumer"}) {
        const std::string value = GetEnv(key);
        if (!value.empty()) {
            bases.push_back(value);
        }
    }
    std::vector<std::string> roots;
    for (const std::string& base : bases) {
        for (const char* name : kLibraryNames) {
            roots.push_back(ToLower(Normalize(JoinPath(base, name))));
        }
    }
    return roots;
}

bool IsUserLibrary(std::string_view path) {
    if (TrimSpace(path).empty()) {
        return false;
    }
    const std::string target = TrimTrailingSeparators(ToLower(Normalize(path)));
    for (const std::string& rootValue : UserLibraryRoots()) {
        const std::string root = TrimTrailingSeparators(rootValue);
        if (target == root ||
            (StartsWith(target, root) && target.size() > root.size() &&
             target[root.size()] == '\\')) {
            return true;
        }
    }
    const std::string tail = TailOf(path);
    std::string trimmed = tail;
    while (!trimmed.empty() && trimmed.front() == '\\') {
        trimmed.erase(trimmed.begin());
    }
    const std::vector<std::string> parts = SplitParts(trimmed);
    if (parts.size() >= 3 && parts[0] == "users" &&
        ContainsString(kLibraryNames,
                       sizeof(kLibraryNames) / sizeof(kLibraryNames[0]),
                       parts[2])) {
        return true;
    }
    if (parts.size() >= 2 && parts[0] == "users" &&
        ContainsString(kLibraryNames,
                       sizeof(kLibraryNames) / sizeof(kLibraryNames[0]),
                       parts[1])) {
        return true;
    }
    return false;
}

bool IsWithin(std::string_view path, const std::vector<std::string>& roots) {
    if (roots.empty()) {
        return false;
    }
    const std::string target = TrimTrailingSeparators(ToLower(Normalize(path)));
    for (const std::string& rootValue : roots) {
        const std::string base = TrimTrailingSeparators(ToLower(Normalize(rootValue)));
        if (base.empty()) {
            continue;
        }
        if (target == base ||
            (StartsWith(target, base) && target.size() > base.size() &&
             target[base.size()] == '\\')) {
            return true;
        }
    }
    return false;
}

bool IsReparsePoint(std::string_view path) {
    const std::optional<bool> status = ReparsePointStatus(path);
    return status.value_or(false);
}

std::optional<bool> ReparsePointStatus(std::string_view path) {
    const std::wstring wide = Utf8ToWide(path);
    if (wide.empty()) {
        return std::nullopt;
    }
    const DWORD attributes = GetFileAttributesW(wide.c_str());
    if (attributes == INVALID_FILE_ATTRIBUTES) {
        return std::nullopt;
    }
    return (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0;
}

bool UnderRemoteAllowlist(std::string_view path) {
    if (PathSyntaxError(path)) {
        return false;
    }
    const std::string normalized = Normalize(path);
    if (IsSystemPath(normalized) || IsUserLibrary(normalized)) {
        return false;
    }
    std::vector<std::string> roots;
    for (const char* key : {"TEMP", "TMP"}) {
        const std::string value = GetEnv(key);
        if (!value.empty() && !PathSyntaxError(value)) {
            roots.push_back(value);
        }
    }
    const std::string local = GetEnv("LOCALAPPDATA");
    if (!local.empty()) {
        roots.push_back(JoinPath(TrimTrailingSeparators(local), "Temp"));
    }
    if (IsWithin(normalized, roots)) {
        return true;
    }
    return KnownAppdataTarget(normalized);
}

bool ExecutionForbidden(std::string_view path, bool untrusted) {
    if (PathSyntaxError(path)) {
        return true;
    }
    if (IsNeverClean(path)) {
        return true;
    }
    if (IsReparsePoint(path)) {
        return true;
    }
    if (IsPasswordStore(path) || IsUserLibrary(path)) {
        return true;
    }
    if (IsDriveRoot(path) || IsCriticalFile(path)) {
        return true;
    }
    if (untrusted && (IsSystemPath(path) || !UnderRemoteAllowlist(path))) {
        return true;
    }
    return false;
}

bool ExplicitDeleteForbidden(std::string_view path) {
    if (PathSyntaxError(path)) {
        return true;
    }
    if (IsNeverClean(path)) {
        return true;
    }
    if (IsReparsePoint(path)) {
        return true;
    }
    if (IsPasswordStore(path) || IsDriveRoot(path) || IsCriticalFile(path)) {
        return true;
    }
    return IsProtectedPath(path);
}

DeleteOutcome DeleteFileChecked(const std::string& path,
                                const std::vector<std::string>& roots,
                                bool untrusted) {
    if (ExecutionForbidden(path, untrusted)) {
        return {DeleteOutcomeKind::Skipped, {}};
    }
    DWORD error = 0;
    const std::optional<bool> outcome = DeletePlainFile(path, roots, error);
    if (outcome.has_value()) {
        if (*outcome) {
            return {DeleteOutcomeKind::Deleted, {}};
        }
        return {DeleteOutcomeKind::Failed, "无法删除：" + path};
    }
    if (error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND ||
        error == ERROR_ACCESS_DENIED || error == ERROR_INVALID_NAME) {
        return {DeleteOutcomeKind::Skipped, {}};
    }
    return {DeleteOutcomeKind::Failed,
            path + "：" + FormatWin32Error(error)};
}

bool MarkDelete(HANDLE handle) {
    FILE_DISPOSITION_INFO info = {};
    info.DeleteFile = TRUE;
    return SetFileInformationByHandle(handle, FileDispositionInfo, &info,
                                      sizeof(info)) != 0;
}

size_t RemoveEmptyDirs(const std::string& root) {
    std::vector<std::string> dirs;
    std::vector<std::string> stack = {Normalize(root)};
    while (!stack.empty()) {
        const std::string current = stack.back();
        stack.pop_back();
        std::error_code iterateError;
        std::filesystem::directory_iterator iterator(
            std::filesystem::path(Utf8ToWide(current)), iterateError);
        if (iterateError) {
            continue;
        }
        for (const auto& entry : iterator) {
            std::error_code statusError;
            const std::filesystem::file_status status =
                entry.symlink_status(statusError);
            if (statusError || !std::filesystem::is_directory(status)) {
                continue;
            }
            const DWORD attributes =
                GetFileAttributesW(entry.path().c_str());
            if (attributes != INVALID_FILE_ATTRIBUTES &&
                (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0) {
                continue;
            }
            const std::string path = WideToUtf8(entry.path().wstring());
            dirs.push_back(path);
            stack.push_back(path);
        }
    }
    std::sort(dirs.begin(), dirs.end(),
              [](const std::string& left, const std::string& right) {
                  return left.size() > right.size();
              });
    size_t removed = 0;
    for (const std::string& path : dirs) {
        if (Normalize(path) == Normalize(root) || IsNeverClean(path)) {
            continue;
        }
        bool removedDirectory = false;
        DWORD error = 0;
        if (RemoveDirectoryChecked(path, {root}, removedDirectory, error) &&
            removedDirectory) {
            ++removed;
        }
    }
    return removed;
}

bool PinDirectoryChecked(const std::string& path,
                         const std::vector<std::string>& roots,
                         HandleChain& chain, DWORD& error) {
    if (PathSyntaxError(path) || !IsWithin(path, roots)) {
        error = ERROR_ACCESS_DENIED;
        return false;
    }
    return OpenDirectoryChain(path, chain, error);
}

bool CreateDirectoryTreeChecked(const std::string& path, HandleChain& chain,
                                DWORD& error) {
    chain.handles.clear();
    if (PathSyntaxError(path)) {
        error = ERROR_INVALID_NAME;
        return false;
    }
    Win32Parts parts;
    if (!ParseWin32Parts(path, parts)) {
        error = ERROR_INVALID_NAME;
        return false;
    }
    if (parts.components.empty()) {
        error = ERROR_ALREADY_EXISTS;
        return false;
    }
    HANDLE rootHandle = INVALID_HANDLE_VALUE;
    if (!OpenDriveRootWithAccess(parts.root, kBaseDirectoryAccess, rootHandle,
                                 error)) {
        return false;
    }
    chain.handles.emplace_back(rootHandle);
    std::string parentPath = parts.root;
    for (size_t index = 0; index < parts.components.size(); ++index) {
        const bool finalComponent = index + 1 == parts.components.size();
        HANDLE child = INVALID_HANDLE_VALUE;
        if (finalComponent) {
            HandleChain createParent;
            if (!OpenDirectoryChainWithFinalAccess(
                    parentPath, FILE_ADD_SUBDIRECTORY, createParent, error)) {
                chain.handles.clear();
                return false;
            }
            if (!OpenRelative(createParent.handles.back().get(),
                              parts.components[index], kBaseDirectoryAccess,
                              FILE_DIRECTORY_FILE, FILE_CREATE,
                              FILE_SHARE_READ | FILE_SHARE_WRITE, child,
                              error)) {
                chain.handles.clear();
                return false;
            }
        } else {
            if (OpenRelative(chain.handles.back().get(),
                             parts.components[index], kBaseDirectoryAccess,
                             FILE_DIRECTORY_FILE, FILE_OPEN,
                             FILE_SHARE_READ | FILE_SHARE_WRITE, child,
                             error)) {
                // existing component
            } else if (error == ERROR_FILE_NOT_FOUND ||
                       error == ERROR_PATH_NOT_FOUND) {
                HandleChain createParent;
                if (!OpenDirectoryChainWithFinalAccess(
                        parentPath, FILE_ADD_SUBDIRECTORY, createParent,
                        error)) {
                    chain.handles.clear();
                    return false;
                }
                if (!OpenRelative(createParent.handles.back().get(),
                                  parts.components[index],
                                  kBaseDirectoryAccess, FILE_DIRECTORY_FILE,
                                  FILE_CREATE,
                                  FILE_SHARE_READ | FILE_SHARE_WRITE, child,
                                  error)) {
                    chain.handles.clear();
                    return false;
                }
            } else {
                chain.handles.clear();
                return false;
            }
        }
        DWORD attributes = 0;
        if (!GetHandleAttributes(child, attributes) ||
            (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 ||
            (attributes & FILE_ATTRIBUTE_DIRECTORY) == 0) {
            CloseHandle(child);
            chain.handles.clear();
            error = ERROR_ACCESS_DENIED;
            return false;
        }
        chain.handles.emplace_back(child);
        parentPath = JoinPath(parentPath, parts.components[index]);
    }
    return true;
}

bool CreateDirectoryChecked(const std::string& path,
                            const std::vector<std::string>& roots,
                            HandleChain& chain, UniqueHandle& directory,
                            DWORD& error) {
    std::string name;
    if (!OpenParentChainWithRights(path, roots, FILE_ADD_SUBDIRECTORY, chain,
                                   name, error)) {
        return false;
    }
    HANDLE handle = INVALID_HANDLE_VALUE;
    if (!OpenRelative(chain.handles.back().get(), name, kBaseDirectoryAccess,
                      FILE_DIRECTORY_FILE, FILE_CREATE,
                      FILE_SHARE_READ | FILE_SHARE_WRITE, handle, error)) {
        return false;
    }
    DWORD attributes = 0;
    if (!GetHandleAttributes(handle, attributes) ||
        (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 ||
        (attributes & FILE_ATTRIBUTE_DIRECTORY) == 0) {
        CloseHandle(handle);
        error = ERROR_ACCESS_DENIED;
        return false;
    }
    directory.reset(handle);
    return true;
}

bool CreateNewFileChecked(const std::string& path,
                          const std::vector<std::string>& roots,
                          HandleChain& chain, UniqueHandle& file,
                          DWORD& error) {
    std::string name;
    if (!OpenParentChainWithRights(path, roots, FILE_ADD_FILE, chain, name,
                                   error)) {
        return false;
    }
    HANDLE handle = INVALID_HANDLE_VALUE;
    if (!OpenRelative(chain.handles.back().get(), name,
                      FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                      FILE_NON_DIRECTORY_FILE, FILE_CREATE, FILE_SHARE_READ,
                      handle, error)) {
        return false;
    }
    DWORD attributes = 0;
    if (!GetHandleAttributes(handle, attributes) ||
        (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 ||
        (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
        CloseHandle(handle);
        error = ERROR_ACCESS_DENIED;
        return false;
    }
    file.reset(handle);
    return true;
}

bool OpenReadFileChecked(const std::string& path,
                         const std::vector<std::string>& roots,
                         HandleChain& chain, UniqueHandle& file,
                         DWORD& error) {
    std::string name;
    if (!OpenParentChain(path, roots, chain, name, error)) {
        return false;
    }
    HANDLE handle = INVALID_HANDLE_VALUE;
    if (!OpenRelative(chain.handles.back().get(), name,
                      FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                      FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_SHARE_READ,
                      handle, error)) {
        return false;
    }
    DWORD attributes = 0;
    if (!GetHandleAttributes(handle, attributes) ||
        (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 ||
        (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
        CloseHandle(handle);
        error = ERROR_ACCESS_DENIED;
        return false;
    }
    file.reset(handle);
    return true;
}

bool RemoveDirectoryChecked(const std::string& path,
                            const std::vector<std::string>& roots,
                            bool& removed, DWORD& error) {
    removed = false;
    HandleChain chain;
    std::string name;
    if (!OpenParentChain(path, roots, chain, name, error)) {
        return false;
    }
    HANDLE handle = INVALID_HANDLE_VALUE;
    if (!OpenRelative(chain.handles.back().get(), name,
                      DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                      FILE_DIRECTORY_FILE, FILE_OPEN,
                      FILE_SHARE_READ | FILE_SHARE_WRITE, handle, error)) {
        return false;
    }
    UniqueHandle directory(handle);
    DWORD attributes = 0;
    if (!GetHandleAttributes(handle, attributes) ||
        (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 ||
        (attributes & FILE_ATTRIBUTE_DIRECTORY) == 0) {
        return true;
    }
    removed = MarkDelete(handle);
    return true;
}

bool RemoveTreeChecked(const std::string& root,
                       const std::vector<std::string>& allowedRoots,
                       uint64_t& removed, DWORD& error) {
    removed = 0;
    if (!IsWithin(root, allowedRoots)) {
        error = ERROR_ACCESS_DENIED;
        return false;
    }
    if (!RemoveContents(root, allowedRoots, removed, error)) {
        return false;
    }
    bool removedRoot = false;
    if (!RemoveDirectoryChecked(root, allowedRoots, removedRoot, error)) {
        return false;
    }
    if (!removedRoot) {
        error = ERROR_DIR_NOT_EMPTY;
        return false;
    }
    ++removed;
    return true;
}

bool RenameDirectorySiblingChecked(
    const std::string& from, const std::string& to,
    const std::vector<std::string>& allowedRoots, DWORD& error) {
    if (PathSyntaxError(from) || PathSyntaxError(to) ||
        (!IsWithin(from, allowedRoots) && !IsWithin(to, allowedRoots)) ||
        ParentPath(from) != ParentPath(to)) {
        error = ERROR_ACCESS_DENIED;
        return false;
    }
    const std::string fromName = Basename(from);
    const std::string toName = Basename(to);
    if (fromName.empty() || toName.empty()) {
        error = ERROR_INVALID_NAME;
        return false;
    }
    const std::string parent = ParentPath(from);
    if (parent.empty()) {
        error = ERROR_INVALID_NAME;
        return false;
    }
    HandleChain chain;
    if (!OpenDirectoryChainWithFinalAccess(parent, FILE_ADD_SUBDIRECTORY,
                                           chain, error)) {
        return false;
    }
    HANDLE parentHandle = chain.handles.back().get();
    HANDLE sourceHandle = INVALID_HANDLE_VALUE;
    if (!OpenRelative(parentHandle, fromName,
                      DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                      FILE_DIRECTORY_FILE, FILE_OPEN,
                      FILE_SHARE_READ | FILE_SHARE_WRITE, sourceHandle,
                      error)) {
        return false;
    }
    UniqueHandle source(sourceHandle);
    DWORD attributes = 0;
    if (!GetHandleAttributes(sourceHandle, attributes) ||
        (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 ||
        (attributes & FILE_ATTRIBUTE_DIRECTORY) == 0) {
        error = ERROR_ACCESS_DENIED;
        return false;
    }

    HANDLE existing = INVALID_HANDLE_VALUE;
    DWORD targetError = 0;
    if (OpenRelative(parentHandle, toName,
                     FILE_READ_ATTRIBUTES | SYNCHRONIZE, 0, FILE_OPEN,
                     kFileShareAll, existing, targetError)) {
        CloseHandle(existing);
        error = ERROR_ALREADY_EXISTS;
        return false;
    }
    if (targetError != ERROR_FILE_NOT_FOUND &&
        targetError != ERROR_PATH_NOT_FOUND) {
        error = targetError;
        return false;
    }

    const std::wstring wide = Utf8ToWide(toName);
    const size_t nameBytes = wide.size() * sizeof(wchar_t);
    const size_t nameOffset = offsetof(FILE_RENAME_INFO, FileName);
    std::vector<BYTE> buffer(nameOffset + nameBytes);
    auto* info = reinterpret_cast<FILE_RENAME_INFO*>(buffer.data());
    info->ReplaceIfExists = FALSE;
    info->RootDirectory = parentHandle;
    info->FileNameLength = static_cast<DWORD>(nameBytes);
    std::memcpy(buffer.data() + nameOffset, wide.data(), nameBytes);

    IO_STATUS_BLOCK statusBlock = {};
    const NTSTATUS status = NtSetInformationFile(
        sourceHandle, &statusBlock, buffer.data(),
        static_cast<ULONG>(buffer.size()), kFileRenameInformation);
    if (status < 0) {
        error = RtlNtStatusToDosError(status);
        return false;
    }
    return true;
}

}  // namespace safety
