#include "app/ipc.h"

#include "app/dialog.h"
#include "app/messages.h"
#include "app/webview_host.h"
#include "core/commands/registry.h"
#include "util/utf.h"

namespace {

Json MakeReply(const Json& id, bool ok, Json data, std::string error) {
    Json reply;
    reply["id"] = id;
    reply["ok"] = ok;
    if (ok) {
        reply["data"] = std::move(data);
    } else {
        reply["error"] = std::move(error);
    }
    return reply;
}

}  // namespace

Ipc::Ipc(HWND hwnd, WebViewHost* host) : hwnd_(hwnd), host_(host) {}

void Ipc::OnWebMessage(const std::wstring& message) {
    Json request;
    try {
        request = Json::parse(WideToUtf8(message));
    } catch (...) {
        return;
    }
    if (!request.is_object()) {
        return;
    }
    const auto cmdIt = request.find("cmd");
    if (cmdIt == request.end() || !cmdIt->is_string()) {
        return;
    }
    const std::string cmd = cmdIt->get<std::string>();
    if (cmd.rfind("__", 0) == 0) {
        HandleSpecial(request);
        return;
    }

    const Json id = request.contains("id") ? request["id"] : Json(nullptr);
    Json args = request.contains("args") ? request["args"] : Json::object();
    if (!args.is_object()) {
        args = Json::object();
    }
    // 前端使用 camelCase 的两个参数，Rust 侧 serde 映射为 snake_case。
    if (cmd == "scan_dups" && args.contains("minSize")) {
        args["min_size"] = args["minSize"];
    }
    if (cmd == "im_move_execute" && args.contains("targetDir")) {
        args["target_dir"] = args["targetDir"];
    }

    pool_.Submit([this, id, cmd, args]() {
        try {
            Json data = commands::Dispatch(cmd, args);
            PostReply(MakeReply(id, true, std::move(data), {}));
        } catch (const std::exception& ex) {
            PostReply(MakeReply(id, false, nullptr, ex.what()));
        } catch (...) {
            PostReply(MakeReply(id, false, nullptr, "命令执行失败"));
        }
    });
}

void Ipc::HandleSpecial(const Json& request) {
    const std::string cmd = request.value("cmd", "");
    if (cmd == "__win_minimize") {
        ShowWindow(hwnd_, SW_MINIMIZE);
        return;
    }
    if (cmd == "__win_toggle_maximize") {
        if (IsZoomed(hwnd_) != 0) {
            ShowWindow(hwnd_, SW_RESTORE);
        } else {
            ShowWindow(hwnd_, SW_MAXIMIZE);
        }
        return;
    }
    if (cmd == "__win_close") {
        PostMessageW(hwnd_, WM_CLOSE, 0, 0);
        return;
    }
    if (cmd == "__win_drag") {
        ReleaseCapture();
        SendMessageW(hwnd_, WM_NCLBUTTONDOWN, HTCAPTION, 0);
        return;
    }
    if (cmd == "__dialog_open") {
        const Json id = request.contains("id") ? request["id"] : Json(nullptr);
        const Json options =
            request.contains("args") ? request["args"] : Json::object();
        try {
            Json picked = PickPathsDialog(hwnd_, options);
            PostReply(MakeReply(id, true, std::move(picked), {}));
        } catch (const std::exception& ex) {
            PostReply(MakeReply(id, false, nullptr, ex.what()));
        }
        return;
    }
}

void Ipc::PostReply(Json reply) {
    {
        std::lock_guard<std::mutex> lock(replyMutex_);
        replies_.push_back(std::move(reply));
    }
    PostMessageW(hwnd_, WM_APP_REPLY, 0, 0);
}

void Ipc::DrainReplies() {
    std::vector<Json> pending;
    {
        std::lock_guard<std::mutex> lock(replyMutex_);
        pending.swap(replies_);
    }
    if (host_ == nullptr) {
        return;
    }
    for (const Json& reply : pending) {
        host_->PostJson(reply.dump(-1, ' ', false, Json::error_handler_t::replace));
    }
}
