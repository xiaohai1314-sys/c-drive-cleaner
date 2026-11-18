#include "harness.hpp"

#include "core/task.h"
#include "core/timeutil.h"
#include "util/env.h"
#include "util/path.h"

TEST(normalize_keeps_windows_shape) {
    CHECK_EQ(NormalizePath("c:/Windows//Temp/"), std::string("C:\\Windows\\Temp"));
    CHECK_EQ(NormalizePath("C:\\"), std::string("C:\\"));
    CHECK_EQ(NormalizePath("C:"), std::string("C:"));
}

TEST(basename_and_drive_root) {
    CHECK_EQ(Basename("C:\\Windows\\Temp\\a.txt"), std::string("a.txt"));
    CHECK_EQ(Basename("C:\\Windows\\Temp\\"), std::string(""));
    CHECK(IsDriveRoot("C:\\"));
    CHECK(IsDriveRoot("c:"));
    CHECK(!IsDriveRoot("C:\\Windows"));
}

TEST(expand_env_keeps_unknown) {
    CHECK(!ExpandEnv("%SystemRoot%").empty());
    CHECK_EQ(ExpandEnv("%CDC_NOT_SET_123%\\x"),
             std::string("%CDC_NOT_SET_123%\\x"));
    CHECK_EQ(ExpandEnv("no-percent"), std::string("no-percent"));
}

TEST(time_stamp_shape) {
    CHECK_EQ(Stamp().size(), size_t(15));
    CHECK_EQ(HumanStamp(0).size(), size_t(19));
}

TEST(task_lifecycle) {
    task::Begin("test-task", "测试任务", false, true, 10, "项");
    task::AddDone("test-task", 3);
    Json status = task::StatusJson();
    CHECK(status.contains("test-task"));
    CHECK_EQ(status["test-task"]["done"].get<uint64_t>(), uint64_t(3));
    CHECK(task::Cancel("test-task"));
    CHECK(task::IsCancelled("test-task"));
    task::Finish("test-task", "done", "完成");
    status = task::StatusJson();
    CHECK_EQ(status["test-task"]["outcome"].get<std::string>(),
             std::string("done"));
}
