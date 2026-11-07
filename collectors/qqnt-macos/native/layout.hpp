#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>
#include <type_traits>

namespace qscope::macos {

inline constexpr std::size_t command_limit = 512;
inline constexpr std::size_t payload_limit = 1024 * 1024;
inline constexpr std::uint64_t original_limit = 64 * 1024 * 1024;

// This is a bounded snapshot for the inspected ARM64 module, not a QQ protocol schema.
// The owner/layout offsets require a matching module identity before installing probes.
struct Decoded {
    std::uint64_t sequence;
    std::uint64_t original_size;
    std::uint32_t command_length;
    std::uint32_t captured_length;
    bool truncated;
    std::uint8_t command[command_limit];
    std::uint8_t payload[payload_limit];
};

namespace detail {

inline bool address_range(std::uint64_t address, std::size_t size) noexcept {
    return address != 0 && size != 0 &&
           size - 1 <= std::numeric_limits<std::uint64_t>::max() - address;
}

inline bool object_pointer(std::uint64_t address) noexcept {
    return address != 0 && (address & 7) == 0;
}

inline std::uint64_t little_u64(const std::uint8_t* bytes) noexcept {
    std::uint64_t value = 0;
    for (unsigned index = 0; index < 8; ++index) {
        value |= std::uint64_t{bytes[index]} << (index * 8);
    }
    return value;
}

template <class Read>
bool read_bytes(Read& read, std::uint64_t address, void* destination,
                std::size_t size) noexcept {
    return size == 0 || (address_range(address, size) && read(address, destination, size));
}

template <class Read>
bool read_field(Read& read, std::uint64_t object, std::uint64_t offset,
                std::uint64_t& value) noexcept {
    if (!object_pointer(object) || offset > std::numeric_limits<std::uint64_t>::max() - object) {
        return false;
    }
    std::uint8_t bytes[8];
    if (!read_bytes(read, object + offset, bytes, sizeof(bytes))) return false;
    value = little_u64(bytes);
    return true;
}

// Darwin ARM64 libc++ alternate string layout: short bytes start at +0, short length
// is byte +23; its high bit selects the long {data +0, size +8, capacity/flag +16} form.
// Target evidence: command comparison at module RVA 0x42e70 and imported string ctors.
template <class Read>
bool read_command(Read& read, std::uint64_t address, Decoded& output,
                  std::uint8_t (&header)[24], std::uint32_t& length) noexcept {
    if (!object_pointer(address) || !read_bytes(read, address, header, sizeof(header))) {
        return false;
    }
    const std::uint8_t tag = header[23];
    if ((tag & 0x80) == 0) {
        length = tag;
        if (length > 22 || header[length] != 0) return false;
        if (length != 0) std::memcpy(output.command, header, length);
        return true;
    }
    const auto data = little_u64(header);
    const auto size = little_u64(header + 8);
    const auto allocated = little_u64(header + 16) & 0x7fffffffffffffffULL;
    if (size > command_limit || allocated < 24 || size >= allocated ||
        !address_range(data, static_cast<std::size_t>(allocated))) {
        return false;
    }
    length = static_cast<std::uint32_t>(size);
    std::uint8_t terminator = 0;
    return read_bytes(read, data, output.command, length) &&
           read_bytes(read, data + size, &terminator, 1) && terminator == 0;
}

}  // namespace detail

// direction: 1 inbound, 2 outbound. x1 is a pointer to a single-pointer owner slot,
// not a shared_ptr control block. Reader performs checked copying, e.g. with
// mach_vm_read_overwrite, and returns true only when every requested byte was read.
// The caller supplies Decoded in a queue slot; never place this 1 MiB object on a hook stack.
template <class Read>
bool decode(std::uint8_t direction, std::uint64_t owner_x1, Read read,
            Decoded& output) noexcept {
    static_assert(std::is_nothrow_invocable_r_v<bool, Read&, std::uint64_t, void*, std::size_t>,
                  "The memory reader must return bool and be noexcept");
    output.command_length = 0;
    output.captured_length = 0;
    output.sequence = 0;
    output.original_size = 0;
    output.truncated = false;
    if (direction != 1 && direction != 2) return false;

    std::uint64_t object = 0;
    if (!detail::read_field(read, owner_x1, 0, object) || !detail::object_pointer(object)) {
        return false;
    }
    std::uint64_t message = object;
    std::uint64_t sequence = 0;
    if (direction == 2 &&
        (!detail::read_field(read, object, 0, message) || !detail::object_pointer(message))) {
        return false;
    }
    if (!detail::read_field(read, object, direction == 1 ? 0x18 : 0x58, sequence)) {
        return false;
    }
    const std::uint64_t command_offset = direction == 1 ? 0x20 : 0;
    if (command_offset > std::numeric_limits<std::uint64_t>::max() - message) return false;
    const auto command_address = message + command_offset;
    std::uint8_t command_header[24];
    std::uint32_t command_length = 0;
    if (!detail::read_command(read, command_address, output, command_header, command_length)) {
        return false;
    }

    std::uint64_t vector = 0;
    if (!detail::read_field(read, message, direction == 1 ? 0x38 : 0x20, vector) ||
        !detail::object_pointer(vector)) {
        return false;
    }
    std::uint8_t vector_header[24];
    if (!detail::read_bytes(read, vector, vector_header, sizeof(vector_header))) return false;
    const auto begin = detail::little_u64(vector_header);
    const auto end = detail::little_u64(vector_header + 8);
    const auto capacity_end = detail::little_u64(vector_header + 16);
    if (end < begin || capacity_end < end ||
        (begin == 0 && (end != 0 || capacity_end != 0))) {
        return false;
    }
    const auto original = end - begin;
    if (original > original_limit) return false;
    const auto captured = static_cast<std::uint32_t>(
        original < payload_limit ? original : payload_limit);
    if (!detail::read_bytes(read, begin, output.payload, captured)) return false;

    // Reject observable owner/container changes during copying. This is not an atomic
    // snapshot guarantee: callers still rely on the instrumented entry's object lifetime.
    std::uint64_t current_object = 0;
    std::uint64_t current_message = 0;
    std::uint64_t current_vector = 0;
    std::uint8_t current_header[24];
    if (!detail::read_field(read, owner_x1, 0, current_object) || current_object != object ||
        (direction == 2 &&
         (!detail::read_field(read, object, 0, current_message) || current_message != message)) ||
        !detail::read_field(read, message, direction == 1 ? 0x38 : 0x20, current_vector) ||
        current_vector != vector ||
        !detail::read_bytes(read, vector, current_header, sizeof(current_header)) ||
        std::memcmp(current_header, vector_header, sizeof(current_header)) != 0 ||
        !detail::read_bytes(read, command_address, current_header, sizeof(current_header)) ||
        std::memcmp(current_header, command_header, sizeof(current_header)) != 0) {
        return false;
    }
    output.sequence = sequence;
    output.original_size = original;
    output.command_length = command_length;
    output.captured_length = captured;
    output.truncated = original > captured;
    return true;
}

}  // namespace qscope::macos
