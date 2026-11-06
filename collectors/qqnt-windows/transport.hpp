#pragma once
#include <cstdint>
namespace qscope_probe {
constexpr std::uint32_t payload_limit = 1024 * 1024;
constexpr std::uint32_t command_limit = 512;
#pragma pack(push, 1)
struct Header {
    char magic[8];
    std::uint32_t version, header_size, pid, flags;
    std::uint64_t process_birth_filetime, started_filetime, started_qpc, qpc_frequency;
    std::uint32_t payload_limit, reserved;
};
struct Record {
    char magic[4];
    std::uint32_t total_size;
    std::uint16_t kind;
    std::uint8_t direction, flags;
    std::uint32_t pid, tid, error_code;
    std::uint64_t event_id, timestamp_filetime, qpc, sequence, original_length;
    std::uint32_t captured_length, command_length;
    std::uint64_t dropped_total;
};
#pragma pack(pop)
static_assert(sizeof(Header) == 64);
static_assert(sizeof(Record) == 80);
}
