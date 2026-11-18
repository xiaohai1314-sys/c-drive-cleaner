#pragma once

#include <optional>
#include <string>
#include <vector>

#include "util/json.h"

namespace logs {

std::string LogDir();
bool ValidLogName(const std::string& name);
std::optional<std::string> WriteLog(const std::vector<std::string>& lines);
void AppendLine(const std::string& name, const std::string& line);
Json List();
std::optional<std::string> Read(const std::string& name);

}  // namespace logs
