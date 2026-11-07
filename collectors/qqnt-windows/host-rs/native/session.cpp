#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <winternl.h>
#include <sddl.h>
#include <aclapi.h>
#include <objbase.h>
#include <algorithm>
#include <array>
#include <atomic>
#include <filesystem>
#include <fstream>
#include <memory>
#include <map>
#include <cwctype>
#include <set>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

namespace fs = std::filesystem;
namespace {
thread_local std::wstring last_error;

void check(bool success, const char* operation) {
    if (!success) throw std::runtime_error(std::string(operation) + " (Win32 " + std::to_string(GetLastError()) + ")");
}

struct Handle {
    HANDLE value = nullptr;
    Handle() = default;
    explicit Handle(HANDLE handle) : value(handle) {}
    Handle(const Handle&) = delete;
    Handle& operator=(const Handle&) = delete;
    ~Handle() { reset(); }
    void reset(HANDLE handle = nullptr) {
        if (value && value != INVALID_HANDLE_VALUE) CloseHandle(value);
        value = handle;
    }
    bool valid() const { return value && value != INVALID_HANDLE_VALUE; }
};

struct LocalMemory {
    void* value = nullptr;
    ~LocalMemory() { if (value) LocalFree(value); }
};

std::vector<wchar_t> wide(const std::wstring& text) {
    std::vector<wchar_t> result(text.begin(), text.end());
    result.push_back(0);
    return result;
}

std::wstring sid_text(PSID sid) {
    LocalMemory text;
    check(ConvertSidToStringSidW(sid, reinterpret_cast<LPWSTR*>(&text.value)), "Read SID");
    return static_cast<wchar_t*>(text.value);
}

std::wstring uuid() {
    GUID id{};
    check(SUCCEEDED(CoCreateGuid(&id)), "Create session identity");
    wchar_t text[40]{};
    check(StringFromGUID2(id, text, 40) > 0, "Format session identity");
    std::wstring result;
    for (wchar_t c : text) if (iswxdigit(c)) result.push_back(c);
    return result;
}

struct Options {
    const wchar_t* executable;
    const wchar_t* state;
    const wchar_t* helper;
    const wchar_t* build;
    const wchar_t* registry;
    const wchar_t* run;
    const wchar_t* capture;
    unsigned port;
    unsigned flags;
    unsigned send_rva;
    unsigned recv_rva;
};

struct BrokerRequest {
    DWORD magic, state, access, mode, count, output, input, timeout, length, error;
    UINT64 handle;
    DWORD descriptor_bytes, caller, inherit, operation;
    wchar_t name[1024];
    BYTE descriptor[8192];
};
static_assert(offsetof(BrokerRequest, name) == 64);
static_assert(offsetof(BrokerRequest, descriptor) == 2112);

class Session {
    Handle token, process, thread, job, mapping, request, response, mutex, outside_pipe;
    LocalMemory write_sid;
    BYTE* view = nullptr;
    std::thread broker;
    std::atomic<bool> stopping{false};
    std::wstring user_sid, session_sid, private_sid, sddl, state_root, registry_root, registry_canary;
    std::wstring canary, acl_canary, acl_before;
    std::vector<std::wstring> granted;
    std::wstring pipe_prefix = L"QScope." + uuid() + L".";
    std::set<std::wstring> pipes;
    bool cleaned = false;
    bool registry_created = false, registry_canary_created = false;

    void grant(const std::wstring& path, SE_OBJECT_TYPE type, ACCESS_MODE mode) {
        LocalMemory previous, next;
        PACL acl = nullptr;
        DWORD error = GetNamedSecurityInfoW(path.c_str(), type, DACL_SECURITY_INFORMATION, nullptr, nullptr, &acl, nullptr, &previous.value);
        check(error == ERROR_SUCCESS, "Read private ACL");
        EXPLICIT_ACCESSW access{};
        access.grfAccessPermissions = GENERIC_ALL;
        access.grfAccessMode = mode;
        access.grfInheritance = type == SE_FILE_OBJECT ? SUB_CONTAINERS_AND_OBJECTS_INHERIT : SUB_CONTAINERS_ONLY_INHERIT;
        access.Trustee.TrusteeForm = TRUSTEE_IS_SID;
        access.Trustee.TrusteeType = TRUSTEE_IS_UNKNOWN;
        access.Trustee.ptstrName = static_cast<wchar_t*>(write_sid.value);
        error = SetEntriesInAclW(1, &access, acl, reinterpret_cast<PACL*>(&next.value));
        check(error == ERROR_SUCCESS, "Build private ACL");
        error = SetNamedSecurityInfoW(const_cast<wchar_t*>(path.c_str()), type, DACL_SECURITY_INFORMATION, nullptr, nullptr, static_cast<PACL>(next.value), nullptr);
        check(error == ERROR_SUCCESS, "Apply private ACL");
    }

    static std::wstring directory_acl(const std::wstring& path) {
        LocalMemory descriptor, text;
        DWORD error = GetNamedSecurityInfoW(path.c_str(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, nullptr, nullptr, nullptr, nullptr, &descriptor.value);
        check(error == ERROR_SUCCESS, "Read canary ACL");
        check(ConvertSecurityDescriptorToStringSecurityDescriptorW(descriptor.value, SDDL_REVISION_1, DACL_SECURITY_INFORMATION, reinterpret_cast<LPWSTR*>(&text.value), nullptr), "Format canary ACL");
        return static_cast<wchar_t*>(text.value);
    }

    void create_token() {
        Handle original;
        check(OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &original.value), "Open caller token");
        check(!IsTokenRestricted(original.value), "Parent token is already restricted");
        auto information = [&](TOKEN_INFORMATION_CLASS type) {
            DWORD length = 0;
            GetTokenInformation(original.value, type, nullptr, 0, &length);
            check(length > 0 && length < 65536, "Read token information size");
            std::vector<UINT64> bytes((length + 7) / 8);
            check(GetTokenInformation(original.value, type, bytes.data(), length, &length), "Read token information");
            return bytes;
        };
        auto user = information(TokenUser);
        user_sid = sid_text(reinterpret_cast<TOKEN_USER*>(user.data())->User.Sid);
        auto groups = information(TokenGroups);
        auto list = reinterpret_cast<TOKEN_GROUPS*>(groups.data());
        PSID logon = nullptr;
        for (DWORD index = 0; index < list->GroupCount; index++) {
            if ((list->Groups[index].Attributes & SE_GROUP_LOGON_ID) == SE_GROUP_LOGON_ID) logon = list->Groups[index].Sid;
        }
        check(logon != nullptr, "Read logon SID");
        session_sid = sid_text(logon);
        GUID random{};
        check(SUCCEEDED(CoCreateGuid(&random)), "Create write SID");
        auto numbers = reinterpret_cast<DWORD*>(&random);
        private_sid = L"S-1-5-21-" + std::to_wstring(numbers[0]) + L"-" + std::to_wstring(numbers[1]) + L"-" + std::to_wstring(numbers[2]) + L"-" + std::to_wstring(numbers[3]);
        check(ConvertStringSidToSidW(private_sid.c_str(), &write_sid.value), "Create private write SID");
        SID_AND_ATTRIBUTES restrictions[] = {{write_sid.value, 0}, {logon, 0}};
        check(CreateRestrictedToken(original.value, DISABLE_MAX_PRIVILEGE | WRITE_RESTRICTED, 0, nullptr, 0, nullptr, 2, restrictions, &token.value), "Create write-restricted token");
        check(IsTokenRestricted(token.value), "Verify write-restricted token");
        sddl = L"D:(A;;GA;;;SY)(A;;GA;;;" + user_sid + L")(A;;GA;;;" + private_sid + L")(A;;GA;;;" + session_sid + L")";
        LocalMemory descriptor;
        check(ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.c_str(), 1, &descriptor.value, nullptr), "Create private object DACL");
        BOOL present = FALSE, defaulted = FALSE;
        TOKEN_DEFAULT_DACL dacl{};
        check(GetSecurityDescriptorDacl(descriptor.value, &present, &dacl.DefaultDacl, &defaulted) && present, "Read private object DACL");
        check(SetTokenInformation(token.value, TokenDefaultDacl, &dacl, sizeof(dacl)), "Apply private token DACL");
    }

    static void new_registry(const std::wstring& path, bool canary, bool& created) {
        HKEY key = nullptr;
        DWORD disposition = 0;
        auto result = RegCreateKeyExW(HKEY_CURRENT_USER, path.c_str(), 0, nullptr, 0, KEY_ALL_ACCESS, nullptr, &key, &disposition);
        check(result == ERROR_SUCCESS, "Create private registry key");
        if (disposition != REG_CREATED_NEW_KEY) { RegCloseKey(key); throw std::runtime_error("Private registry key already exists"); }
        created = true;
        if (canary) { DWORD value = 1; result = RegSetValueExW(key, L"unchanged", 0, REG_DWORD, reinterpret_cast<BYTE*>(&value), sizeof(value)); }
        RegCloseKey(key);
        check(result == ERROR_SUCCESS, "Create registry canary");
    }

    void initialise_broker() {
        SECURITY_ATTRIBUTES security{sizeof(security), nullptr, TRUE};
        mapping.reset(CreateFileMappingW(INVALID_HANDLE_VALUE, &security, PAGE_READWRITE, 0, 16384, nullptr));
        check(mapping.valid(), "Create broker mapping");
        view = static_cast<BYTE*>(MapViewOfFile(mapping.value, FILE_MAP_ALL_ACCESS, 0, 0, 16384));
        check(view != nullptr, "Map broker memory");
        request.reset(CreateEventW(&security, FALSE, FALSE, nullptr));
        response.reset(CreateEventW(&security, FALSE, FALSE, nullptr));
        mutex.reset(CreateMutexW(&security, FALSE, nullptr));
        check(request.valid() && response.valid() && mutex.valid(), "Create broker synchronization");
        reinterpret_cast<BrokerRequest*>(view)->magic = 0x31505351;
    }

    static bool pipe_name(const std::wstring& name) {
        if (name.size() <= 9 || name.size() > 1032 || _wcsnicmp(name.c_str(), L"\\\\.\\pipe\\", 9)) return false;
        auto path = name.substr(9);
        size_t start = 0;
        for (;;) {
            const auto end = path.find(L'\\', start);
            auto part = path.substr(start, end == std::wstring::npos ? end : end - start);
            if (part.empty() || part == L"." || part == L"..") return false;
            if (end == std::wstring::npos) return true;
            start = end + 1;
        }
    }

    void broker_loop() {
        while (!stopping) {
            if (WaitForSingleObject(request.value, 100) != WAIT_OBJECT_0 || stopping) continue;
            BrokerRequest input{};
            MemoryBarrier();
            memcpy(&input, view, sizeof(input));
            DWORD error = ERROR_INVALID_PARAMETER;
            HANDLE target = nullptr;
            UINT64 information = 0;
            try { answer(input, error, target, information); } catch (...) { error = ERROR_INVALID_PARAMETER; }
            memcpy(view + 16, &information, sizeof(information));
            memcpy(view + 40, &target, sizeof(target));
            memcpy(view + 36, &error, sizeof(error));
            InterlockedExchange(reinterpret_cast<volatile LONG*>(view + 4), 2);
            SetEvent(response.value);
        }
    }

    void answer(BrokerRequest& input, DWORD& error, HANDLE& target, UINT64& information) {
        check(input.magic == 0x31505351 && input.state == 1 && input.length > 0 && input.length <= 1023 && input.descriptor_bytes <= 8192, "Validate broker request");
        std::wstring name(input.name, input.length);
        check(name.find(L'\0') == std::wstring::npos, "Validate broker name");
        Handle actor;
        HANDLE receiver = process.value;
        if (input.caller != pid) {
            actor.reset(OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_DUP_HANDLE, FALSE, input.caller));
            BOOL inside = FALSE;
            check(actor.valid() && IsProcessInJob(actor.value, job.value, &inside) && inside, "Reject caller outside research job");
            receiver = actor.value;
        }
        Handle owned;
        LocalMemory generated;
        void* descriptor = input.descriptor_bytes ? input.descriptor : nullptr;
        if (input.descriptor_bytes) {
            check(input.descriptor_bytes >= SECURITY_DESCRIPTOR_MIN_LENGTH && IsValidSecurityDescriptor(descriptor), "Validate broker security descriptor");
            SECURITY_DESCRIPTOR_CONTROL control = 0; DWORD revision = 0;
            check(GetSecurityDescriptorControl(descriptor, &control, &revision) && (control & SE_SELF_RELATIVE), "Require self-relative broker descriptor");
            check(GetSecurityDescriptorLength(descriptor) <= input.descriptor_bytes, "Bound broker security descriptor");
        }
        if (input.operation == 1 || input.operation == 2) {
            check(pipe_name(name), "Validate local pipe path");
            auto private_pipe = L"\\\\.\\pipe\\" + pipe_prefix + name.substr(9);
            std::wstring identity = name;
            std::transform(identity.begin(), identity.end(), identity.begin(), towlower);
            if (input.operation == 1) {
                check(input.count > 0 && input.count <= 255 && input.output <= 4194304 && input.input <= 4194304 && pipes.size() < 1024, "Bound private pipe");
                if (!descriptor) {
                    check(ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.c_str(), 1, &generated.value, nullptr), "Create pipe DACL");
                    descriptor = generated.value;
                }
                SECURITY_ATTRIBUTES security{sizeof(security), descriptor, input.inherit != 0};
                owned.reset(CreateNamedPipeW(private_pipe.c_str(), input.access, input.mode, input.count, input.output, input.input, input.timeout, &security));
                error = GetLastError();
                if (owned.valid()) pipes.insert(identity);
            } else if (pipes.count(identity)) {
                owned.reset(CreateFileW(private_pipe.c_str(), input.access, input.mode, nullptr, OPEN_EXISTING, input.count, nullptr));
                error = GetLastError();
            } else { error = ERROR_NOT_SUPPORTED; }
        } else if (input.operation == 3) {
            check(descriptor && (input.access & DACL_SECURITY_INFORMATION) && !(input.access & ~0xA0000004u) && fs::path(name).is_absolute(), "Validate directory ACL request");
            owned.reset(CreateFileW(name.c_str(), READ_CONTROL | WRITE_DAC, 7, nullptr, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, nullptr));
            error = GetLastError();
            if (owned.valid()) {
                wchar_t resolved[4096]{};
                const DWORD count = GetFinalPathNameByHandleW(owned.value, resolved, 4096, 0);
                std::wstring path = resolved;
                if (path.rfind(L"\\\\?\\", 0) == 0) path.erase(0, 4);
                BY_HANDLE_FILE_INFORMATION info{};
                BOOL present = FALSE, defaulted = FALSE; PACL dacl = nullptr;
                if (!count || count >= 4096 || path.size() <= state_root.size() || _wcsnicmp(path.c_str(), (state_root + L"\\").c_str(), state_root.size() + 1) || !GetFileInformationByHandle(owned.value, &info) || (info.dwFileAttributes & 0x410) != 0x10) error = ERROR_ACCESS_DENIED;
                else if (!GetSecurityDescriptorDacl(descriptor, &present, &dacl, &defaulted) || !present) error = ERROR_INVALID_PARAMETER;
                else error = SetSecurityInfo(owned.value, SE_FILE_OBJECT, input.access, nullptr, nullptr, dacl, nullptr);
            }
        } else if (input.operation == 4) {
            check(_wcsicmp(name.c_str(), L"\\Device\\KsecDD") == 0 && input.access == 0x100003 && input.mode == 7 && input.count == 0x20 && !descriptor && !input.inherit, "Validate CNG capability");
            auto device = wide(name);
            UNICODE_STRING text{static_cast<USHORT>(name.size() * 2), static_cast<USHORT>(device.size() * 2), device.data()};
            OBJECT_ATTRIBUTES attributes{}; attributes.Length = sizeof(attributes); attributes.ObjectName = &text;
            IO_STATUS_BLOCK status{};
            using Open = NTSTATUS(NTAPI*)(PHANDLE, ACCESS_MASK, POBJECT_ATTRIBUTES, PIO_STATUS_BLOCK, ULONG, ULONG);
            auto open = reinterpret_cast<Open>(GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "NtOpenFile"));
            check(open != nullptr, "Resolve CNG device open");
            auto result = open(&owned.value, 0x100003, &attributes, &status, 7, 0x20);
            error = static_cast<DWORD>(result);
            if (result >= 0) information = status.Information; else owned.value = nullptr;
        } else if (input.operation == 5) {
            check(name == L"NUL" && input.access && !(input.access & ~0xC0000000u) && !(input.mode & ~7u) && !(input.count & ~0x40000080u) && !descriptor, "Validate null-device capability");
            owned.reset(CreateFileW(L"NUL", input.access, input.mode, nullptr, OPEN_EXISTING, input.count, nullptr));
            error = GetLastError();
        } else { throw std::runtime_error("Unsupported broker operation"); }
        if (input.operation != 3 && owned.valid()) {
            if (DuplicateHandle(GetCurrentProcess(), owned.value, receiver, &target, 0, input.inherit != 0, DUPLICATE_SAME_ACCESS)) error = ERROR_SUCCESS;
            else { error = GetLastError(); target = nullptr; }
        }
    }

    static std::wstring quote(const std::wstring& value) {
        if (value.find(L'"') != std::wstring::npos || (!value.empty() && value.back() == L'\\')) throw std::runtime_error("Invalid research argument");
        return L"\"" + value + L"\"";
    }

    void inject(const std::wstring& helper) {
        const auto bytes = (helper.size() + 1) * sizeof(wchar_t);
        auto remote = VirtualAllocEx(process.value, nullptr, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        check(remote != nullptr, "Allocate helper path");
        bool completed = false;
        try {
            SIZE_T written = 0;
            check(WriteProcessMemory(process.value, remote, helper.c_str(), bytes, &written) && written == bytes, "Write helper path");
            auto entry = reinterpret_cast<LPTHREAD_START_ROUTINE>(GetProcAddress(GetModuleHandleW(L"kernel32.dll"), "LoadLibraryW"));
            check(entry != nullptr, "Resolve helper loader");
            Handle loader(CreateRemoteThread(process.value, nullptr, 0, entry, remote, 0, nullptr));
            check(loader.valid() && WaitForSingleObject(loader.value, 10000) == WAIT_OBJECT_0, "Wait for owned helper");
            completed = true;
            check(GetExitCodeThread(loader.value, &helper_result), "Read helper status");
        } catch (...) { if (completed) VirtualFreeEx(process.value, remote, 0, MEM_RELEASE); throw; }
        VirtualFreeEx(process.value, remote, 0, MEM_RELEASE);
    }

public:
    DWORD pid = 0, helper_result = 0;
    bool self_test = false;

    ~Session() { cleanup(); }

    void start(const Options& options) {
        self_test = (options.flags & 1) != 0;
        state_root = fs::absolute(options.state).wstring();
        registry_root = options.registry;
        registry_canary = L"Software\\QScope\\PortableCanaries\\" + uuid();
        create_token();
        new_registry(registry_root, false, registry_created);
        new_registry(registry_canary, true, registry_canary_created);
        grant(L"CURRENT_USER\\" + registry_root, SE_REGISTRY_KEY, GRANT_ACCESS);
        std::vector<std::wstring> private_directories{state_root};
        if (!self_test) private_directories.push_back(fs::path(options.executable).parent_path().wstring());
        for (const auto& path : private_directories) {
            if (std::find(granted.begin(), granted.end(), path) == granted.end()) { grant(path, SE_FILE_OBJECT, GRANT_ACCESS); granted.push_back(path); }
        }
        canary = std::wstring(options.run) + L"\\outside-write-canary.txt";
        acl_canary = std::wstring(options.run) + L"\\outside-acl-canary";
        fs::create_directory(acl_canary);
        acl_before = directory_acl(acl_canary);
        { std::ofstream output(fs::path(canary), std::ios::binary); output << "QScope synthetic canary; do not alter."; }
        initialise_broker();
        SIZE_T attribute_bytes = 0;
        InitializeProcThreadAttributeList(nullptr, 1, 0, &attribute_bytes);
        std::vector<BYTE> attribute_storage(attribute_bytes);
        auto attributes = reinterpret_cast<LPPROC_THREAD_ATTRIBUTE_LIST>(attribute_storage.data());
        check(InitializeProcThreadAttributeList(attributes, 1, 0, &attribute_bytes), "Initialize handle allowlist");
        struct AttributeCleanup { LPPROC_THREAD_ATTRIBUTE_LIST value; ~AttributeCleanup() { DeleteProcThreadAttributeList(value); } } attribute_cleanup{attributes};
        // 显式提供 NUL 标准流，防止研究 QQ 保留发起 CLI 的输出管道。
        SECURITY_ATTRIBUTES standard_security{sizeof(SECURITY_ATTRIBUTES), nullptr, TRUE};
        Handle standard_input(CreateFileW(L"NUL", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, &standard_security, OPEN_EXISTING, 0, nullptr));
        Handle standard_output(CreateFileW(L"NUL", GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, &standard_security, OPEN_EXISTING, 0, nullptr));
        check(standard_input.valid() && standard_output.valid(), "Open isolated standard streams");
        HANDLE handles[] = {mapping.value, request.value, response.value, mutex.value, standard_input.value, standard_output.value};
        check(UpdateProcThreadAttribute(attributes, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, handles, sizeof(handles), nullptr, nullptr), "Apply inherited handle allowlist");
        struct CaseInsensitive { bool operator()(const std::wstring& a, const std::wstring& b) const { return _wcsicmp(a.c_str(), b.c_str()) < 0; } };
        std::map<std::wstring, std::wstring, CaseInsensitive> environment;
        for (const auto key : {L"PATH",L"SystemRoot",L"SystemDrive",L"WINDIR",L"COMSPEC",L"PATHEXT",L"NUMBER_OF_PROCESSORS",L"PROCESSOR_ARCHITECTURE",L"PROCESSOR_IDENTIFIER",L"PROCESSOR_LEVEL",L"PROCESSOR_REVISION",L"OS",L"ProgramFiles",L"ProgramFiles(x86)",L"ProgramW6432",L"CommonProgramFiles",L"CommonProgramFiles(x86)",L"CommonProgramW6432"}) {
            DWORD count = GetEnvironmentVariableW(key, nullptr, 0);
            if (count > 0 && count < 65536) { std::vector<wchar_t> value(count); GetEnvironmentVariableW(key, value.data(), count); environment[key] = value.data(); }
        }
        const auto ready = state_root + L"\\.qscope\\" + uuid() + L".ready";
        environment[L"USERPROFILE"] = state_root + L"\\user";
        environment[L"APPDATA"] = state_root + L"\\user\\AppData\\Roaming";
        environment[L"LOCALAPPDATA"] = state_root + L"\\user\\AppData\\Local";
        environment[L"PUBLIC"] = state_root + L"\\public";
        environment[L"PROGRAMDATA"] = state_root + L"\\programData";
        environment[L"TEMP"] = environment[L"TMP"] = state_root + L"\\temp";
        environment[L"QSCOPE_STATE_ROOT"] = state_root;
        environment[L"QSCOPE_ROOT_IMAGE"] = options.executable;
        environment[L"QSCOPE_CLIENT_BUILD"] = options.build;
        environment[L"QSCOPE_SEND_RVA"] = std::to_wstring(options.send_rva);
        environment[L"QSCOPE_RECV_RVA"] = std::to_wstring(options.recv_rva);
        environment[L"QSCOPE_READY_PATH"] = ready;
        environment[L"QSCOPE_REGISTRY_ROOT"] = registry_root;
        environment[L"QSCOPE_WRITE_SID"] = private_sid;
        environment[L"QSCOPE_SESSION_SID"] = session_sid;
        environment[L"QSCOPE_CURRENT_USER_SID"] = user_sid;
        environment[L"QSCOPE_BROKER_MAPPING"] = std::to_wstring(reinterpret_cast<UINT_PTR>(mapping.value));
        environment[L"QSCOPE_BROKER_REQUEST"] = std::to_wstring(reinterpret_cast<UINT_PTR>(request.value));
        environment[L"QSCOPE_BROKER_RESPONSE"] = std::to_wstring(reinterpret_cast<UINT_PTR>(response.value));
        environment[L"QSCOPE_BROKER_MUTEX"] = std::to_wstring(reinterpret_cast<UINT_PTR>(mutex.value));
        environment[L"QSCOPE_SELF_TEST"] = self_test ? L"1" : L"0";
        environment[L"QSCOPE_FILE_CANARY"] = canary;
        environment[L"QSCOPE_REGISTRY_CANARY"] = user_sid + L"\\" + registry_canary;
        environment[L"QSCOPE_ACL_CANARY"] = acl_canary;
        const auto outside_name = L"\\\\.\\pipe\\QScopeOutsideCanary." + uuid();
        environment[L"QSCOPE_PIPE_CANARY"] = outside_name;
        if (self_test) outside_pipe.reset(CreateNamedPipeW(outside_name.c_str(), PIPE_ACCESS_INBOUND, PIPE_TYPE_BYTE, 1, 0, 0, 0, nullptr));
        if (options.port) environment[L"QSCOPE_DEVTOOLS_PORT"] = std::to_wstring(options.port);
        if (options.flags & 2) environment[L"QSCOPE_STARTUP_DIAGNOSTICS"] = L"1";
        if (options.capture && *options.capture) environment[L"QSCOPE_CAPTURE_DIR"] = options.capture;
        std::vector<wchar_t> block;
        for (const auto& pair : environment) { auto entry = pair.first + L"=" + pair.second; block.insert(block.end(), entry.begin(), entry.end()); block.push_back(0); }
        block.push_back(0);
        auto command = quote(options.executable) + (self_test ? L" __guard" : L"");
        command += L" --user-data-dir=" + quote(state_root + L"\\electron") + L" --enable-logging=file --log-file=" + quote(state_root + L"\\.qscope\\chromium.log") + L" --no-stdio-init --disable-gpu --disable-breakpad --no-first-run";
        if (!self_test) command += L" --no-sandbox";
        auto writable = wide(command);
        STARTUPINFOEXW startup{}; startup.StartupInfo.cb = sizeof(startup); startup.StartupInfo.dwFlags = STARTF_USESHOWWINDOW | STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = standard_input.value;
        startup.StartupInfo.hStdOutput = standard_output.value;
        startup.StartupInfo.hStdError = standard_output.value;
        startup.StartupInfo.wShowWindow = self_test ? SW_HIDE : SW_SHOWNORMAL;
        startup.StartupInfo.lpDesktop = self_test ? nullptr : const_cast<wchar_t*>(L"winsta0\\default");
        startup.lpAttributeList = attributes;
        PROCESS_INFORMATION info{};
        check(CreateProcessAsUserW(token.value, options.executable, writable.data(), nullptr, nullptr, TRUE, EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | (self_test ? CREATE_NO_WINDOW : 0), block.data(), fs::path(options.executable).parent_path().c_str(), &startup.StartupInfo, &info), "Create isolated process");
        process.reset(info.hProcess); thread.reset(info.hThread); pid = info.dwProcessId;
        job.reset(CreateJobObjectW(nullptr, nullptr));
        check(job.valid(), "Create research job");
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        limits.BasicLimitInformation.ActiveProcessLimit = 32;
        check(SetInformationJobObject(job.value, JobObjectExtendedLimitInformation, &limits, sizeof(limits)) && AssignProcessToJobObject(job.value, process.value), "Assign research job");
        broker = std::thread([this] { broker_loop(); });
        Handle child_token;
        check(OpenProcessToken(process.value, TOKEN_QUERY, &child_token.value), "Read child token");
        DWORD container = 0, bytes = 0;
        check(GetTokenInformation(child_token.value, TokenIsAppContainer, &container, sizeof(container), &bytes) && !container && IsTokenRestricted(child_token.value), "Verify isolated child identity");
        inject(options.helper);
        const auto ready_path = ready + L".pid-" + std::to_wstring(pid);
        bool redirected = false;
        for (int attempt = 0; attempt < 50; attempt++) {
            if (fs::exists(ready_path)) { std::ifstream input(fs::path(ready_path), std::ios::binary); std::string text((std::istreambuf_iterator<char>(input)), {}); redirected = text.rfind("ready:", 0) == 0; break; }
            Sleep(100);
        }
        check(redirected, "Verify directory redirection before resume");
        if (!(options.flags & 4)) resume();
    }

    void resume() { check(ResumeThread(thread.value) != static_cast<DWORD>(-1), "Resume research process"); }
    bool running() const { return process.valid() && WaitForSingleObject(process.value, 0) == WAIT_TIMEOUT; }
    DWORD exit_code() const { DWORD value = 0; check(GetExitCodeProcess(process.value, &value), "Read process exit"); return value; }

    unsigned finish() {
        job.reset();
        if (process.valid()) WaitForSingleObject(process.value, 5000);
        unsigned result = 0;
        std::ifstream input(fs::path(canary), std::ios::binary);
        std::string contents((std::istreambuf_iterator<char>(input)), {});
        if (contents == "QScope synthetic canary; do not alter.") result |= 1;
        if (directory_acl(acl_canary) == acl_before) result |= 2;
        HKEY key = nullptr; DWORD value = 0, bytes = sizeof(value);
        if (RegOpenKeyExW(HKEY_CURRENT_USER, registry_canary.c_str(), 0, KEY_QUERY_VALUE, &key) == ERROR_SUCCESS) {
            if (RegQueryValueExW(key, L"unchanged", nullptr, nullptr, reinterpret_cast<BYTE*>(&value), &bytes) == ERROR_SUCCESS && value == 1) result |= 4;
            RegCloseKey(key);
        }
        cleanup();
        return result;
    }

    void cleanup() noexcept {
        if (cleaned) return;
        cleaned = true;
        if (process.valid() && !job.valid() && running()) TerminateProcess(process.value, 1);
        job.reset();
        stopping = true;
        if (request.valid()) SetEvent(request.value);
        if (broker.joinable()) broker.join();
        if (view) { UnmapViewOfFile(view); view = nullptr; }
        for (const auto& path : granted) { try { grant(path, SE_FILE_OBJECT, REVOKE_ACCESS); } catch (...) {} }
        if (registry_created) RegDeleteTreeW(HKEY_CURRENT_USER, registry_root.c_str());
        if (registry_canary_created) RegDeleteTreeW(HKEY_CURRENT_USER, registry_canary.c_str());
        if (!canary.empty()) DeleteFileW(canary.c_str());
    }
};

template<class Work> int protect(Work work) {
    try { work(); return 1; }
    catch (const std::exception& error) { const std::string message = error.what(); last_error.assign(message.begin(), message.end()); return 0; }
}
}

extern "C" const wchar_t* qscope_session_error() { return last_error.c_str(); }
extern "C" void* qscope_session_start(const Options* options) {
    auto session = std::make_unique<Session>();
    if (!protect([&] { session->start(*options); })) return nullptr;
    return session.release();
}
extern "C" unsigned qscope_session_pid(void* session) { return static_cast<Session*>(session)->pid; }
extern "C" int qscope_session_running(void* session) { return static_cast<Session*>(session)->running(); }
extern "C" unsigned qscope_session_exit(void* session) { unsigned value = 0xffffffff; protect([&] { value = static_cast<Session*>(session)->exit_code(); }); return value; }
extern "C" int qscope_session_resume(void* session) { return protect([&] { static_cast<Session*>(session)->resume(); }); }
extern "C" unsigned qscope_session_finish(void* value) {
    std::unique_ptr<Session> session(static_cast<Session*>(value));
    unsigned result = 0;
    protect([&] { result = session->finish(); });
    return result;
}
