#pragma once

#include <windows.h>

#include <string>
#include <string_view>
#include <vector>

std::string FormatWin32Error(DWORD code);
std::string HResultMessage(HRESULT hr);
std::wstring ExePathW();
std::wstring ExeDirW();
std::string ExeDirUtf8();
std::string LoadResourceText(int resourceId);

/// `%SystemRoot%`（去掉末尾反斜杠）与 `%SystemRoot%\System32\<name>`。
std::string WindowsDir();
std::string System32Path(const std::string& name);

/// 子进程运行结果（对应 Rust 的 `Command::output()`）。
struct ProcessResult {
    bool started = false;
    DWORD exitCode = 0;
    std::string out;
    std::string err;

    bool Success() const { return started && exitCode == 0; }
};

/// 运行可执行文件并捕获 stdout/stderr，不弹控制台窗口。
ProcessResult RunCapture(const std::wstring& exePath,
                         const std::vector<std::wstring>& arguments);

/// 直接按完整命令行运行并捕获输出（用于 `cmd /C` 这类内建命令）。
ProcessResult RunCommandLine(const std::wstring& commandLine);

/// 按 Windows 命令行规则拼接参数（处理引号与结尾反斜杠）。
std::wstring BuildCommandLine(const std::wstring& exePath,
                              const std::vector<std::wstring>& arguments);

/// 只启动、不等待（对应 Rust 的 `Command::spawn`）。
struct SpawnResult {
    bool started = false;
    DWORD error = 0;
};
SpawnResult SpawnCommandLine(const std::wstring& commandLine);

class UniqueHandle {
public:
    UniqueHandle() = default;
    explicit UniqueHandle(HANDLE handle) : handle_(handle) {}
    ~UniqueHandle() { reset(); }

    UniqueHandle(const UniqueHandle&) = delete;
    UniqueHandle& operator=(const UniqueHandle&) = delete;

    UniqueHandle(UniqueHandle&& other) noexcept : handle_(other.release()) {}
    UniqueHandle& operator=(UniqueHandle&& other) noexcept {
        if (this != &other) {
            reset();
            handle_ = other.release();
        }
        return *this;
    }

    HANDLE get() const { return handle_; }
    explicit operator bool() const {
        return handle_ != nullptr && handle_ != INVALID_HANDLE_VALUE;
    }

    HANDLE release() {
        HANDLE handle = handle_;
        handle_ = nullptr;
        return handle;
    }

    void reset(HANDLE handle = nullptr) {
        if (handle_ != nullptr && handle_ != INVALID_HANDLE_VALUE) {
            CloseHandle(handle_);
        }
        handle_ = handle;
    }

private:
    HANDLE handle_ = nullptr;
};
