#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include "probe.hpp"
#include "transport.hpp"
#include "client-profile.hpp"
#include "MinHook.h"
#include <algorithm>
#include <atomic>
#include <cstdio>
#include <cstring>
#include <cwchar>
#include <string>
#include <new>

namespace {
using namespace qscope_probe;
constexpr unsigned queue_capacity = 32;
constexpr std::uint64_t file_limit = 256ull * 1024 * 1024;
constexpr std::uint32_t account_limit = 32;
// 账号值随包传递，变化时单独写入状态记录。
struct PacketData {
    Record record; std::uint32_t account_length, account_type; unsigned char account[account_limit];
    unsigned char command[command_limit]; unsigned char payload[payload_limit];
};
struct Slot { std::atomic<std::uint64_t> sequence; PacketData packet; };
static_assert(std::atomic<std::uint64_t>::is_always_lock_free);
Slot* slots = nullptr;
std::atomic<std::uint64_t> queue_tail{0}, consumed{0};
std::uint64_t queue_head=0;
#ifdef QSCOPE_PROBE_SELFTEST
std::atomic<bool> pause_writer{false};
#endif
HANDLE output = INVALID_HANDLE_VALUE, wakeup = nullptr;
HANDLE completed = nullptr;
std::wstring directory, stop_path;
std::atomic<bool> active{false};
std::atomic<std::uint64_t> dropped{0}, invalid_layout{0};
std::atomic<std::uint64_t> queue_full{0}, producer_contention{0};
std::atomic<unsigned> copy_calls{0};
std::atomic<long> install_state{0};
std::uint64_t next_id = 0, bytes_written = 0;
void* send_target = nullptr;
void* recv_target = nullptr;
using Send = std::int64_t (__fastcall*)(void*, void**);
using Receive = std::int64_t (__fastcall*)(void*, void**, std::uint32_t);
Send original_send = nullptr;
Receive original_receive = nullptr;

std::uint64_t filetime_now() {
    FILETIME time; GetSystemTimePreciseAsFileTime(&time);
    return (std::uint64_t(time.dwHighDateTime) << 32) | time.dwLowDateTime;
}
std::uint64_t qpc_now() { LARGE_INTEGER time; QueryPerformanceCounter(&time); return time.QuadPart; }
bool copy_memory(void* destination, const void* source, std::size_t size) noexcept {
    if(size == 0) return true;
    if(!source) return false;
    __try { std::memcpy(destination, source, size); return true; }
    __except(EXCEPTION_EXECUTE_HANDLER) { return false; }
}
bool read_u64(std::uintptr_t address, std::uint64_t& value) noexcept {
    return copy_memory(&value, reinterpret_cast<void*>(address), sizeof(value));
}
bool qq_string(std::uintptr_t address, unsigned char* destination, std::uint32_t& length, std::uint32_t limit = command_limit) noexcept {
    // 54395 wrapper.node 使用 24 字节 libc++ 字符串，布局依据其复制例程及 MSF 收发调用点验证。
    std::uint64_t words[3];
    if(!copy_memory(words, reinterpret_cast<void*>(address), sizeof(words))) return false;
    std::uint64_t n = (words[0] & 1) ? words[1] : (words[0] & 0xff) >> 1;
    if(n > limit || (!(words[0] & 1) && n > 22)) return false;
    if((words[0] & 1) && ((words[0] & ~std::uint64_t(1)) < n)) return false;
    const auto data = (words[0] & 1) ? words[2] : address + 1;
    if(!copy_memory(destination, reinterpret_cast<void*>(data), static_cast<std::size_t>(n))) return false;
    length = static_cast<std::uint32_t>(n);
    return true;
}
bool write_bytes(const void* data, DWORD length) {
    auto* cursor = static_cast<const unsigned char*>(data);
    while(length) {
        DWORD written = 0;
        if(!WriteFile(output, cursor, length, &written, nullptr) || written == 0) return false;
        cursor += written; length -= written; bytes_written += written;
    }
    return true;
}
Record record_header(std::uint16_t kind, std::uint8_t direction = 0) {
    Record r{};
    std::memcpy(r.magic, "QSR1", 4);
    r.kind = kind; r.direction = direction;
    r.pid = GetCurrentProcessId(); r.tid = GetCurrentThreadId();
    r.timestamp_filetime = filetime_now(); r.qpc = qpc_now();
    return r;
}
bool write_slot(PacketData& slot) {
    slot.record.event_id = ++next_id;
    slot.record.total_size = sizeof(Record) + slot.record.command_length + slot.record.captured_length;
    slot.record.dropped_total = dropped.load(std::memory_order_relaxed);
    return write_bytes(&slot.record, sizeof(Record)) && write_bytes(slot.command, slot.record.command_length)
        && write_bytes(slot.payload, slot.record.captured_length);
}
void completion_marker(bool success) {
    const auto name=directory+L"\\closed."+std::to_wstring(GetCurrentProcessId());
    HANDLE file=CreateFileW(name.c_str(),GENERIC_WRITE,FILE_SHARE_READ,nullptr,CREATE_NEW,0,nullptr);
    if(file!=INVALID_HANDLE_VALUE) { const char* status=success?"closed":"write_failed"; DWORD n=0;WriteFile(file,status,static_cast<DWORD>(std::strlen(status)),&n,nullptr);CloseHandle(file); }
}
bool control_record(std::uint16_t kind, const char* text, std::uint32_t code = 0) {
    // 小型控制记录仅由工作线程或准备阶段写入。
    Record r = record_header(kind);
    r.command_length = static_cast<std::uint32_t>(std::strlen(text));
    r.total_size = sizeof(Record) + r.command_length;
    r.event_id = ++next_id; r.error_code = code;
    r.dropped_total = dropped.load(std::memory_order_relaxed);
    return write_bytes(&r, sizeof(r)) && write_bytes(text, r.command_length);
}
Slot* reserve_slot(std::uint64_t& position) noexcept {
    position=queue_tail.load(std::memory_order_relaxed);
    for(unsigned attempt=0;attempt<64;++attempt) {
        auto& slot=slots[position%queue_capacity];
        const auto sequence=slot.sequence.load(std::memory_order_acquire);
        if(sequence==position) {
            if(queue_tail.compare_exchange_weak(position,position+1,std::memory_order_relaxed)) return &slot;
        } else if(sequence<position) {
            queue_full.fetch_add(1,std::memory_order_relaxed);
            dropped.fetch_add(1,std::memory_order_relaxed);return nullptr;
        } else position=queue_tail.load(std::memory_order_relaxed);
    }
    producer_contention.fetch_add(1,std::memory_order_relaxed);
    dropped.fetch_add(1,std::memory_order_relaxed);return nullptr;
}
void publish_slot(Slot& slot,std::uint64_t position) noexcept {
    slot.sequence.store(position+1,std::memory_order_release);SetEvent(wakeup);
}
void enqueue_status(const char* text, std::uint32_t code = 0) {
    copy_calls.fetch_add(1,std::memory_order_acquire);
    if(active.load(std::memory_order_acquire)) {
        std::uint64_t position=0;
        if(auto* claimed=reserve_slot(position)) {
            auto& slot=claimed->packet;slot.record=record_header(2);slot.account_length=0;slot.record.error_code=code;
            slot.record.command_length=static_cast<std::uint32_t>(std::strlen(text));
            std::memcpy(slot.command,text,slot.record.command_length);publish_slot(*claimed,position);
        }
    }
    copy_calls.fetch_sub(1,std::memory_order_release);
}
bool copy_packet(PacketData& slot, void** owner, bool outbound, std::uint32_t error) noexcept {
    std::uint64_t packet = 0, request = 0, vector = 0, begin = 0, end = 0, capacity = 0, sequence = 0;
    if(!read_u64(reinterpret_cast<std::uintptr_t>(owner), packet)) return false;
    if(packet == 0 && !outbound) { slot.record.direction=1; slot.record.error_code=error; return true; }
    if(packet == 0) return false;
    const std::uintptr_t command = outbound ?
        (read_u64(packet, request) ? request : 0) : packet + 32;
    if(command == 0 || !qq_string(command, slot.command, slot.record.command_length)) return false;
    if(!read_u64(packet + (outbound ? 88 : 24), sequence)) return false;
    if(!read_u64((outbound ? request + 32 : packet + 56), vector) || vector == 0
        || !read_u64(vector, begin) || !read_u64(vector + 8, end) || !read_u64(vector + 16, capacity)) return false;
    if(end < begin || capacity < end || end-begin > 64ull * 1024 * 1024 || (begin == 0 && end != 0)) return false;
    if(outbound) {
        // 账号布局假设来自发送函数的 uin 与 uin_type 日志，非法账号值单独丢弃，原始包照常保留。
        std::uint32_t type = 0;
        if(!copy_memory(&type, reinterpret_cast<void*>(packet + qscope_profile::send_account_type_offset), sizeof(type))
            || !qq_string(packet + qscope_profile::send_account_offset, slot.account, slot.account_length, account_limit)) slot.account_length = 0;
        for(std::uint32_t i = 0; i < slot.account_length; ++i) {
            const auto c = slot.account[i];
            if(!((c >= '0' && c <= '9') || (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || c == '_' || c == '-')) { slot.account_length = 0; break; }
        }
        slot.account_type = type;
    }
    slot.record.direction = outbound ? 2 : 1;
    slot.record.error_code = error; slot.record.sequence = sequence;
    slot.record.original_length = end - begin;
    slot.record.captured_length = static_cast<std::uint32_t>(std::min<std::uint64_t>(end-begin, payload_limit));
    slot.record.flags = slot.record.captured_length != slot.record.original_length;
    return copy_memory(slot.payload, reinterpret_cast<void*>(begin), slot.record.captured_length);
}
void observe(void** owner, bool outbound, std::uint32_t error) noexcept {
    copy_calls.fetch_add(1, std::memory_order_acquire);
    if(active.load(std::memory_order_acquire)) {
        std::uint64_t position=0;
        if(auto* claimed=reserve_slot(position)) {
                auto& slot = claimed->packet; slot.record = record_header(1); slot.account_length = 0;
                if(!copy_packet(slot, owner, outbound, error)) {
                    dropped.fetch_add(1, std::memory_order_relaxed); invalid_layout.fetch_add(1, std::memory_order_relaxed);
                    slot.record=record_header(3);slot.account_length=0;constexpr char message[]="packet_read_failed";
                    slot.record.command_length=sizeof(message)-1;std::memcpy(slot.command,message,sizeof(message)-1);
                }
                publish_slot(*claimed,position);
        }
    }
    copy_calls.fetch_sub(1, std::memory_order_release);
}
std::int64_t __fastcall send_hook(void* self, void** owner) {
    const DWORD error=GetLastError();
    observe(owner, true, 0);
    SetLastError(error);
    return original_send(self, owner);
}
std::int64_t __fastcall receive_hook(void* self, void** owner, std::uint32_t error) {
    const DWORD last_error=GetLastError();
    observe(owner, false, error);
    SetLastError(last_error);
    return original_receive(self, owner, error);
}
DWORD WINAPI writer(void*) {
    auto* scratch = static_cast<PacketData*>(VirtualAlloc(nullptr, sizeof(PacketData), MEM_COMMIT|MEM_RESERVE, PAGE_READWRITE));
    if(!scratch) { active.store(false); control_record(4, "worker_allocation_failed", GetLastError()); CloseHandle(output); output=INVALID_HANDLE_VALUE; completion_marker(false);SetEvent(completed); return 1; }
    bool stopping = false, failed = false;
    new(scratch) PacketData;
    std::uint64_t last_invalid = 0,last_full=0,last_contention=0;
    unsigned char last_account[account_limit]{}; std::uint32_t last_account_length = 0, last_account_type = 0;
    const char* end_reason = "controller_stopped";
    for(;;) {
#ifdef QSCOPE_PROBE_SELFTEST
        if(pause_writer.load(std::memory_order_acquire)) {WaitForSingleObject(wakeup,5);continue;}
#endif
        if(!stopping && (GetFileAttributesW(stop_path.c_str()) != INVALID_FILE_ATTRIBUTES || bytes_written >= file_limit)) {
            if(bytes_written >= file_limit) end_reason = "file_limit_reached";
            active.store(false, std::memory_order_release);
            if(send_target) MH_DisableHook(send_target);
            if(recv_target) MH_DisableHook(recv_target);
            stopping = true;
        }
        bool have = false;
        auto& cell=slots[queue_head%queue_capacity];
        if(cell.sequence.load(std::memory_order_acquire)==queue_head+1) {
            const auto& source = cell.packet;
            scratch->record = source.record;
            scratch->account_length = source.account_length; scratch->account_type = source.account_type;
            std::memcpy(scratch->account, source.account, source.account_length);
            std::memcpy(scratch->command, source.command, source.record.command_length);
            std::memcpy(scratch->payload, source.payload, source.record.captured_length);
            cell.sequence.store(queue_head+queue_capacity,std::memory_order_release);
            ++queue_head;consumed.store(queue_head,std::memory_order_release);have=true;
        }
        if(have && scratch->record.kind == 1 && scratch->account_length && (scratch->account_length != last_account_length
            || scratch->account_type != last_account_type || std::memcmp(scratch->account, last_account, last_account_length))) {
            char text[96]; std::snprintf(text, sizeof(text), "account_observed type=%u uin=%.*s", scratch->account_type, static_cast<int>(scratch->account_length), reinterpret_cast<const char*>(scratch->account));
            if(!control_record(2, text)) { failed = true; break; }
            std::memcpy(last_account, scratch->account, scratch->account_length);
            last_account_length = scratch->account_length; last_account_type = scratch->account_type;
        }
        if(have && !write_slot(*scratch)) { failed = true; break; }
        const auto bad = invalid_layout.load(std::memory_order_relaxed);
        if(bad != last_invalid) { if(!control_record(2, "layout_rejected", static_cast<std::uint32_t>(bad))) { failed = true; break; } last_invalid=bad; }
        const auto full=queue_full.load(std::memory_order_relaxed),contention=producer_contention.load(std::memory_order_relaxed);
        if(full!=last_full) {if(!control_record(3,"queue_full",static_cast<std::uint32_t>(full))){failed=true;break;}last_full=full;}
        if(contention!=last_contention) {if(!control_record(3,"producer_contention",static_cast<std::uint32_t>(contention))){failed=true;break;}last_contention=contention;}
        if(!have) {
            FlushFileBuffers(output);
            if(stopping && copy_calls.load(std::memory_order_acquire) == 0) break;
            WaitForSingleObject(wakeup, 100);
        }
    }
    active.store(false, std::memory_order_release);
    if(!failed) { failed=!control_record(4, end_reason) || !FlushFileBuffers(output); }
    CloseHandle(output); output = INVALID_HANDLE_VALUE;
    VirtualFree(scratch, 0, MEM_RELEASE);
    completion_marker(!failed);
    SetEvent(completed);
    // DLL 和队列驻留至 QQ 退出，清理由进程地址空间回收完成。
    return failed ? 1 : 0;
}
std::wstring environment(const wchar_t* name) {
    wchar_t value[4096]; const auto n = GetEnvironmentVariableW(name, value, 4096);
    return n && n < 4096 ? std::wstring(value, n) : L"";
}
}

bool qscope_capture_enabled() { return slots != nullptr; }
bool qscope_capture_prepare() {
    directory = environment(L"QSCOPE_CAPTURE_DIR");
    if(directory.empty() || std::wcsstr(GetCommandLineW(), L"--type=")) return true;
    wchar_t image[4096];const auto image_size=GetModuleFileNameW(nullptr,image,4096);
    const auto root_image=environment(L"QSCOPE_ROOT_IMAGE");
    if(!image_size || image_size>=4096 || root_image.empty()) return false;
    if(_wcsicmp(image,root_image.c_str())!=0) return true;
    FILETIME birth, exit, kernel, user;
    if(!GetProcessTimes(GetCurrentProcess(), &birth, &exit, &kernel, &user)) return false;
    const std::uint64_t born = (std::uint64_t(birth.dwHighDateTime) << 32) | birth.dwLowDateTime;
    wchar_t filename[128]; std::swprintf(filename, 128, L"\\capture.%lu.%llu.qscap", GetCurrentProcessId(), born);
    output = CreateFileW((directory+filename).c_str(), GENERIC_WRITE, FILE_SHARE_READ, nullptr, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, nullptr);
    if(output == INVALID_HANDLE_VALUE) return false;
    Header header{};
    std::memcpy(header.magic, "QSCAP01\0", 8);
    header.version=1; header.header_size=sizeof(Header); header.pid=GetCurrentProcessId();
    header.process_birth_filetime=born; header.started_filetime=filetime_now(); header.started_qpc=qpc_now();
    LARGE_INTEGER frequency; QueryPerformanceFrequency(&frequency); header.qpc_frequency=frequency.QuadPart;
    header.payload_limit=payload_limit;
    if(!write_bytes(&header, sizeof(header)) || !control_record(2, "probe_started")) return false;
    slots=static_cast<Slot*>(VirtualAlloc(nullptr, sizeof(Slot)*queue_capacity, MEM_COMMIT|MEM_RESERVE, PAGE_READWRITE));
    wakeup=CreateEventW(nullptr, FALSE, FALSE, nullptr);
    completed=CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if(!slots || !wakeup || !completed) return false;
    for(unsigned i=0;i<queue_capacity;++i){new(&slots[i]) Slot;slots[i].sequence.store(i,std::memory_order_relaxed);}
    stop_path=directory+L"\\stop";
    active.store(true, std::memory_order_release);
    HANDLE thread=CreateThread(nullptr, 0, writer, nullptr, 0, nullptr);
    if(!thread) { active.store(false); return false; }
    CloseHandle(thread);
    enqueue_status("waiting_wrapper_module");
    return true;
}

void qscope_capture_module_loaded(HMODULE module) {
    if(!active.load(std::memory_order_acquire) || !module) return;
    wchar_t path[4096]; const auto n=GetModuleFileNameW(module,path,4096);
    if(!n || n>=4096) return;
    const auto slash=std::wcsrchr(path,L'\\');
    if(_wcsicmp(slash?slash+1:path,L"wrapper.node") != 0) return;
    long expected=0;
    if(!install_state.compare_exchange_strong(expected,1)) return;
    // 布局对应 Windows x64 QQ 9.9.37-54395，模块身份及唯一字符串引用由宿主验证，实际地址通过本次扫描传入。
    auto* base=reinterpret_cast<unsigned char*>(module);
    using namespace qscope_profile;
    const auto target_rva = [](const wchar_t* name) -> unsigned long {
        const auto value = environment(name);
        if(value.empty() || value.size() > 10) return 0;
        for(const auto ch : value) if(ch < L'0' || ch > L'9') return 0;
        wchar_t* end = nullptr;
        const auto rva = std::wcstoull(value.c_str(), &end, 10);
        return end && !*end && rva <= 0xffffffffull ? static_cast<unsigned long>(rva) : 0;
    };
    const auto send_rva = target_rva(L"QSCOPE_SEND_RVA");
    const auto recv_rva = target_rva(L"QSCOPE_RECV_RVA");
    unsigned char actual[32];
    if(!send_rva || !recv_rva || send_rva == recv_rva
        || !copy_memory(actual,base+send_rva,32) || std::memcmp(actual,send_prefix,32)
        || !copy_memory(actual,base+recv_rva,32) || std::memcmp(actual,recv_prefix,32)) {
        enqueue_status("module_profile_rejected"); install_state.store(-1); return;
    }
    send_target=base+send_rva; recv_target=base+recv_rva;
    auto status=MH_CreateHook(send_target,reinterpret_cast<void*>(send_hook),reinterpret_cast<void**>(&original_send));
    if(status==MH_OK) status=MH_CreateHook(recv_target,reinterpret_cast<void*>(receive_hook),reinterpret_cast<void**>(&original_receive));
    if(status==MH_OK) status=MH_QueueEnableHook(send_target);
    if(status==MH_OK) status=MH_QueueEnableHook(recv_target);
    if(status==MH_OK) status=MH_ApplyQueued();
    if(status!=MH_OK) {
        MH_DisableHook(send_target); MH_DisableHook(recv_target);
        enqueue_status("hook_install_failed",status); install_state.store(-1); return;
    }
    enqueue_status("hooks_ready"); install_state.store(2);
}
