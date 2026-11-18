#include "core/process.h"

#include <windows.h>
#include <tlhelp32.h>

#include <cctype>
#include <string>
#include <unordered_set>
#include <utility>
#include <vector>

#include "util/utf.h"

namespace {

std::string LowerAscii(std::string text) {
    for (char& ch : text) {
        ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    }
    return text;
}

std::string TrimAscii(const std::string& text) {
    size_t begin = 0;
    size_t end = text.size();
    while (begin < end &&
           std::isspace(static_cast<unsigned char>(text[begin])) != 0) {
        ++begin;
    }
    while (end > begin &&
           std::isspace(static_cast<unsigned char>(text[end - 1])) != 0) {
        --end;
    }
    return text.substr(begin, end - begin);
}

std::string EntryName(const PROCESSENTRY32W& entry) {
    size_t length = 0;
    while (length < MAX_PATH && entry.szExeFile[length] != L'\0') {
        ++length;
    }
    const std::wstring wide(entry.szExeFile, length);
    return LowerAscii(TrimAscii(WideToUtf8(wide)));
}

}  // namespace

namespace process {

std::unordered_set<std::string> RunningProcesses() {
    std::unordered_set<std::string> names;
    const HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    if (snapshot == INVALID_HANDLE_VALUE) {
        return names;
    }
    PROCESSENTRY32W entry = {};
    entry.dwSize = sizeof(entry);
    if (Process32FirstW(snapshot, &entry) != 0) {
        do {
            std::string name = EntryName(entry);
            if (!name.empty()) {
                names.insert(std::move(name));
            }
        } while (Process32NextW(snapshot, &entry) != 0);
    }
    CloseHandle(snapshot);
    return names;
}

std::vector<std::string> CheckRunning(
    const std::vector<std::string>& targets) {
    const std::unordered_set<std::string> running = RunningProcesses();
    std::vector<std::string> found;
    for (const std::string& target : targets) {
        const std::string lowered = LowerAscii(target);
        if (running.find(lowered) != running.end()) {
            found.push_back(lowered);
        }
    }
    return found;
}

size_t KillProcessByName(const std::string& targetName) {
    const std::string target = LowerAscii(TrimAscii(targetName));
    size_t killed = 0;
    const HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    if (snapshot == INVALID_HANDLE_VALUE) {
        return 0;
    }
    PROCESSENTRY32W entry = {};
    entry.dwSize = sizeof(entry);
    if (Process32FirstW(snapshot, &entry) != 0) {
        do {
            if (EntryName(entry) != target) {
                continue;
            }
            const HANDLE handle =
                OpenProcess(PROCESS_TERMINATE, FALSE, entry.th32ProcessID);
            if (handle != nullptr && handle != INVALID_HANDLE_VALUE) {
                if (TerminateProcess(handle, 1) != 0) {
                    ++killed;
                }
                CloseHandle(handle);
            }
        } while (Process32NextW(snapshot, &entry) != 0);
    }
    CloseHandle(snapshot);
    return killed;
}

}  // namespace process
