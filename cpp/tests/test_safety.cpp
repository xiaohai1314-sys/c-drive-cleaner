#include "harness.hpp"

#include <windows.h>

#include <filesystem>
#include <string>
#include <string_view>
#include <vector>

#include "core/safety.h"
#include "core/timeutil.h"
#include "util/env.h"
#include "util/path.h"
#include "util/utf.h"

namespace {

std::string TempDir(const std::string& name) {
    wchar_t buffer[MAX_PATH];
    const DWORD length = GetTempPathW(MAX_PATH, buffer);
    const std::string base =
        WideToUtf8(std::wstring(buffer, length == 0 ? 0 : length));
    return safety::Normalize(JoinPath(
        base, "cdc-" + name + "-" + std::to_string(GetCurrentProcessId()) +
                  "-" + Stamp()));
}

void EnsureDir(const std::string& path) {
    std::error_code error;
    std::filesystem::create_directories(
        std::filesystem::path(Utf8ToWide(path)), error);
}

void WriteFileText(const std::string& path, std::string_view data) {
    HANDLE handle = CreateFileW(
        Utf8ToWide(path).c_str(), GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS,
        FILE_ATTRIBUTE_NORMAL, nullptr);
    if (handle == INVALID_HANDLE_VALUE) {
        return;
    }
    DWORD written = 0;
    WriteFile(handle, data.data(), static_cast<DWORD>(data.size()), &written,
              nullptr);
    CloseHandle(handle);
}

bool Exists(const std::string& path) {
    return GetFileAttributesW(Utf8ToWide(path).c_str()) !=
           INVALID_FILE_ATTRIBUTES;
}

void RemoveAll(const std::string& path) {
    std::error_code error;
    std::filesystem::remove_all(std::filesystem::path(Utf8ToWide(path)),
                                error);
}

bool MakeJunction(const std::string& link, const std::string& target) {
    std::wstring command = L"cmd.exe /D /C mklink /J \"" +
                           Utf8ToWide(link) + L"\" \"" +
                           Utf8ToWide(target) + L"\"";
    std::vector<wchar_t> mutableCommand(command.begin(), command.end());
    mutableCommand.push_back(L'\0');
    STARTUPINFOW startup = {sizeof(startup)};
    PROCESS_INFORMATION process = {};
    if (CreateProcessW(nullptr, mutableCommand.data(), nullptr, nullptr, FALSE,
                       CREATE_NO_WINDOW, nullptr, nullptr, &startup,
                       &process) == 0) {
        return false;
    }
    WaitForSingleObject(process.hProcess, 10000);
    DWORD code = 1;
    GetExitCodeProcess(process.hProcess, &code);
    CloseHandle(process.hProcess);
    CloseHandle(process.hThread);
    return code == 0;
}

}  // namespace

TEST(safety_normalize_keeps_windows_shape) {
    CHECK_EQ(safety::Normalize("C:/Windows/Temp/"),
             std::string("C:\\Windows\\Temp"));
    CHECK_EQ(safety::Normalize("c:\\Windows\\\\Temp"),
             std::string("C:\\Windows\\Temp"));
    CHECK_EQ(safety::Normalize("C:\\"), std::string("C:\\"));
}

TEST(safety_protected_paths_are_forbidden) {
    CHECK(safety::IsSystemPath("C:\\Windows\\System32\\kernel32.dll"));
    CHECK(safety::ExecutionForbidden(
        "C:\\Windows\\System32\\kernel32.dll", true));
    CHECK(!safety::ExecutionForbidden("C:\\Windows\\Temp\\a.tmp", false));
    CHECK(safety::ExecutionForbidden("C:\\pagefile.sys", false));
    CHECK(safety::ExecutionForbidden("C:\\Program Files\\App\\a.dll", true));
    CHECK(safety::ExecutionForbidden("C:\\$Recycle.Bin\\x", true));
    const std::string documents =
        JoinPath(GetEnv("USERPROFILE"), "Documents\\note.txt");
    CHECK(safety::ExecutionForbidden(documents, false));
}

TEST(safety_password_store_is_forbidden) {
    CHECK(safety::IsPasswordStore("C:\\x\\Login Data"));
    CHECK(safety::IsPasswordStore("C:\\x\\logins.json"));
    CHECK(!safety::IsPasswordStore("C:\\x\\data.bin"));
}

TEST(safety_temp_file_can_be_deleted_but_only_inside_roots) {
    const std::string dir = TempDir("delete");
    EnsureDir(dir);
    const std::string file = JoinPath(dir, "sample.bin");
    WriteFileText(file, "hello");
    const std::vector<std::string> roots = {dir};
    const std::vector<std::string> other = {JoinPath(dir, "other")};

    CHECK(safety::DeleteFileChecked(file, other, true).kind ==
          safety::DeleteOutcomeKind::Skipped);
    CHECK(Exists(file));
    CHECK(safety::DeleteFileChecked(file, roots, true).kind ==
          safety::DeleteOutcomeKind::Deleted);
    CHECK(!Exists(file));
    RemoveAll(dir);
}

TEST(safety_file_and_directory_operations_refuse_junction_components) {
    const std::string base = TempDir("junction");
    const std::string root = JoinPath(base, "root");
    const std::string outside = JoinPath(base, "outside");
    const std::string junction = JoinPath(root, "redirect");
    EnsureDir(root);
    EnsureDir(outside);
    const std::string outsideFile = JoinPath(outside, "keep.txt");
    WriteFileText(outsideFile, "must survive");
    WriteFileText(JoinPath(outside, "im_move_pending.json"),
                  "must not be read or overwritten");
    CHECK(MakeJunction(junction, outside));

    const std::vector<std::string> roots = {root};
    safety::HandleChain chain;
    UniqueHandle file;
    DWORD error = 0;
    CHECK(!safety::OpenReadFileChecked(
        JoinPath(junction, "im_move_pending.json"), roots, chain, file, error));
    CHECK(!safety::CreateNewFileChecked(JoinPath(junction, "new-journal.json"),
                                        roots, chain, file, error));
    safety::HandleChain treeChain;
    CHECK(!safety::CreateDirectoryTreeChecked(
        JoinPath(junction, "must-not-create"), treeChain, error));
    CHECK(!Exists(JoinPath(outside, "must-not-create")));
    CHECK(safety::DeleteFileChecked(JoinPath(junction, "keep.txt"), roots,
                                    false)
              .kind == safety::DeleteOutcomeKind::Skipped);
    bool removed = true;
    CHECK(safety::RemoveDirectoryChecked(junction, roots, removed, error));
    CHECK(!removed);
    CHECK(Exists(outsideFile));
    CHECK(Exists(junction));

    std::error_code removeError;
    std::filesystem::remove(std::filesystem::path(Utf8ToWide(junction)),
                            removeError);
    RemoveAll(base);
}

TEST(safety_directory_rename_uses_sibling_names) {
    const std::string base = TempDir("rename");
    const std::string source = JoinPath(base, "source");
    const std::string backup = JoinPath(base, "source_backup_20261116_200000");
    EnsureDir(source);
    WriteFileText(JoinPath(source, "data.bin"), "keep data");
    const std::vector<std::string> allowed = {source};
    DWORD error = 0;

    CHECK(safety::RenameDirectorySiblingChecked(source, backup, allowed, error));
    CHECK(!Exists(source));
    CHECK(Exists(JoinPath(backup, "data.bin")));
    CHECK(safety::RenameDirectorySiblingChecked(backup, source, allowed, error));
    CHECK(Exists(JoinPath(source, "data.bin")));
    RemoveAll(base);
}

TEST(safety_user_library_and_system_detection) {
    CHECK(safety::IsSystemPath("C:\\Windows\\Temp"));
    CHECK(!safety::IsSystemPath(
        "C:\\Users\\Administrator\\AppData\\Local\\Temp"));
    CHECK(safety::IsWithin(
        "C:\\Users\\Administrator\\AppData\\Local\\Temp\\a\\b.txt",
        {"C:\\Users\\Administrator\\AppData\\Local\\Temp"}));
}

TEST(safety_explicit_delete_allows_user_folders_but_blocks_system) {
    const std::string profile = GetEnv("USERPROFILE");
    CHECK(!safety::ExplicitDeleteForbidden(
        JoinPath(profile, "Downloads\\movie.mp4")));
    CHECK(!safety::ExplicitDeleteForbidden(
        JoinPath(profile, "Documents\\WeChat Files\\a.jpg")));
    CHECK(safety::ExplicitDeleteForbidden(
        "C:\\Windows\\System32\\kernel32.dll"));
    CHECK(safety::ExplicitDeleteForbidden("C:\\pagefile.sys"));
    CHECK(safety::ExplicitDeleteForbidden(
        JoinPath(profile,
                 "AppData\\Local\\Google\\Chrome\\User Data\\Default\\Login "
                 "Data")));
    CHECK(safety::ExplicitDeleteForbidden("C:\\Program Files\\App\\a.dll"));
    CHECK(safety::ExplicitDeleteForbidden("C:\\"));
}
