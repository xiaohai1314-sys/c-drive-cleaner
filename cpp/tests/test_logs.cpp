#include <windows.h>

#include <string>
#include <vector>

#include "core/logs.h"
#include "harness.hpp"
#include "util/path.h"
#include "util/utf.h"

TEST(logs_reject_path_traversal) {
    CHECK(!logs::ValidLogName("../rules.json"));
    CHECK(!logs::ValidLogName("sub\\file.log"));
    CHECK(!logs::ValidLogName("..\\..\\windows\\win.ini"));
    CHECK(!logs::ValidLogName(""));
    CHECK(logs::ValidLogName("20260101_120000.log"));
    CHECK(!logs::Read("../rules.json").has_value());
}

TEST(logs_write_and_list) {
    const std::optional<std::string> path =
        logs::WriteLog({"第一行", "第二行"});
    CHECK(path.has_value());
    const std::string name = Basename(*path);
    const std::optional<std::string> text = logs::Read(name);
    CHECK(text.has_value());
    CHECK(text->find("第一行") != std::string::npos);
    const Json list = logs::List();
    bool found = false;
    for (const Json& item : list) {
        if (item.value("name", std::string()) == name) {
            found = true;
            break;
        }
    }
    CHECK(found);
    DeleteFileW(Utf8ToWide(*path).c_str());
}
