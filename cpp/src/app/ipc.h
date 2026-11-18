#pragma once

#include <windows.h>

#include <mutex>
#include <string>
#include <vector>

#include "core/threadpool.h"
#include "util/json.h"

class WebViewHost;

class Ipc {
public:
    Ipc(HWND hwnd, WebViewHost* host);

    void OnWebMessage(const std::wstring& message);
    void DrainReplies();

private:
    void HandleSpecial(const Json& request);
    void PostReply(Json reply);

    HWND hwnd_ = nullptr;
    WebViewHost* host_ = nullptr;
    ThreadPool pool_{8};
    std::mutex replyMutex_;
    std::vector<Json> replies_;
};
