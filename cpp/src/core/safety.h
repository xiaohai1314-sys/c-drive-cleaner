#pragma once

#include <windows.h>

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "util/win32.h"

namespace safety {

enum class DeleteOutcomeKind { Deleted, Skipped, Failed };

struct DeleteOutcome {
    DeleteOutcomeKind kind = DeleteOutcomeKind::Skipped;
    std::string message;
};

std::string Basename(std::string_view path);
std::string Normalize(std::string_view path);
bool UnderMarker(std::string_view path,
                 const std::vector<std::string>& markers);
bool IsDriveRoot(std::string_view path);
bool IsCriticalFile(std::string_view path);
bool IsNeverClean(std::string_view path);
bool IsPasswordStore(std::string_view path);
bool PathSyntaxError(std::string_view path);
bool IsSystemPath(std::string_view path);
bool IsProtectedPath(std::string_view path);
std::vector<std::string> UserLibraryRoots();
bool IsUserLibrary(std::string_view path);
bool IsWithin(std::string_view path, const std::vector<std::string>& roots);
bool IsReparsePoint(std::string_view path);
std::optional<bool> ReparsePointStatus(std::string_view path);
bool UnderRemoteAllowlist(std::string_view path);
bool ExecutionForbidden(std::string_view path, bool untrusted);
bool ExplicitDeleteForbidden(std::string_view path);
DeleteOutcome DeleteFileChecked(const std::string& path,
                                const std::vector<std::string>& roots,
                                bool untrusted);
bool MarkDelete(HANDLE handle);
size_t RemoveEmptyDirs(const std::string& root);

struct HandleChain {
    std::vector<UniqueHandle> handles;
};

bool PinDirectoryChecked(const std::string& path,
                         const std::vector<std::string>& roots,
                         HandleChain& chain, DWORD& error);
bool CreateDirectoryTreeChecked(const std::string& path, HandleChain& chain,
                                DWORD& error);
bool CreateDirectoryChecked(const std::string& path,
                            const std::vector<std::string>& roots,
                            HandleChain& chain, UniqueHandle& directory,
                            DWORD& error);
bool CreateNewFileChecked(const std::string& path,
                          const std::vector<std::string>& roots,
                          HandleChain& chain, UniqueHandle& file,
                          DWORD& error);
bool OpenReadFileChecked(const std::string& path,
                         const std::vector<std::string>& roots,
                         HandleChain& chain, UniqueHandle& file, DWORD& error);
bool RemoveDirectoryChecked(const std::string& path,
                            const std::vector<std::string>& roots,
                            bool& removed, DWORD& error);
bool RemoveTreeChecked(const std::string& root,
                       const std::vector<std::string>& allowedRoots,
                       uint64_t& removed, DWORD& error);
bool RenameDirectorySiblingChecked(
    const std::string& from, const std::string& to,
    const std::vector<std::string>& allowedRoots, DWORD& error);

}  // namespace safety
