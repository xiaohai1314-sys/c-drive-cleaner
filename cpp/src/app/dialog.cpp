#include "app/dialog.h"

#include <shobjidl.h>
#include <wrl/client.h>

#include <stdexcept>
#include <string>

#include "util/utf.h"
#include "util/win32.h"

Json PickPathsDialog(HWND owner, const Json& options) {
    Microsoft::WRL::ComPtr<IFileOpenDialog> dialog;
    HRESULT hr = CoCreateInstance(CLSID_FileOpenDialog, nullptr,
                                  CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&dialog));
    if (FAILED(hr)) {
        throw std::runtime_error("打开文件选择器失败: " + HResultMessage(hr));
    }

    DWORD flags = 0;
    dialog->GetOptions(&flags);
    flags |= FOS_FORCEFILESYSTEM | FOS_ALLOWMULTISELECT | FOS_PATHMUSTEXIST;

    bool directory = false;
    if (options.is_object()) {
        const auto directoryIt = options.find("directory");
        if (directoryIt != options.end() && directoryIt->is_boolean()) {
            directory = directoryIt->get<bool>();
        }
        const auto titleIt = options.find("title");
        if (titleIt != options.end() && titleIt->is_string()) {
            const std::wstring title = Utf8ToWide(titleIt->get<std::string>());
            dialog->SetTitle(title.c_str());
        }
    }
    if (directory) {
        flags |= FOS_PICKFOLDERS;
    }
    dialog->SetOptions(flags);

    hr = dialog->Show(owner);
    if (hr == HRESULT_FROM_WIN32(ERROR_CANCELLED)) {
        return nullptr;
    }
    if (FAILED(hr)) {
        throw std::runtime_error("文件选择失败: " + HResultMessage(hr));
    }

    Microsoft::WRL::ComPtr<IShellItemArray> items;
    hr = dialog->GetResults(&items);
    if (FAILED(hr)) {
        throw std::runtime_error("读取选择结果失败: " + HResultMessage(hr));
    }

    DWORD count = 0;
    items->GetCount(&count);
    Json result = Json::array();
    for (DWORD index = 0; index < count; ++index) {
        Microsoft::WRL::ComPtr<IShellItem> item;
        if (FAILED(items->GetItemAt(index, &item))) {
            continue;
        }
        PWSTR path = nullptr;
        if (SUCCEEDED(item->GetDisplayName(SIGDN_FILESYSPATH, &path)) &&
            path != nullptr) {
            result.push_back(WideToUtf8(path));
            CoTaskMemFree(path);
        }
    }
    if (result.empty()) {
        return nullptr;
    }
    return result;
}
