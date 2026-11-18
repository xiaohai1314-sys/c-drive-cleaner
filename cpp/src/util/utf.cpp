#include "util/utf.h"

#include <windows.h>

std::wstring Utf8ToWide(std::string_view text) {
    if (text.empty()) {
        return {};
    }
    const int size = MultiByteToWideChar(
        CP_UTF8, 0, text.data(), static_cast<int>(text.size()), nullptr, 0);
    if (size <= 0) {
        return {};
    }
    std::wstring out(static_cast<size_t>(size), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, text.data(), static_cast<int>(text.size()),
                        out.data(), size);
    return out;
}

std::string WideToUtf8(std::wstring_view text) {
    if (text.empty()) {
        return {};
    }
    const int size = WideCharToMultiByte(
        CP_UTF8, 0, text.data(), static_cast<int>(text.size()), nullptr, 0,
        nullptr, nullptr);
    if (size <= 0) {
        return {};
    }
    std::string out(static_cast<size_t>(size), '\0');
    WideCharToMultiByte(CP_UTF8, 0, text.data(), static_cast<int>(text.size()),
                        out.data(), size, nullptr, nullptr);
    return out;
}

bool IsValidUtf8(const std::string& text) {
    size_t index = 0;
    while (index < text.size()) {
        const unsigned char ch = static_cast<unsigned char>(text[index]);
        size_t extra = 0;
        if (ch < 0x80) {
            ++index;
            continue;
        }
        if ((ch & 0xE0) == 0xC0) {
            extra = 1;
            if (ch < 0xC2) {
                return false;
            }
        } else if ((ch & 0xF0) == 0xE0) {
            extra = 2;
        } else if ((ch & 0xF8) == 0xF0) {
            extra = 3;
            if (ch > 0xF4) {
                return false;
            }
        } else {
            return false;
        }
        if (index + extra >= text.size()) {
            return false;
        }
        for (size_t offset = 1; offset <= extra; ++offset) {
            if ((static_cast<unsigned char>(text[index + offset]) & 0xC0) !=
                0x80) {
                return false;
            }
        }
        index += extra + 1;
    }
    return true;
}
