#pragma once

#include <windows.h>

#include <memory>

#include "app/webview_host.h"

class MainWindow {
public:
    bool Create(HINSTANCE instance, int showCommand);
    int Run();

private:
    static LRESULT CALLBACK WndProc(HWND hwnd, UINT message, WPARAM wParam,
                                    LPARAM lParam);
    static LRESULT CALLBACK ChildSubclassProc(HWND hwnd, UINT message,
                                              WPARAM wParam, LPARAM lParam,
                                              UINT_PTR id, DWORD_PTR refData);
    LRESULT HandleMessage(UINT message, WPARAM wParam, LPARAM lParam);
    void OnCreate();
    void EnsureChildSubclass();
    void ResizeWebView();

    HINSTANCE instance_ = nullptr;
    HWND hwnd_ = nullptr;
    UINT dpi_ = 96;
    bool childSubclassed_ = false;
    std::unique_ptr<WebViewHost> webview_;
};
