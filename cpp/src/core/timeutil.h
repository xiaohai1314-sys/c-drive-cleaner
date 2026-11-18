#pragma once

#include <cstdint>
#include <string>

struct LocalTimeParts {
    int year = 0;
    int month = 0;
    int day = 0;
    int hour = 0;
    int minute = 0;
    int second = 0;
};

uint64_t NowUnixSeconds();
uint64_t NowMs();
LocalTimeParts LocalTimeFromUnix(uint64_t seconds);
std::string MonthLabel(uint64_t seconds);
std::string Stamp();
std::string HumanStamp(uint64_t seconds);
