#pragma once

#include <windows.h>

#include "util/json.h"

// 在 STA UI 线程调用；取消返回 null。
Json PickPathsDialog(HWND owner, const Json& options);
