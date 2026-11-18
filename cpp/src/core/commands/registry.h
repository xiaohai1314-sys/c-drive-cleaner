#pragma once

#include <string>

#include "util/json.h"

namespace commands {

void RegisterAll();
// 未知命令或执行失败时抛出 std::runtime_error。
Json Dispatch(const std::string& name, const Json& args);

}  // namespace commands
