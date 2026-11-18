#include "app/webview_host.h"

#include <wrl.h>
#include <shlwapi.h>

#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "app/ipc.h"
#include "core/commands/registry.h"
#include "core/im.h"
#include "util/env.h"
#include "util/path.h"
#include "util/utf.h"
#include "util/win32.h"
#include "shim_js.h"

namespace {

constexpr wchar_t kAppHost[] = L"app.cdrivecleaner.invalid";
constexpr wchar_t kThumbHost[] = L"imthumb.cdrivecleaner.invalid";
constexpr wchar_t kAppUrl[] = L"https://app.cdrivecleaner.invalid/index.html";

std::string PercentDecode(const std::string& input) {
    auto hex = [](char ch) -> int {
        if (ch >= '0' && ch <= '9') {
            return ch - '0';
        }
        if (ch >= 'a' && ch <= 'f') {
            return ch - 'a' + 10;
        }
        if (ch >= 'A' && ch <= 'F') {
            return ch - 'A' + 10;
        }
        return -1;
    };
    std::string out;
    out.reserve(input.size());
    for (size_t index = 0; index < input.size();) {
        if (input[index] == '%' && index + 2 < input.size()) {
            const int high = hex(input[index + 1]);
            const int low = hex(input[index + 2]);
            if (high >= 0 && low >= 0) {
                out.push_back(static_cast<char>(high * 16 + low));
                index += 3;
                continue;
            }
        }
        out.push_back(input[index]);
        ++index;
    }
    return out;
}

/// 从 `...?p=<percent-encoded path>` 取出路径。
std::string ThumbnailPathFromUri(const std::wstring& uri) {
    const std::string text = WideToUtf8(uri);
    const size_t query = text.find("?p=");
    if (query == std::string::npos) {
        return {};
    }
    return PercentDecode(text.substr(query + 3));
}

std::wstring UserDataDir() {
    std::string base = LocalAppDataDir();
    if (base.empty()) {
        base = ExeDirUtf8();
    }
    if (base.empty()) {
        base = ".";
    }
    const std::wstring dir = Utf8ToWide(JoinPath(base, "webview2-cpp"));
    CreateDirectoryW(dir.c_str(), nullptr);
    return dir;
}

}  // namespace

WebViewHost::WebViewHost() = default;

WebViewHost::~WebViewHost() { Close(); }

bool WebViewHost::Initialize(HWND hwnd, const std::wstring& webDir,
                             std::function<void()> onReady) {
    hwnd_ = hwnd;
    webDir_ = webDir;
    onReady_ = std::move(onReady);
    ipc_ = std::make_unique<Ipc>(hwnd, this);

    if (!PathExists(WideToUtf8(webDir + L"\\index.html"))) {
        MessageBoxW(hwnd,
                    L"找不到 web/dist。请先在仓库根目录运行:\n\n"
                    L"npm --prefix web run build",
                    L"磁盘清理助手", MB_ICONERROR);
        return false;
    }

    const std::wstring userData = UserDataDir();
    environmentHandler_ = Microsoft::WRL::Callback<
        ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler>(
        [this](HRESULT result, ICoreWebView2Environment* environment) -> HRESULT {
            if (FAILED(result) || environment == nullptr) {
                MessageBoxW(hwnd_, (L"WebView2 初始化失败: " +
                                    Utf8ToWide(HResultMessage(result)))
                                       .c_str(),
                            L"磁盘清理助手", MB_ICONERROR);
                return result;
            }
            environment_ = environment;
            controllerHandler_ = Microsoft::WRL::Callback<
                ICoreWebView2CreateCoreWebView2ControllerCompletedHandler>(
                [this](HRESULT controllerResult,
                       ICoreWebView2Controller* controller) -> HRESULT {
                    OnControllerReady(controllerResult, controller);
                    return S_OK;
                });
            environment_->CreateCoreWebView2Controller(
                hwnd_, controllerHandler_.Get());
            return S_OK;
        });

    const HRESULT hr = CreateCoreWebView2EnvironmentWithOptions(
        nullptr, userData.c_str(), nullptr, environmentHandler_.Get());
    if (FAILED(hr)) {
        MessageBoxW(hwnd_,
                    (L"创建 WebView2 环境失败: " + Utf8ToWide(HResultMessage(hr)))
                        .c_str(),
                    L"磁盘清理助手", MB_ICONERROR);
        return false;
    }
    return true;
}

void WebViewHost::OnControllerReady(HRESULT result,
                                    ICoreWebView2Controller* controller) {
    if (FAILED(result) || controller == nullptr) {
        MessageBoxW(hwnd_,
                    (L"创建 WebView2 控制器失败: " + Utf8ToWide(HResultMessage(result)))
                        .c_str(),
                    L"磁盘清理助手", MB_ICONERROR);
        return;
    }
    controller_ = controller;
    if (FAILED(controller_->get_CoreWebView2(&webview_)) || !webview_) {
        MessageBoxW(hwnd_, L"获取 WebView2 实例失败", L"磁盘清理助手",
                    MB_ICONERROR);
        return;
    }
    ConfigureWebView();
    Resize();
    controller_->put_IsVisible(TRUE);

    webview_->AddScriptToExecuteOnDocumentCreated(
        Utf8ToWide(kShimJs).c_str(),
        Microsoft::WRL::Callback<
            ICoreWebView2AddScriptToExecuteOnDocumentCreatedCompletedHandler>(
            [](HRESULT, LPCWSTR) -> HRESULT { return S_OK; })
            .Get());

    if (onReady_) {
        onReady_();
    }
    Navigate();
}

void WebViewHost::ConfigureWebView() {
    Microsoft::WRL::ComPtr<ICoreWebView2Settings> settings;
    if (SUCCEEDED(webview_->get_Settings(&settings)) && settings) {
        settings->put_IsStatusBarEnabled(FALSE);
        settings->put_AreDefaultContextMenusEnabled(FALSE);
        settings->put_IsZoomControlEnabled(FALSE);
        settings->put_IsBuiltInErrorPageEnabled(FALSE);
        settings->put_AreDefaultScriptDialogsEnabled(FALSE);
#ifdef _DEBUG
        settings->put_AreDevToolsEnabled(TRUE);
#else
        settings->put_AreDevToolsEnabled(FALSE);
#endif
    }

    Microsoft::WRL::ComPtr<ICoreWebView2_3> webview3;
    if (SUCCEEDED(webview_->QueryInterface(IID_PPV_ARGS(&webview3))) &&
        webview3) {
        webview3->SetVirtualHostNameToFolderMapping(
            kAppHost, webDir_.c_str(),
            COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY_CORS);
    }

    webview_->AddWebResourceRequestedFilter(
        L"https://imthumb.cdrivecleaner.invalid/*",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE);
    webview_->add_WebResourceRequested(
        Microsoft::WRL::Callback<ICoreWebView2WebResourceRequestedEventHandler>(
            [this](ICoreWebView2*,
                   ICoreWebView2WebResourceRequestedEventArgs* args) -> HRESULT {
                if (environment_ == nullptr || args == nullptr) {
                    return S_OK;
                }
                std::wstring uri;
                Microsoft::WRL::ComPtr<ICoreWebView2WebResourceRequest> request;
                if (SUCCEEDED(args->get_Request(&request)) && request != nullptr) {
                    LPWSTR raw = nullptr;
                    if (SUCCEEDED(request->get_Uri(&raw)) && raw != nullptr) {
                        uri = raw;
                        CoTaskMemFree(raw);
                    }
                }
                const std::string path = ThumbnailPathFromUri(uri);
                std::optional<std::pair<std::string, std::vector<uint8_t>>> image;
                if (!path.empty() && im::IsScanned(path)) {
                    image = im::Thumbnail(path);
                }
                Microsoft::WRL::ComPtr<ICoreWebView2WebResourceResponse> response;
                if (image.has_value()) {
                    Microsoft::WRL::ComPtr<IStream> stream;
                    stream.Attach(SHCreateMemStream(
                        image->second.data(),
                        static_cast<UINT>(image->second.size())));
                    const std::wstring headers =
                        L"Content-Type: " + Utf8ToWide(image->first) +
                        L"\r\nCache-Control: max-age=3600";
                    if (stream != nullptr &&
                        SUCCEEDED(environment_->CreateWebResourceResponse(
                            stream.Get(), 200, L"OK", headers.c_str(),
                            &response))) {
                        args->put_Response(response.Get());
                        return S_OK;
                    }
                }
                if (SUCCEEDED(environment_->CreateWebResourceResponse(
                        nullptr, 404, L"Not Found",
                        L"Content-Type: text/plain\r\nCache-Control: no-store",
                        &response))) {
                    args->put_Response(response.Get());
                }
                return S_OK;
            })
            .Get(),
        &resourceToken_);

    webview_->add_WebMessageReceived(
        Microsoft::WRL::Callback<ICoreWebView2WebMessageReceivedEventHandler>(
            [this](ICoreWebView2*,
                   ICoreWebView2WebMessageReceivedEventArgs* args) -> HRESULT {
                LPWSTR message = nullptr;
                if (SUCCEEDED(args->get_WebMessageAsJson(&message)) &&
                    message != nullptr) {
                    if (ipc_) {
                        ipc_->OnWebMessage(message);
                    }
                    CoTaskMemFree(message);
                }
                return S_OK;
            })
            .Get(),
        &messageToken_);

    webview_->add_NavigationStarting(
        Microsoft::WRL::Callback<ICoreWebView2NavigationStartingEventHandler>(
            [](ICoreWebView2*,
               ICoreWebView2NavigationStartingEventArgs* args) -> HRESULT {
                LPWSTR uri = nullptr;
                std::wstring target;
                if (SUCCEEDED(args->get_Uri(&uri)) && uri != nullptr) {
                    target.assign(uri);
                    CoTaskMemFree(uri);
                }
                if (target.rfind(L"https://app.cdrivecleaner.invalid/", 0) != 0) {
                    args->put_Cancel(TRUE);
                }
                return S_OK;
            })
            .Get(),
        &navigationToken_);

    webview_->add_NewWindowRequested(
        Microsoft::WRL::Callback<ICoreWebView2NewWindowRequestedEventHandler>(
            [](ICoreWebView2*,
               ICoreWebView2NewWindowRequestedEventArgs* args) -> HRESULT {
                args->put_Handled(TRUE);
                return S_OK;
            })
            .Get(),
        &newWindowToken_);

    webview_->add_ProcessFailed(
        Microsoft::WRL::Callback<ICoreWebView2ProcessFailedEventHandler>(
            [](ICoreWebView2*,
               ICoreWebView2ProcessFailedEventArgs* args) -> HRESULT {
                COREWEBVIEW2_PROCESS_FAILED_KIND kind =
                    COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED;
                args->get_ProcessFailedKind(&kind);
                OutputDebugStringW(
                    (L"WebView2 process failed, kind=" + std::to_wstring(kind) +
                     L"\n")
                        .c_str());
                return S_OK;
            })
            .Get(),
        &processFailedToken_);

    handlersRegistered_ = true;
}

void WebViewHost::Navigate() {
    if (webview_) {
        webview_->Navigate(kAppUrl);
    }
}

void WebViewHost::Resize() {
    if (!controller_) {
        return;
    }
    RECT bounds;
    GetClientRect(hwnd_, &bounds);
    controller_->put_Bounds(bounds);
}

void WebViewHost::DrainReplies() {
    if (ipc_) {
        ipc_->DrainReplies();
    }
}

void WebViewHost::PostJson(const std::string& json) {
    if (!webview_) {
        return;
    }
    const std::wstring wide = Utf8ToWide(json);
    webview_->PostWebMessageAsJson(wide.c_str());
}

void WebViewHost::Close() {
    if (webview_ && handlersRegistered_) {
        webview_->remove_WebMessageReceived(messageToken_);
        webview_->remove_WebResourceRequested(resourceToken_);
        webview_->remove_NavigationStarting(navigationToken_);
        webview_->remove_NewWindowRequested(newWindowToken_);
        webview_->remove_ProcessFailed(processFailedToken_);
        handlersRegistered_ = false;
    }
    if (controller_) {
        controller_->put_IsVisible(FALSE);
        controller_->Close();
        controller_.Reset();
    }
    webview_.Reset();
    environment_.Reset();
    controllerHandler_.Reset();
    environmentHandler_.Reset();
    ipc_.reset();
}
