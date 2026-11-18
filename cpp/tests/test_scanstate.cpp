#include <string>

#include "core/scanstate.h"
#include "harness.hpp"

TEST(scanstate_lifecycle_and_cancel) {
    scanstate::Begin("big", "大文件扫描");
    Json status = scanstate::StatusJson();
    CHECK(status.contains("big"));
    CHECK(status["big"]["active"].get<bool>());
    CHECK_EQ(status["big"]["stage"].get<std::string>(), std::string("大文件扫描"));
    CHECK(!status["big"]["cancelled"].get<bool>());
    CHECK(!scanstate::WasCancelled("big"));

    scanstate::Stage("big", "枚举文件");
    scanstate::Progress("big", 3, 10);
    scanstate::SetFound("big", 7);
    status = scanstate::StatusJson();
    CHECK_EQ(status["big"]["stage"].get<std::string>(), std::string("枚举文件"));
    CHECK_EQ(status["big"]["done"].get<uint64_t>(), static_cast<uint64_t>(3));
    CHECK_EQ(status["big"]["total"].get<uint64_t>(), static_cast<uint64_t>(10));
    CHECK_EQ(status["big"]["found"].get<uint64_t>(), static_cast<uint64_t>(7));

    scanstate::Cancel("big");
    CHECK(scanstate::WasCancelled("big"));
    CHECK(scanstate::StatusJson()["big"]["cancelled"].get<bool>());

    scanstate::End("big");
    CHECK(!scanstate::StatusJson()["big"]["active"].get<bool>());

    // 已结束的扫描不会被取消。
    scanstate::Begin("frag", "碎片分析");
    scanstate::End("frag");
    scanstate::Cancel("frag");
    CHECK(!scanstate::WasCancelled("frag"));

    // kind 为空时取消所有正在跑的扫描。
    scanstate::Begin("dup", "重复文件扫描");
    scanstate::Cancel("");
    CHECK(scanstate::WasCancelled("dup"));
    scanstate::End("dup");
}
