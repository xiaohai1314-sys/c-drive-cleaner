#pragma once

#include <windows.h>
#include <wrl/client.h>
#include <WebView2.h>

#include <functional>
#include <memory>
#include <string>

class Ipc;

class WebViewHost {
public:
    WebViewHost();
    ~WebViewHost();

    WebViewHost(const WebViewHost&) = delete;
    WebViewHost& operator=(const WebViewHost&) = delete;

    bool Initialize(HWND hwnd, const std::wstring& webDir,
                    std::function<void()> onReady);
    void Resize();
    void Close();
    void DrainReplies();
    void PostJson(const std::string& json);

    bool ready() const { return webview_ != nullptr; }

private:
    void ConfigureWebView();
    void Navigate();
    void OnControllerReady(HRESULT result,
                           ICoreWebView2Controller* controller);

    HWND hwnd_ = nullptr;
    std::wstring webDir_;
    std::function<void()> onReady_;
    std::unique_ptr<Ipc> ipc_;

    Microsoft::WRL::ComPtr<ICoreWebView2Environment> environment_;
    Microsoft::WRL::ComPtr<ICoreWebView2Controller> controller_;
    Microsoft::WRL::ComPtr<ICoreWebView2> webview_;
    Microsoft::WRL::ComPtr<ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler>
        environmentHandler_;
    Microsoft::WRL::ComPtr<ICoreWebView2CreateCoreWebView2ControllerCompletedHandler>
        controllerHandler_;

    EventRegistrationToken messageToken_{};
    EventRegistrationToken resourceToken_{};
    EventRegistrationToken navigationToken_{};
    EventRegistrationToken newWindowToken_{};
    EventRegistrationToken processFailedToken_{};
    bool handlersRegistered_ = false;
};
