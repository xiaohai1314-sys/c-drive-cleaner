#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "util/glob.h"
#include "util/json.h"

struct Rule {
    std::string key;
    std::string name;
    std::string tier;
    std::string kind;
    std::string describe;
    std::string warning;
    std::string note;
    std::vector<std::string> paths;
    std::vector<std::string> glob_dirs;
    std::vector<std::string> patterns;
    std::vector<std::string> exclude;
    uint64_t min_age_minutes = 0;
    bool requires_confirm = false;
    bool prune_dirs = false;
};

std::vector<Rule> AllRules();
std::vector<std::string> ExpandPaths(const std::vector<std::string>& raw);
std::vector<GlobPattern> BuildMatcher(const std::vector<std::string>& patterns);
bool IncludeMatches(const std::string& lowerName,
                    const std::vector<GlobPattern>& include,
                    const std::vector<GlobPattern>& exclude);
bool AgeOk(uint64_t mtimeUnix, uint64_t minAgeSeconds, uint64_t nowUnix);

std::string RemoteRulesPath();
std::string RulesUpdate();
std::string RulesRollback();
