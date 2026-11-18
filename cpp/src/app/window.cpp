#include "app/window.h"

#include <commctrl.h>
#include <windowsx.h>

#include <string>

#include "app/messages.h"
#include "app/resource.h"
#include "util/utf.h"
#include "util/win32.h"

namespace {

constexpr wchar_t kClassName[] = L"CDriveCleanerCppWindow";
constexpr int kBaseWidth = 1040;
constexpr int kBaseHeight = 680;
constexpr int kMinWidth = 980;
constexpr int kMinHeight = 640;
constexpr int kBorderDip = 8;

HWND FindDescendantByClass(HWND parent, const wchar_t* className) {
    HWND child = nullptr;
    while ((child = FindWindowExW(parent, child, nullptr, nullptr)) != nullptr) {
        wchar_t name[256] = {};
        GetClassNameW(child, name, 256);
        if (wcscmp(name, className) == 0) {
            return child;
        }
        if (HWND nested = FindDescendantByClass(child, className)) {
            return nested;
        }
    }
    return nullptr;
}

int Scaled(UINT dpi, int dip) { return MulDiv(dip, static_cast<int>(dpi), 96); }

}  // namespace

bool MainWindow::Create(HINSTANCE instance, int showCommand) {
    instance_ = instance;
    dpi_ = GetDpiForSystem();

    WNDCLASSEXW windowClass = {};
    windowClass.cbSize = sizeof(windowClass);
    windowClass.style = CS_HREDRAW | CS_VREDRAW;
    windowClass.lpfnWndProc = WndProc;
    windowClass.hInstance = instance_;
    windowClass.hCursor = LoadCursorW(nullptr, IDC_ARROW);
    windowClass.hbrBackground = CreateSolidBrush(RGB(245, 246, 248));
    windowClass.lpszClassName = kClassName;
    windowClass.hIcon = LoadIconW(instance_, MAKEINTRESOURCEW(IDI_APP_ICON));
    windowClass.hIconSm = windowClass.hIcon;
    if (RegisterClassExW(&windowClass) == 0) {
        return false;
    }

    const int width = Scaled(dpi_, kBaseWidth);
    const int height = Scaled(dpi_, kBaseHeight);
    RECT workArea;
    SystemParametersInfoW(SPI_GETWORKAREA, 0, &workArea, 0);
    const int x = workArea.left + (workArea.right - workArea.left - width) / 2;
    const int y = workArea.top + (workArea.bottom - workArea.top - height) / 2;

    const DWORD style = WS_POPUP | WS_THICKFRAME | WS_MINIMIZEBOX |
                        WS_MAXIMIZEBOX | WS_CLIPCHILDREN;
    hwnd_ = CreateWindowExW(0, kClassName, L"磁盘清理助手", style, x, y, width,
                            height, nullptr, nullptr, instance_, this);
    if (hwnd_ == nullptr) {
        return false;
    }
    ShowWindow(hwnd_, showCommand);
    UpdateWindow(hwnd_);
    return true;
}

int MainWindow::Run() {
    MSG message;
    while (GetMessageW(&message, nullptr, 0, 0) > 0) {
        TranslateMessage(&message);
        DispatchMessageW(&message);
    }
    return static_cast<int>(message.wParam);
}

void MainWindow::OnCreate() {
    webview_ = std::make_unique<WebViewHost>();
    const std::wstring webDir = Utf8ToWide(ExeDirUtf8() + "\\web");
    webview_->Initialize(hwnd_, webDir, [this] { EnsureChildSubclass(); });
    SetTimer(hwnd_, 1, 200, nullptr);
}

void MainWindow::EnsureChildSubclass() {
    if (childSubclassed_ || hwnd_ == nullptr) {
        return;
    }
    HWND child = FindDescendantByClass(hwnd_, L"Chrome_WidgetWin_0");
    if (child == nullptr) {
        return;
    }
    SetWindowSubclass(child, ChildSubclassProc, 1,
                      reinterpret_cast<DWORD_PTR>(this));
    childSubclassed_ = true;
    KillTimer(hwnd_, 1);
}

void MainWindow::ResizeWebView() {
    if (webview_) {
        webview_->Resize();
    }
}

LRESULT CALLBACK MainWindow::WndProc(HWND hwnd, UINT message, WPARAM wParam,
                                     LPARAM lParam) {
    MainWindow* self = reinterpret_cast<MainWindow*>(
        GetWindowLongPtrW(hwnd, GWLP_USERDATA));
    if (message == WM_NCCREATE) {
        auto* create = reinterpret_cast<CREATESTRUCTW*>(lParam);
        self = reinterpret_cast<MainWindow*>(create->lpCreateParams);
        self->hwnd_ = hwnd;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA,
                          reinterpret_cast<LONG_PTR>(self));
        return TRUE;
    }
    if (self == nullptr) {
        return DefWindowProcW(hwnd, message, wParam, lParam);
    }
    return self->HandleMessage(message, wParam, lParam);
}

LRESULT MainWindow::HandleMessage(UINT message, WPARAM wParam, LPARAM lParam) {
    switch (message) {
        case WM_CREATE:
            OnCreate();
            return 0;
        case WM_NCCALCSIZE:
            if (wParam != 0) {
                return 0;
            }
            break;
        case WM_GETMINMAXINFO: {
            auto* info = reinterpret_cast<MINMAXINFO*>(lParam);
            info->ptMinTrackSize.x = Scaled(dpi_, kMinWidth);
            info->ptMinTrackSize.y = Scaled(dpi_, kMinHeight);
            HMONITOR monitor =
                MonitorFromWindow(hwnd_, MONITOR_DEFAULTTONEAREST);
            MONITORINFO monitorInfo = {sizeof(monitorInfo)};
            if (GetMonitorInfoW(monitor, &monitorInfo)) {
                info->ptMaxPosition.x =
                    monitorInfo.rcWork.left - monitorInfo.rcMonitor.left;
                info->ptMaxPosition.y =
                    monitorInfo.rcWork.top - monitorInfo.rcMonitor.top;
                info->ptMaxSize.x =
                    monitorInfo.rcWork.right - monitorInfo.rcWork.left;
                info->ptMaxSize.y =
                    monitorInfo.rcWork.bottom - monitorInfo.rcWork.top;
                info->ptMaxTrackSize = info->ptMaxSize;
            }
            return 0;
        }
        case WM_NCHITTEST: {
            const LRESULT hit =
                DefWindowProcW(hwnd_, message, wParam, lParam);
            if (hit != HTCLIENT || IsZoomed(hwnd_) != 0) {
                return hit;
            }
            POINT point = {GET_X_LPARAM(lParam), GET_Y_LPARAM(lParam)};
            RECT rect;
            GetWindowRect(hwnd_, &rect);
            const int border = Scaled(dpi_, kBorderDip);
            const bool left = point.x < rect.left + border;
            const bool right = point.x >= rect.right - border;
            const bool top = point.y < rect.top + border;
            const bool bottom = point.y >= rect.bottom - border;
            if (top && left) return HTTOPLEFT;
            if (top && right) return HTTOPRIGHT;
            if (bottom && left) return HTBOTTOMLEFT;
            if (bottom && right) return HTBOTTOMRIGHT;
            if (left) return HTLEFT;
            if (right) return HTRIGHT;
            if (top) return HTTOP;
            if (bottom) return HTBOTTOM;
            return HTCLIENT;
        }
        case WM_DPICHANGED: {
            dpi_ = HIWORD(wParam);
            auto* suggested = reinterpret_cast<RECT*>(lParam);
            SetWindowPos(hwnd_, nullptr, suggested->left, suggested->top,
                         suggested->right - suggested->left,
                         suggested->bottom - suggested->top,
                         SWP_NOZORDER | SWP_NOACTIVATE);
            ResizeWebView();
            return 0;
        }
        case WM_SIZE:
            ResizeWebView();
            return 0;
        case WM_TIMER:
            if (wParam == 1) {
                EnsureChildSubclass();
            }
            return 0;
        case WM_APP_REPLY:
            if (webview_) {
                webview_->DrainReplies();
            }
            return 0;
        case WM_ERASEBKGND:
            return 1;
        case WM_PAINT: {
            PAINTSTRUCT paint;
            BeginPaint(hwnd_, &paint);
            EndPaint(hwnd_, &paint);
            return 0;
        }
        case WM_CLOSE:
            if (webview_) {
                webview_->Close();
            }
            DestroyWindow(hwnd_);
            return 0;
        case WM_DESTROY:
            KillTimer(hwnd_, 1);
            PostQuitMessage(0);
            return 0;
        default:
            break;
    }
    return DefWindowProcW(hwnd_, message, wParam, lParam);
}

LRESULT CALLBACK MainWindow::ChildSubclassProc(HWND hwnd, UINT message,
                                               WPARAM wParam, LPARAM lParam,
                                               UINT_PTR id, DWORD_PTR refData) {
    auto* self = reinterpret_cast<MainWindow*>(refData);
    if (message == WM_NCHITTEST && self != nullptr && self->hwnd_ != nullptr &&
        IsZoomed(self->hwnd_) == 0) {
        POINT point = {GET_X_LPARAM(lParam), GET_Y_LPARAM(lParam)};
        ScreenToClient(self->hwnd_, &point);
        RECT client;
        GetClientRect(self->hwnd_, &client);
        const int border = Scaled(self->dpi_, kBorderDip);
        if (point.x < border || point.y < border ||
            point.x >= client.right - border ||
            point.y >= client.bottom - border) {
            return HTTRANSPARENT;
        }
    }
    if (message == WM_NCDESTROY) {
        RemoveWindowSubclass(hwnd, ChildSubclassProc, id);
    }
    return DefSubclassProc(hwnd, message, wParam, lParam);
}
