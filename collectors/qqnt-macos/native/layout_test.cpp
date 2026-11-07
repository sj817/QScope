#include "layout.hpp"

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <memory>
#include <string>
#include <utility>
#include <vector>

namespace {
using qscope::macos::Decoded;
constexpr std::uint64_t owner = 0x1000;
constexpr std::uint64_t task = 0x2000;
constexpr std::uint64_t message = 0x3000;
constexpr std::uint64_t response = 0x4000;
constexpr std::uint64_t vector_address = 0x5000;
constexpr std::uint64_t long_command = 0x10000;
constexpr std::uint64_t payload = 0x100003;
constexpr std::uint64_t sequence = 0xfedcba9876543210ULL;
unsigned checks = 0;

void expect(bool condition, const char* message) {
    ++checks;
    if (!condition) {
        std::fprintf(stderr, "layout test failed: %s\n", message);
        std::exit(1);
    }
}

struct Region {
    std::uint64_t address;
    std::vector<std::uint8_t> bytes;
};

struct Memory {
    std::vector<Region> regions;
    std::size_t largest_read = 0;
    std::uint64_t fail_address = 0;
    std::uint64_t mutate_address = 0;
    unsigned mutate_reads = 0;

    std::vector<std::uint8_t>& add(std::uint64_t address, std::size_t size) {
        regions.push_back({address, std::vector<std::uint8_t>(size)});
        return regions.back().bytes;
    }

    bool read(std::uint64_t address, void* output, std::size_t size) noexcept {
        largest_read = std::max(largest_read, size);
        if (address == fail_address) return false;
        for (auto& region : regions) {
            if (address < region.address) continue;
            const auto offset = address - region.address;
            if (offset > region.bytes.size() || size > region.bytes.size() - offset) continue;
            std::memcpy(output, region.bytes.data() + offset, size);
            if (address == mutate_address && ++mutate_reads > 1 && size != 0) {
                static_cast<std::uint8_t*>(output)[0] ^= 8;
            }
            return true;
        }
        return false;
    }

    auto reader() noexcept {
        return [this](std::uint64_t address, void* output, std::size_t size) noexcept {
            return read(address, output, size);
        };
    }

    void word(std::uint64_t address, std::uint64_t value) {
        for (auto& region : regions) {
            if (address < region.address) continue;
            const auto offset = address - region.address;
            if (offset > region.bytes.size() || 8 > region.bytes.size() - offset) continue;
            for (unsigned index = 0; index < 8; ++index) {
                region.bytes[offset + index] = static_cast<std::uint8_t>(value >> (index * 8));
            }
            return;
        }
        expect(false, "test word has an owned region");
    }
};

Memory fixture(std::uint8_t direction, const std::string& command, std::uint64_t original = 3) {
    Memory memory;
    memory.regions.reserve(7);
    memory.add(owner, 8);
    memory.add(task, 0xd8);
    memory.add(message, 0x40);
    memory.add(response, 0xb0);
    memory.add(vector_address, 24);
    memory.word(owner, direction == 1 ? response : task);
    memory.word(task, message);
    memory.word(task + 0x58, sequence);
    memory.word(response + 0x18, sequence);
    memory.word(message + 0x20, vector_address);
    memory.word(response + 0x38, vector_address);
    const auto address = direction == 1 ? response + 0x20 : message;
    for (auto& region : memory.regions) {
        if (address < region.address || address + 24 > region.address + region.bytes.size()) continue;
        auto* bytes = region.bytes.data() + (address - region.address);
        if (command.size() <= 22) {
            std::memcpy(bytes, command.data(), command.size());
            bytes[23] = static_cast<std::uint8_t>(command.size());
        } else {
            memory.word(address, long_command);
            memory.word(address + 8, command.size());
            memory.word(address + 16, (command.size() + 32) | (1ULL << 63));
        }
        break;
    }
    if (command.size() > 22) {
        auto& bytes = memory.add(long_command, command.size() + 1);
        std::copy(command.begin(), command.end(), bytes.begin());
    }
    if (original != 0) {
        memory.word(vector_address, payload);
        memory.word(vector_address + 8, payload + original);
        memory.word(vector_address + 16, payload + original + 32);
        auto& bytes = memory.add(payload, static_cast<std::size_t>(
            std::min(original, std::uint64_t{qscope::macos::payload_limit})));
        for (std::size_t index = 0; index < bytes.size(); ++index) {
            bytes[index] = static_cast<std::uint8_t>(index * 17 + 8);
        }
    }
    return memory;
}

bool decode(Memory& memory, std::uint8_t direction, Decoded& output) {
    return qscope::macos::decode(direction, owner, memory.reader(), output);
}

void check_directions_and_string_boundaries(Decoded& output) {
    for (std::uint8_t direction : {1, 2}) {
        for (std::size_t length : {0U, 8U, 22U, 23U, 512U}) {
            const std::string command(length, 'C');
            auto memory = fixture(direction, command);
            expect(decode(memory, direction, output), "both directions and short/long commands decode");
            expect(output.sequence == sequence, "64-bit sequence is preserved");
            expect(output.command_length == length, "command length is exact");
            expect(std::memcmp(output.command, command.data(), length) == 0, "command bytes are exact");
            expect(output.original_size == 3 && output.captured_length == 3 && !output.truncated,
                   "small payload is complete");
            expect(output.payload[0] == 8 && output.payload[1] == 25 && output.payload[2] == 42,
                   "body copy is byte-exact and supports an unaligned byte buffer");
        }
        auto oversized = fixture(direction, std::string(513, 'X'));
        expect(!decode(oversized, direction, output), "commands larger than 512 fail");
        auto malformed_short = fixture(direction, "ok");
        const auto command_address = direction == 1 ? response + 0x20 : message;
        malformed_short.word(command_address + 16, std::uint64_t{23} << 56);
        expect(!decode(malformed_short, direction, output), "impossible short-string size fails");
        auto bad_capacity = fixture(direction, std::string(23, 'L'));
        bad_capacity.word(command_address + 16, (1ULL << 63) | 23);
        expect(!decode(bad_capacity, direction, output), "long-string allocation must include its terminator");
        auto unreadable = fixture(direction, std::string(23, 'L'));
        unreadable.fail_address = long_command;
        expect(!decode(unreadable, direction, output), "unreadable command storage fails");
        auto unterminated = fixture(direction, std::string(23, 'L'));
        for (auto& region : unterminated.regions) {
            if (region.address == long_command) region.bytes.back() = 'X';
        }
        expect(!decode(unterminated, direction, output), "unterminated command fails");
    }
}

void check_payload_boundaries(Decoded& output) {
    for (const auto original : {std::uint64_t{0}, std::uint64_t{qscope::macos::payload_limit},
                               std::uint64_t{qscope::macos::payload_limit + 1},
                               qscope::macos::original_limit}) {
        auto memory = fixture(1, "Inbound.Command", original);
        expect(decode(memory, 1, output), "bounded original payload length decodes");
        const auto captured = std::min(original, std::uint64_t{qscope::macos::payload_limit});
        expect(output.original_size == original && output.captured_length == captured,
               "original and copied lengths remain distinct");
        expect(output.truncated == (captured != original), "truncation is observable");
        expect(memory.largest_read <= qscope::macos::payload_limit, "reader never receives a request above 1 MiB");
    }
    auto oversized = fixture(2, "Outbound.Command", qscope::macos::original_limit + 1);
    expect(!decode(oversized, 2, output), "payload originals above 64 MiB fail");
    auto reversed = fixture(2, "Outbound.Command");
    reversed.word(vector_address + 8, payload - 1);
    expect(!decode(reversed, 2, output), "end before begin fails");
    auto no_capacity = fixture(2, "Outbound.Command");
    no_capacity.word(vector_address + 16, payload + 2);
    expect(!decode(no_capacity, 2, output), "end beyond capacity fails");
    auto partial_null = fixture(1, "Inbound.Command", 0);
    partial_null.word(vector_address + 16, payload);
    expect(!decode(partial_null, 1, output), "inconsistent null vector fails");
    auto failed_copy = fixture(2, "Outbound.Command");
    failed_copy.fail_address = payload;
    expect(!decode(failed_copy, 2, output), "unreadable body fails");
    expect(output.command_length == 0 && output.captured_length == 0 && output.original_size == 0,
           "failed copies publish no stale lengths");
}

void check_invalid_pointers_and_changes(Decoded& output) {
    auto memory = fixture(1, "Inbound.Command");
    for (const auto invalid : {std::uint64_t{0}, std::uint64_t{0x1001},
                               std::uint64_t{0x9000}, std::uint64_t{0xfffffffffffffff8ULL}}) {
        expect(!qscope::macos::decode(1, invalid, memory.reader(), output), "invalid owner fails safely");
    }
    expect(!decode(memory, 0, output) && !decode(memory, 3, output), "unknown directions fail");
    auto null_owner = fixture(1, "Inbound.Command");
    null_owner.word(owner, 0);
    expect(!decode(null_owner, 1, output), "null owned object fails");
    auto null_message = fixture(2, "Outbound.Command");
    null_message.word(task, 0);
    expect(!decode(null_message, 2, output), "null outbound message fails");
    auto overflow = fixture(1, std::string(23, 'L'));
    overflow.word(response + 0x20, 0xfffffffffffffff0ULL);
    expect(!decode(overflow, 1, output), "overflowing string storage fails");
    auto changed_owner = fixture(1, "Inbound.Command");
    changed_owner.mutate_address = owner;
    expect(!decode(changed_owner, 1, output), "owner changes during copy fail");
    auto changed_vector = fixture(2, "Outbound.Command");
    changed_vector.mutate_address = vector_address;
    expect(!decode(changed_vector, 2, output), "vector header changes during copy fail");
    auto changed_message = fixture(2, "Outbound.Command");
    changed_message.mutate_address = task;
    expect(!decode(changed_message, 2, output), "outbound message owner changes during copy fail");
    auto changed_command = fixture(1, "Inbound.Command");
    changed_command.mutate_address = response + 0x20;
    expect(!decode(changed_command, 1, output), "command header changes during copy fail");
}

void check_local_darwin_libcxx(Decoded& output) {
#if defined(__APPLE__) && defined(__aarch64__) && defined(_LIBCPP_VERSION)
    static_assert(sizeof(std::string) == 24);
    static_assert(sizeof(std::vector<std::uint8_t>) == 24);
    for (const std::size_t size : {22U, 23U, 96U}) {
        const std::string command(size, 'D');
        auto memory = fixture(1, "replace");
        for (auto& region : memory.regions) {
            if (region.address == response) {
                std::memcpy(region.bytes.data() + 0x20, &command, sizeof(command));
            }
        }
        if (size > 22) {
            auto& bytes = memory.add(reinterpret_cast<std::uintptr_t>(command.data()), size + 1);
            std::copy(command.begin(), command.end(), bytes.begin());
        }
        expect(decode(memory, 1, output), "SDK libc++ short/long object representations agree");
        expect(output.command_length == size && std::memcmp(output.command, command.data(), size) == 0,
               "SDK libc++ command bytes agree");
    }
    std::vector<std::uint8_t> bytes = {8, 1, 18, 0};
    bytes.reserve(64);
    std::uint8_t header[24];
    std::memcpy(header, &bytes, sizeof(header));
    expect(qscope::macos::detail::little_u64(header) == reinterpret_cast<std::uintptr_t>(bytes.data()),
           "SDK vector begins with its data pointer");
    expect(qscope::macos::detail::little_u64(header + 8) - qscope::macos::detail::little_u64(header) == bytes.size(),
           "SDK vector end is byte-granular");
    expect(qscope::macos::detail::little_u64(header + 16) - qscope::macos::detail::little_u64(header) == bytes.capacity(),
           "SDK vector third pointer gives capacity independently of size");
#else
    (void)output;
#endif
}
}  // namespace

int main() {
    auto output = std::make_unique<Decoded>();
    check_directions_and_string_boundaries(*output);
    check_payload_boundaries(*output);
    check_invalid_pointers_and_changes(*output);
    check_local_darwin_libcxx(*output);
    std::printf("{\"checks\":%u,\"result\":\"passed\",\"copy_limit\":%zu,\"original_limit\":%llu,\"command_limit\":%zu}\n",
                checks, qscope::macos::payload_limit,
                static_cast<unsigned long long>(qscope::macos::original_limit), qscope::macos::command_limit);
}
