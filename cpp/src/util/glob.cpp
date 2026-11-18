#include "util/glob.h"

#include <windows.h>

#include <algorithm>
#include <cctype>
#include <string>
#include <unordered_set>
#include <vector>

#include "util/path.h"
#include "util/utf.h"

namespace {

char Lower(char ch) {
    return static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
}

std::string ToLower(std::string_view text) {
    std::string out(text);
    for (char& ch : out) {
        ch = Lower(ch);
    }
    return out;
}

bool MatchClass(const char*& pattern, char ch) {
    ++pattern;  // '['
    bool negate = false;
    if (*pattern == '!' || *pattern == '^') {
        negate = true;
        ++pattern;
    }
    bool matched = false;
    bool first = true;
    while (*pattern != '\0' && (*pattern != ']' || first)) {
        first = false;
        char start = *pattern++;
        if (start == '\\' && *pattern != '\0') {
            start = *pattern++;
        }
        if (*pattern == '-' && pattern[1] != '\0' && pattern[1] != ']') {
            ++pattern;
            char end = *pattern++;
            if (end == '\\' && *pattern != '\0') {
                end = *pattern++;
            }
            if (ch >= start && ch <= end) {
                matched = true;
            }
        } else if (ch == start) {
            matched = true;
        }
    }
    if (*pattern == ']') {
        ++pattern;
    }
    return negate ? !matched : matched;
}

bool MatchHere(const char* pattern, const char* text) {
    while (*pattern != '\0') {
        if (*pattern == '*') {
            while (pattern[1] == '*') {
                ++pattern;
            }
            ++pattern;
            if (*pattern == '\0') {
                return true;
            }
            for (const char* probe = text;; ++probe) {
                if (MatchHere(pattern, probe)) {
                    return true;
                }
                if (*probe == '\0') {
                    return false;
                }
            }
        }
        if (*pattern == '?') {
            if (*text == '\0') {
                return false;
            }
            ++pattern;
            ++text;
            continue;
        }
        if (*pattern == '[') {
            if (*text == '\0') {
                return false;
            }
            const char* saved = pattern;
            if (!MatchClass(pattern, *text)) {
                pattern = saved;
                return false;
            }
            ++text;
            continue;
        }
        if (*pattern == '\\' && pattern[1] != '\0') {
            ++pattern;
        }
        if (*pattern != *text) {
            return false;
        }
        ++pattern;
        ++text;
    }
    return *text == '\0';
}

bool HasWildcard(const std::string& text) {
    return text.find_first_of("*?[") != std::string::npos;
}

void ExpandComponent(const std::string& current,
                     const std::vector<std::string>& components, size_t index,
                     std::vector<std::string>& results) {
    if (index >= components.size()) {
        if (PathExists(current)) {
            results.push_back(current);
        }
        return;
    }
    const std::string& component = components[index];
    if (!HasWildcard(component)) {
        ExpandComponent(JoinPath(current, component), components, index + 1,
                        results);
        return;
    }

    const std::wstring pattern = Utf8ToWide(JoinPath(current, "*"));
    WIN32_FIND_DATAW data;
    HANDLE handle = FindFirstFileW(pattern.c_str(), &data);
    if (handle == INVALID_HANDLE_VALUE) {
        return;
    }
    do {
        if (wcscmp(data.cFileName, L".") == 0 ||
            wcscmp(data.cFileName, L"..") == 0) {
            continue;
        }
        const std::string name = WideToUtf8(data.cFileName);
        if (GlobMatch(component, name, true)) {
            ExpandComponent(JoinPath(current, name), components, index + 1,
                            results);
        }
    } while (FindNextFileW(handle, &data) != 0);
    FindClose(handle);
}

}  // namespace

GlobPattern::GlobPattern(std::string pattern) : pattern_(std::move(pattern)) {}

bool GlobPattern::matches(std::string_view text) const {
    return GlobMatch(pattern_, text);
}

bool GlobMatch(std::string_view pattern, std::string_view text,
               bool caseInsensitive) {
    const std::string patternStorage =
        caseInsensitive ? ToLower(pattern) : std::string(pattern);
    const std::string textStorage =
        caseInsensitive ? ToLower(text) : std::string(text);
    return MatchHere(patternStorage.c_str(), textStorage.c_str());
}

std::vector<std::string> GlobFilesystem(const std::string& pattern) {
    std::string normalized = pattern;
    for (char& ch : normalized) {
        if (ch == '/') {
            ch = '\\';
        }
    }

    std::string root;
    std::string rest;
    if (normalized.size() >= 3 && std::isalpha(static_cast<unsigned char>(
                                      normalized[0])) &&
        normalized[1] == ':' && normalized[2] == '\\') {
        root = normalized.substr(0, 3);
        rest = normalized.substr(3);
    } else {
        wchar_t buffer[MAX_PATH];
        const DWORD length = GetCurrentDirectoryW(MAX_PATH, buffer);
        if (length == 0) {
            return {};
        }
        root = WideToUtf8(std::wstring(buffer, length));
        rest = normalized;
    }

    std::vector<std::string> components;
    size_t start = 0;
    while (start <= rest.size()) {
        const size_t slash = rest.find('\\', start);
        const std::string part =
            slash == std::string::npos ? rest.substr(start)
                                       : rest.substr(start, slash - start);
        if (!part.empty()) {
            components.push_back(part);
        }
        if (slash == std::string::npos) {
            break;
        }
        start = slash + 1;
    }

    std::vector<std::string> results;
    ExpandComponent(root, components, 0, results);

    std::vector<std::string> unique;
    std::unordered_set<std::string> seen;
    for (const std::string& item : results) {
        const std::string key = ToLower(NormalizePath(item));
        if (seen.insert(key).second) {
            unique.push_back(item);
        }
    }
    return unique;
}

std::string NormalizeExcludePattern(const std::string& pattern) {
    if (pattern.size() >= 2 && pattern[0] == '.') {
        const std::string rest = pattern.substr(1);
        const bool asciiAlnum = std::all_of(
            rest.begin(), rest.end(), [](char ch) {
                return std::isalnum(static_cast<unsigned char>(ch)) != 0;
            });
        if (asciiAlnum) {
            return "*" + pattern;
        }
    }
    return pattern;
}
