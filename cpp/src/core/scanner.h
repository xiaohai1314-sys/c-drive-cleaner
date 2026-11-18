#pragma once

#include <string>
#include <vector>

#include "util/json.h"

Json ScanCategories();
Json CleanCategories(const std::vector<std::string>& keys);
