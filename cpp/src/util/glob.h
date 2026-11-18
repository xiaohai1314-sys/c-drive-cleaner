#pragma once

#include <string>
#include <string_view>
#include <vector>

class GlobPattern {
public:
    explicit GlobPattern(std::string pattern);
    bool matches(std::string_view text) const;
    const std::string& text() const { return pattern_; }

private:
    std::string pattern_;
};

bool GlobMatch(std::string_view pattern, std::string_view text,
               bool caseInsensitive = false);
std::vector<std::string> GlobFilesystem(const std::string& pattern);
std::string NormalizeExcludePattern(const std::string& pattern);
