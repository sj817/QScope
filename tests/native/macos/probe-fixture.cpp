#include <cstdint>
#include <atomic>
static std::atomic<std::uint64_t> calls{0};
extern "C" __attribute__((visibility("default"), noinline))
std::uint64_t qscope_test_send(void* self, void** owner, std::uint64_t error) {
    calls.fetch_add(1, std::memory_order_relaxed);
    return reinterpret_cast<std::uintptr_t>(self) + reinterpret_cast<std::uintptr_t>(owner) + error;
}
extern "C" __attribute__((visibility("default"), noinline))
std::uint64_t qscope_test_receive(void* self, void** owner, std::uint64_t error) {
    calls.fetch_add(1, std::memory_order_relaxed);
    return reinterpret_cast<std::uintptr_t>(self) + reinterpret_cast<std::uintptr_t>(owner) + error + 7;
}
