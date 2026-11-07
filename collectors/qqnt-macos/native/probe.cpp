#include "layout.hpp"
#include "transport.hpp"
#include "dobby.h"
#include <atomic>
#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <dlfcn.h>
#include <fcntl.h>
#include <libproc.h>
#include <mach/mach.h>
#include <mach/mach_vm.h>
#include <mach-o/dyld.h>
#include <new>
#include <pthread.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

namespace {
using namespace qscope::macos;
constexpr uint64_t file_limit = 256ull * 1024 * 1024;
constexpr uint64_t epoch = 116444736000000000ull;
constexpr unsigned queue_capacity = 32;
struct Slot {
    std::atomic<uint64_t> turn{0};
    Record record{};
    Decoded packet{};
};
struct Probe {
    Slot slots[queue_capacity];
    std::atomic<uint64_t> tail{0}, dropped{0}, invalid{0}, calls{0};
    std::atomic<bool> active{true}, stop{false};
    std::atomic<int> installed{0};
    uint64_t head = 0, event_id = 0, bytes = 0;
    int output = -1;
    pthread_t worker{};
    char module[PATH_MAX]{}, stop_path[PATH_MAX]{}, closed_path[PATH_MAX]{};
    uint64_t rvas[2]{};
    unsigned char prefixes[2][16]{};
};
Probe* probe = nullptr;

uint64_t wall_time() noexcept {
    timespec value{};
    clock_gettime(CLOCK_REALTIME, &value);
    return epoch + uint64_t(value.tv_sec) * 10000000 + value.tv_nsec / 100;
}
uint64_t monotonic() noexcept {
    timespec value{};
    clock_gettime(CLOCK_MONOTONIC, &value);
    return uint64_t(value.tv_sec) * 1000000000 + value.tv_nsec;
}
bool read_memory(uint64_t address, void* output, size_t size) noexcept {
    if (size == 0) return true;
    if (address == 0 || address > UINT64_MAX - size) return false;
    mach_vm_size_t copied = 0;
    return mach_vm_read_overwrite(mach_task_self(), address, size,
               reinterpret_cast<mach_vm_address_t>(output), &copied) == KERN_SUCCESS && copied == size;
}
Record record(uint16_t kind) noexcept {
    Record r{};
    std::memcpy(r.magic, "QSR1", 4);
    r.kind = kind;
    r.pid = getpid();
    r.tid = pthread_mach_thread_np(pthread_self());
    r.timestamp_filetime = wall_time();
    r.qpc = monotonic();
    return r;
}
Slot* reserve(uint64_t& position) noexcept {
    position = probe->tail.load(std::memory_order_relaxed);
    for (unsigned attempt = 0; attempt < 64; ++attempt) {
        auto& slot = probe->slots[position % queue_capacity];
        const auto turn = slot.turn.load(std::memory_order_acquire);
        if (turn == position) {
            if (probe->tail.compare_exchange_weak(position, position + 1, std::memory_order_relaxed)) return &slot;
        } else if (turn < position) break;
        else position = probe->tail.load(std::memory_order_relaxed);
    }
    probe->dropped.fetch_add(1, std::memory_order_relaxed);
    return nullptr;
}
void status(const char* message, uint32_t error = 0) noexcept {
    probe->calls.fetch_add(1);
    if (!probe->active.load()) { probe->calls.fetch_sub(1); return; }
    uint64_t position = 0;
    if (auto* slot = reserve(position)) {
        slot->record = record(2);
        slot->record.error_code = error;
        slot->record.command_length = std::strlen(message);
        std::memcpy(slot->packet.command, message, slot->record.command_length);
        slot->turn.store(position + 1, std::memory_order_release);
    }
    probe->calls.fetch_sub(1);
}
void observe(uint8_t direction, DobbyRegisterContext* context) noexcept {
    const int saved_errno = errno;
    // 回调与写入线程的停止和计数检查共用全序，防止结束标记之后继续发布。
    probe->calls.fetch_add(1);
    if (probe->active.load()) {
        uint64_t position = 0;
        if (auto* slot = reserve(position)) {
            slot->record = record(1);
            if (decode(direction, context->general.regs.x1, read_memory, slot->packet)) {
                slot->record.direction = direction;
                slot->record.error_code = direction == 1 ? uint32_t(context->general.regs.x2) : 0;
                slot->record.sequence = slot->packet.sequence;
                slot->record.original_length = slot->packet.original_size;
                slot->record.captured_length = slot->packet.captured_length;
                slot->record.command_length = slot->packet.command_length;
                slot->record.flags = slot->packet.truncated;
            } else {
                probe->invalid.fetch_add(1, std::memory_order_relaxed);
                probe->dropped.fetch_add(1, std::memory_order_relaxed);
                slot->record = record(3);
                constexpr char reason[] = "packet_read_failed";
                slot->record.command_length = sizeof(reason) - 1;
                std::memcpy(slot->packet.command, reason, sizeof(reason) - 1);
            }
            slot->turn.store(position + 1, std::memory_order_release);
        }
    }
    probe->calls.fetch_sub(1);
    errno = saved_errno;
}
void send_callback(void*, DobbyRegisterContext* context) { observe(2, context); }
void receive_callback(void*, DobbyRegisterContext* context) { observe(1, context); }

bool write_bytes(const void* data, size_t length) {
    if (length > file_limit - probe->bytes) return false;
    const auto* bytes = static_cast<const unsigned char*>(data);
    while (length) {
        const auto count = write(probe->output, bytes, length);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) return false;
        bytes += count;
        length -= count;
        probe->bytes += count;
    }
    return true;
}
bool control(uint16_t kind, const char* text) {
    auto r = record(kind);
    r.event_id = ++probe->event_id;
    r.command_length = std::strlen(text);
    r.total_size = sizeof(r) + r.command_length;
    r.dropped_total = probe->dropped.load();
    return write_bytes(&r, sizeof(r)) && write_bytes(text, r.command_length);
}
void* writer(void*) {
    uint64_t last_dropped = 0;
    bool failed = false, limiting = false;
    for (;;) {
        if (access(probe->stop_path, F_OK) == 0) probe->stop.store(true);
        if (probe->stop.load()) probe->active.store(false);
        auto& slot = probe->slots[probe->head % queue_capacity];
        if (slot.turn.load(std::memory_order_acquire) == probe->head + 1) {
            auto& r = slot.record;
            r.total_size = sizeof(r) + r.command_length + r.captured_length;
            r.dropped_total = probe->dropped.load();
            if (probe->bytes + r.total_size + 1024 > file_limit) {
                limiting = true;
                probe->active.store(false);
                probe->stop.store(true);
                probe->dropped.fetch_add(1);
            } else if (!limiting && !failed) {
                r.event_id = ++probe->event_id;
                failed = !write_bytes(&r, sizeof(r)) || !write_bytes(slot.packet.command, r.command_length)
                    || !write_bytes(slot.packet.payload, r.captured_length);
            } else probe->dropped.fetch_add(1);
            slot.turn.store(probe->head + queue_capacity, std::memory_order_release);
            ++probe->head;
            if (failed) { probe->active.store(false); probe->stop.store(true); }
            continue;
        }
        const auto dropped = probe->dropped.load();
        if (!failed && dropped != last_dropped) {
            if (probe->bytes + sizeof(Record) + 12 + 256 > file_limit) {
                limiting = true;
                probe->active.store(false);
                probe->stop.store(true);
            } else failed = !control(3, "capture_loss");
            last_dropped = dropped;
        }
        if (probe->stop.load() && probe->calls.load() == 0
            && probe->head == probe->tail.load()) break;
        timespec pause{0, 5000000};
        nanosleep(&pause, nullptr);
    }
    if (!failed) failed = !control(4, limiting ? "file_limit_reached" : "controller_stopped");
    if (fsync(probe->output) != 0) failed = true;
    close(probe->output);
    probe->output = -1;
    int closed = open(probe->closed_path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0600);
    if (closed >= 0) {
        const char* state = failed ? "write_failed" : "closed";
        (void)write(closed, state, std::strlen(state));
        close(closed);
    }
    return nullptr;
}

// 在 dlopen 完成重定位及模块构造后、Node 获取模块并调用导出前执行。
void install() {
    if (!probe || !probe->active.load() || probe->installed.load() != 0) return;
    for (uint32_t index = 0, count = _dyld_image_count(); index < count; ++index) {
        const char* path = _dyld_get_image_name(index);
        if (!path || std::strcmp(path, probe->module) != 0) continue;
        int expected = 0;
        if (!probe->installed.compare_exchange_strong(expected, 1)) return;
        const auto base = reinterpret_cast<uintptr_t>(_dyld_get_image_header(index));
        void* targets[2]{};
        for (unsigned i = 0; i < 2; ++i) {
            unsigned char actual[16]{};
            targets[i] = reinterpret_cast<void*>(base + probe->rvas[i]);
            if (!read_memory(reinterpret_cast<uintptr_t>(targets[i]), actual, sizeof(actual))
                || std::memcmp(actual, probe->prefixes[i], sizeof(actual)) != 0) {
                status("module_profile_rejected"); probe->installed.store(-1); return;
            }
        }
        auto result = DobbyInstrument(targets[0], send_callback);
        if (result == 0) {
            result = DobbyInstrument(targets[1], receive_callback);
        }
        status(result == 0 ? "hooks_ready" : "hook_install_failed", result);
        // 模块构造函数可能已启动线程，保留仍可能正在执行的跳板。
        if (result != 0) probe->active.store(false);
        probe->installed.store(result == 0 ? 2 : -1);
        return;
    }
}
void* observed_dlopen(const char* path, int mode) {
    void* handle = dlopen(path, mode);
    const int saved_errno = errno;
    if (handle && probe && path && std::strstr(path, "wrapper.node")) install();
    errno = saved_errno;
    return handle;
}
__attribute__((used, section("__DATA,__interpose")))
static const struct { const void* replacement; const void* original; } interpose = {
    reinterpret_cast<const void*>(observed_dlopen), reinterpret_cast<const void*>(dlopen)
};
bool copy_env(const char* name, char* output, size_t capacity) {
    const char* value = std::getenv(name);
    if (!value || !value[0] || std::strlen(value) >= capacity) return false;
    std::strcpy(output, value);
    return true;
}
void finish() {
    if (!probe) return;
    probe->active.store(false);
    probe->stop.store(true);
    pthread_join(probe->worker, nullptr);
}
__attribute__((constructor)) void start() {
    char executable[PATH_MAX]{}, expected_executable[PATH_MAX]{};
    uint32_t capacity = sizeof(executable);
    if (!copy_env("QSCOPE_PROBE_EXECUTABLE", expected_executable, sizeof(expected_executable))
        || _NSGetExecutablePath(executable, &capacity) != 0
        || std::strcmp(executable, expected_executable) != 0) return;
    auto* state = new (std::nothrow) Probe;
    if (!state) return;
    if (!copy_env("QSCOPE_PROBE_MODULE", state->module, sizeof(state->module))
        || !copy_env("QSCOPE_PROBE_STOP", state->stop_path, sizeof(state->stop_path))
        || !copy_env("QSCOPE_PROBE_CLOSED", state->closed_path, sizeof(state->closed_path))) { delete state; return; }
    const char* names[2] = {"QSCOPE_PROBE_SEND", "QSCOPE_PROBE_RECEIVE"};
    for (unsigned i = 0; i < 2; ++i) {
        const char* value = std::getenv(names[i]);
        char prefix[33]{}, extra{};
        unsigned long long rva = 0;
        if (!value || std::sscanf(value, "%llx:%32[0-9a-f]%c", &rva, prefix, &extra) != 2
            || std::strlen(prefix) != 32 || rva == 0 || rva > 512 * 1024 * 1024) { delete state; return; }
        state->rvas[i] = rva;
        for (unsigned j = 0; j < 16; ++j) {
            unsigned byte = 0;
            std::sscanf(prefix + j * 2, "%2x", &byte);
            state->prefixes[i][j] = byte;
        }
    }
    char output[PATH_MAX]{};
    if (!copy_env("QSCOPE_PROBE_OUTPUT", output, sizeof(output))) { delete state; return; }
    state->output = open(output, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0600);
    if (state->output < 0) { delete state; return; }
    for (unsigned i = 0; i < queue_capacity; ++i) state->slots[i].turn.store(i);
    probe = state;
    Header header{};
    std::memcpy(header.magic, "QSCAP01", 8);
    header.version = 1; header.header_size = sizeof(header); header.pid = getpid();
    proc_bsdinfo info{};
    if (proc_pidinfo(getpid(), PROC_PIDTBSDINFO, 0, &info, sizeof(info)) == sizeof(info))
        header.process_birth_filetime = epoch + info.pbi_start_tvsec * 10000000 + info.pbi_start_tvusec * 10;
    header.started_filetime = wall_time(); header.started_qpc = monotonic();
    header.qpc_frequency = 1000000000; header.payload_limit = 1024 * 1024;
    if (!header.process_birth_filetime || !write_bytes(&header, sizeof(header)) || !control(2, "probe_started")
        || pthread_create(&state->worker, nullptr, writer, nullptr) != 0) {
        close(state->output); probe = nullptr; delete state; return;
    }
    std::atexit(finish);
    status("waiting_wrapper_module");
    install();
}
}
