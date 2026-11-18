#include "util/env.h"

#include <windows.h>

#include "util/utf.h"

std::string GetEnv(const std::string& name) {
    if (name.empty()) {
        return {};
    }
    const std::wstring wideName = Utf8ToWide(name);
    std::wstring buffer(256, L'\0');
    for (;;) {
        const DWORD length = GetEnvironmentVariableW(
            wideName.c_str(), buffer.data(), static_cast<DWORD>(buffer.size()));
        if (length == 0) {
            return {};
        }
        if (length < buffer.size()) {
            buffer.resize(length);
            return WideToUtf8(buffer);
        }
        buffer.resize(length);
    }
}

std::string ExpandEnv(const std::string& input) {
    std::string out;
    out.reserve(input.size());
    size_t index = 0;
    while (index < input.size()) {
        if (input[index] != '%') {
            out.push_back(input[index]);
            ++index;
            continue;
        }
        const size_t close = input.find('%', index + 1);
        if (close == std::string::npos) {
            out.append(input, index, std::string::npos);
            break;
        }
        const std::string name = input.substr(index + 1, close - index - 1);
        const std::string value = GetEnv(name);
        if (!value.empty()) {
            out.append(value);
        } else {
            out.append(input, index, close - index + 1);
        }
        index = close + 1;
    }
    return out;
}
