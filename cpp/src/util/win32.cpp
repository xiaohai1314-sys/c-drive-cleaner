#include "util/win32.h"

#include <thread>
#include <utility>

#include "util/env.h"
#include "util/utf.h"

std::string FormatWin32Error(DWORD code) {
    LPWSTR buffer = nullptr;
    const DWORD length = FormatMessageW(
        FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM |
            FORMAT_MESSAGE_IGNORE_INSERTS,
        nullptr, code, MAKELANGID(LANG_NEUTRAL, SUBLANG_DEFAULT),
        reinterpret_cast<LPWSTR>(&buffer), 0, nullptr);
    std::wstring text;
    if (length != 0 && buffer != nullptr) {
        text.assign(buffer, length);
        while (!text.empty() && (text.back() == L'\r' || text.back() == L'\n')) {
            text.pop_back();
        }
    }
    if (buffer != nullptr) {
        LocalFree(buffer);
    }
    if (text.empty()) {
        return "错误码 " + std::to_string(code);
    }
    return WideToUtf8(text);
}

std::string HResultMessage(HRESULT hr) {
    if (HRESULT_FACILITY(hr) == FACILITY_WIN32) {
        return FormatWin32Error(HRESULT_CODE(hr));
    }
    LPWSTR buffer = nullptr;
    const DWORD length = FormatMessageW(
        FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM |
            FORMAT_MESSAGE_IGNORE_INSERTS,
        nullptr, static_cast<DWORD>(hr), MAKELANGID(LANG_NEUTRAL, SUBLANG_DEFAULT),
        reinterpret_cast<LPWSTR>(&buffer), 0, nullptr);
    std::wstring text;
    if (length != 0 && buffer != nullptr) {
        text.assign(buffer, length);
        while (!text.empty() && (text.back() == L'\r' || text.back() == L'\n')) {
            text.pop_back();
        }
    }
    if (buffer != nullptr) {
        LocalFree(buffer);
    }
    if (text.empty()) {
        return "HRESULT 0x" + std::to_string(static_cast<unsigned long>(hr));
    }
    return WideToUtf8(text);
}

std::wstring ExePathW() {
    std::wstring buffer(MAX_PATH, L'\0');
    for (;;) {
        const DWORD length = GetModuleFileNameW(nullptr, buffer.data(),
                                                static_cast<DWORD>(buffer.size()));
        if (length == 0) {
            return {};
        }
        if (length < buffer.size()) {
            buffer.resize(length);
            return buffer;
        }
        buffer.resize(buffer.size() * 2);
    }
}

std::wstring ExeDirW() {
    std::wstring path = ExePathW();
    const size_t slash = path.find_last_of(L"\\/");
    if (slash == std::wstring::npos) {
        return path;
    }
    return path.substr(0, slash);
}

std::string ExeDirUtf8() { return WideToUtf8(ExeDirW()); }

std::string WindowsDir() {
    std::string root = GetEnv("SystemRoot");
    if (root.empty()) {
        root = "C:\\Windows";
    }
    while (!root.empty() && root.back() == '\\') {
        root.pop_back();
    }
    return root;
}

std::string System32Path(const std::string& name) {
    return WindowsDir() + "\\System32\\" + name;
}

std::string LoadResourceText(int resourceId) {
    const HRSRC resource =
        FindResourceW(nullptr, MAKEINTRESOURCEW(resourceId), RT_RCDATA);
    if (resource == nullptr) {
        return {};
    }
    const DWORD size = SizeofResource(nullptr, resource);
    const HGLOBAL loaded = LoadResource(nullptr, resource);
    if (loaded == nullptr) {
        return {};
    }
    const void* data = LockResource(loaded);
    if (data == nullptr) {
        return {};
    }
    return std::string(static_cast<const char*>(data), size);
}

namespace {

std::wstring QuoteArgument(const std::wstring& argument) {
    if (argument.empty()) {
        return L"\"\"";
    }
    bool needsQuotes = false;
    for (const wchar_t ch : argument) {
        if (ch == L' ' || ch == L'\t' || ch == L'"') {
            needsQuotes = true;
            break;
        }
    }
    if (!needsQuotes) {
        return argument;
    }
    std::wstring out = L"\"";
    const wchar_t backslash = static_cast<wchar_t>(92);
    size_t backslashes = 0;
    for (const wchar_t ch : argument) {
        if (ch == backslash) {
            ++backslashes;
            continue;
        }
        if (ch == L'"') {
            out.append(backslashes * 2 + 1, backslash);
            out.push_back(L'"');
            backslashes = 0;
            continue;
        }
        out.append(backslashes, backslash);
        backslashes = 0;
        out.push_back(ch);
    }
    out.append(backslashes * 2, backslash);
    out.push_back(L'"');
    return out;
}

std::string ReadAll(HANDLE pipe) {
    std::string text;
    char buffer[8192];
    DWORD read = 0;
    while (ReadFile(pipe, buffer, sizeof(buffer), &read, nullptr) != 0 &&
           read > 0) {
        text.append(buffer, read);
    }
    return text;
}

}  // namespace

std::wstring BuildCommandLine(const std::wstring& exePath,
                              const std::vector<std::wstring>& arguments) {
    std::wstring commandLine = QuoteArgument(exePath);
    for (const std::wstring& argument : arguments) {
        commandLine.push_back(L' ');
        commandLine += QuoteArgument(argument);
    }
    return commandLine;
}

ProcessResult RunCapture(const std::wstring& exePath,
                         const std::vector<std::wstring>& arguments) {
    return RunCommandLine(BuildCommandLine(exePath, arguments));
}

ProcessResult RunCommandLine(const std::wstring& commandLine) {
    ProcessResult result;
    SECURITY_ATTRIBUTES attributes = {};
    attributes.nLength = sizeof(attributes);
    attributes.bInheritHandle = TRUE;

    HANDLE outRead = nullptr;
    HANDLE outWrite = nullptr;
    HANDLE errRead = nullptr;
    HANDLE errWrite = nullptr;
    if (CreatePipe(&outRead, &outWrite, &attributes, 0) == 0) {
        return result;
    }
    if (CreatePipe(&errRead, &errWrite, &attributes, 0) == 0) {
        CloseHandle(outRead);
        CloseHandle(outWrite);
        return result;
    }
    SetHandleInformation(outRead, HANDLE_FLAG_INHERIT, 0);
    SetHandleInformation(errRead, HANDLE_FLAG_INHERIT, 0);

    STARTUPINFOW startup = {};
    startup.cb = sizeof(startup);
    startup.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
    startup.wShowWindow = SW_HIDE;
    startup.hStdOutput = outWrite;
    startup.hStdError = errWrite;
    startup.hStdInput = nullptr;

    PROCESS_INFORMATION info = {};
    std::wstring mutableLine = commandLine;
    const BOOL created =
        CreateProcessW(nullptr, mutableLine.data(), nullptr, nullptr, TRUE,
                       CREATE_NO_WINDOW, nullptr, nullptr, &startup, &info);
    CloseHandle(outWrite);
    CloseHandle(errWrite);
    if (created == 0) {
        CloseHandle(outRead);
        CloseHandle(errRead);
        return result;
    }

    std::string out;
    std::string err;
    std::thread outThread([&out, outRead] { out = ReadAll(outRead); });
    std::thread errThread([&err, errRead] { err = ReadAll(errRead); });
    WaitForSingleObject(info.hProcess, INFINITE);
    DWORD exitCode = 0;
    GetExitCodeProcess(info.hProcess, &exitCode);
    outThread.join();
    errThread.join();
    CloseHandle(outRead);
    CloseHandle(errRead);
    CloseHandle(info.hThread);
    CloseHandle(info.hProcess);

    result.started = true;
    result.exitCode = exitCode;
    result.out = std::move(out);
    result.err = std::move(err);
    return result;
}

SpawnResult SpawnCommandLine(const std::wstring& commandLine) {
    SpawnResult result;
    STARTUPINFOW startup = {};
    startup.cb = sizeof(startup);
    PROCESS_INFORMATION info = {};
    std::wstring mutableLine = commandLine;
    if (CreateProcessW(nullptr, mutableLine.data(), nullptr, nullptr, FALSE,
                       CREATE_NO_WINDOW, nullptr, nullptr, &startup,
                       &info) == 0) {
        result.error = GetLastError();
        return result;
    }
    CloseHandle(info.hThread);
    CloseHandle(info.hProcess);
    result.started = true;
    return result;
}
