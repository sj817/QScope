// End-to-end synthetic probe/trampoline -> native file -> Rust reader fixture.
#define QSCOPE_PROBE_SELFTEST 1
#include "probe.cpp"
#include <cstdlib>
#include <vector>
#include <thread>
namespace {
std::atomic<unsigned> native_send_calls{0}, native_receive_calls{0};
std::atomic<DWORD> send_input_error{0},receive_input_error{0};
__declspec(noinline) std::int64_t __fastcall fixture_send(void* self, void** owner) {
    send_input_error=GetLastError();SetLastError(0x9876);
    ++native_send_calls;
    return reinterpret_cast<std::uintptr_t>(self) + (owner ? 73 : 2);
}
__declspec(noinline) std::int64_t __fastcall fixture_receive(void* self, void** owner, std::uint32_t code) {
    receive_input_error=GetLastError();SetLastError(0x8765);
    ++native_receive_calls;
    return reinterpret_cast<std::uintptr_t>(self) + (owner ? code : 3);
}
void require_at(bool condition, int line) { if(!condition) { std::fprintf(stderr,"Probe self-test failed at line %d\n",line); ExitProcess(2); } }
#define require(condition) require_at((condition),__LINE__)
void set_pointer(unsigned char* target, std::size_t offset, const void* value) { std::memcpy(target+offset,&value,sizeof(value)); }
}
int wmain(int argc,wchar_t** argv) {
    require(argc==2 && SetEnvironmentVariableW(L"QSCOPE_CAPTURE_DIR",argv[1]));
    wchar_t image[4096];require(GetModuleFileNameW(nullptr,image,4096)>0 && SetEnvironmentVariableW(L"QSCOPE_ROOT_IMAGE",image));
    pause_writer.store(true);
    require(MH_Initialize()==MH_OK && qscope_capture_prepare());
    send_target=reinterpret_cast<void*>(fixture_send); recv_target=reinterpret_cast<void*>(fixture_receive);
    require(MH_CreateHook(send_target,reinterpret_cast<void*>(send_hook),reinterpret_cast<void**>(&original_send))==MH_OK);
    require(MH_CreateHook(recv_target,reinterpret_cast<void*>(receive_hook),reinterpret_cast<void**>(&original_receive))==MH_OK);
    require(MH_EnableHook(MH_ALL_HOOKS)==MH_OK);
    unsigned char request[64]{}, task[160]{}, response[200]{};
    const unsigned char bytes[]={0,1,0,3,0x11,0x22,0x33}; // TLV-like raw bytes, no PB assumption.
    std::uint64_t vector[3]={reinterpret_cast<std::uintptr_t>(bytes),reinterpret_cast<std::uintptr_t>(bytes+sizeof(bytes)),reinterpret_cast<std::uintptr_t>(bytes+sizeof(bytes))};
    request[0]=6; std::memcpy(request+1,"cmd",3);
    set_pointer(request,32,vector); set_pointer(task,0,request);
    // Fake account fields at the profile offsets. The writer emits one status per change.
    auto set_account=[&](const char* text,std::uint32_t type){const auto n=std::strlen(text);std::memset(task+qscope_profile::send_account_offset,0,24);
        if(n<=22){task[qscope_profile::send_account_offset]=static_cast<unsigned char>(n<<1);std::memcpy(task+qscope_profile::send_account_offset+1,text,n);}
        else{const std::uint64_t words[3]={(n+8)|1,n,reinterpret_cast<std::uintptr_t>(text)};std::memcpy(task+qscope_profile::send_account_offset,words,24);}
        std::memcpy(task+qscope_profile::send_account_type_offset,&type,4);};
    set_account("10001",1);
    const std::uint64_t seq=9007199254740993ull;
    std::memcpy(task+88,&seq,8); std::memcpy(response+24,&seq,8);
    response[32]=6; std::memcpy(response+33,"cmd",3); set_pointer(response,56,vector);
    void* task_owner=task; void* response_owner=response;
    Send volatile call_send=fixture_send; Receive volatile call_receive=fixture_receive;
    SetLastError(12345);
    require(call_send(reinterpret_cast<void*>(100),&task_owner)==173 && task_owner==task);
    require(send_input_error==12345 && GetLastError()==0x9876);
    SetLastError(54321);
    require(call_receive(reinterpret_cast<void*>(100),&response_owner,17)==117 && response_owner==response);
    require(receive_input_error==54321 && GetLastError()==0x8765);
    // Long libc++ strings are not the host compiler's std::string layout.
    const char long_cmd[]="fixture.long.command.with.more.than.twenty.two.bytes";
    std::uint64_t long_string[3]={65,sizeof(long_cmd)-1,reinterpret_cast<std::uintptr_t>(long_cmd)};
    std::memcpy(request,long_string,24);
    static const char long_account[]="u_fixture-account-long_1";set_account(long_account,2);
    require(call_send(reinterpret_cast<void*>(100),&task_owner)==173);
    require(call_send(reinterpret_cast<void*>(100),reinterpret_cast<void**>(1))==173);
    std::vector<unsigned char> large(payload_limit+1,0x5a);
    vector[0]=reinterpret_cast<std::uintptr_t>(large.data()); vector[1]=vector[2]=vector[0]+large.size();
    set_account("bad!",3);
    observe(&task_owner,true,0);
    for(unsigned i=0;i<queue_capacity+16;++i) observe(&task_owner,true,0);
    require(queue_full.load()>0);
    pause_writer.store(false,std::memory_order_release);SetEvent(wakeup);
    const auto deadline=GetTickCount64()+10000;
    while(consumed.load(std::memory_order_acquire)!=queue_tail.load(std::memory_order_relaxed)) {require(GetTickCount64()<deadline);Sleep(1);}
    vector[0]=reinterpret_cast<std::uintptr_t>(bytes);vector[1]=vector[2]=vector[0]+sizeof(bytes);
    std::vector<std::thread> producers;
    for(unsigned i=0;i<8;++i) producers.emplace_back([&,i]{
        unsigned char local_task[sizeof(task)];std::memcpy(local_task,task,sizeof(task));void* local_owner=local_task;
        for(unsigned n=0;n<1000;++n) {
            const std::uint64_t unique_sequence=seq+1+i*1000+n;std::memcpy(local_task+88,&unique_sequence,8);
            require(call_send(reinterpret_cast<void*>(100),&local_owner)==173 && local_owner==local_task);
        }
    });
    for(auto& producer:producers) producer.join();
    HANDLE stop=CreateFileW(stop_path.c_str(),GENERIC_WRITE,0,nullptr,CREATE_NEW,0,nullptr);
    require(stop!=INVALID_HANDLE_VALUE); CloseHandle(stop);
    require(WaitForSingleObject(completed,10000)==WAIT_OBJECT_0);
    require(native_send_calls==8003 && native_receive_calls==1 && dropped.load()>0 && invalid_layout.load()>0);
    WIN32_FIND_DATAW found{};HANDLE search=FindFirstFileW((directory+L"\\capture.*.qscap").c_str(),&found);require(search!=INVALID_HANDLE_VALUE);FindClose(search);
    HANDLE capture=CreateFileW((directory+L"\\"+found.cFileName).c_str(),GENERIC_READ,FILE_SHARE_READ,nullptr,OPEN_EXISTING,0,nullptr);require(capture!=INVALID_HANDLE_VALUE);
    std::vector<char> written(GetFileSize(capture,nullptr));DWORD read=0;require(ReadFile(capture,written.data(),static_cast<DWORD>(written.size()),&read,nullptr)&&read==written.size());CloseHandle(capture);
    const auto occurrences=[&](const std::string& text){std::size_t n=0;for(auto at=std::search(written.begin(),written.end(),text.begin(),text.end());at!=written.end();at=std::search(at+1,written.end(),text.begin(),text.end()))++n;return n;};
    require(occurrences("account_observed type=1 uin=10001")==1 && occurrences("account_observed type=2 uin=u_fixture-account-long_1")==1 && occurrences("account_observed")==2 && occurrences("bad!")==0);
    std::puts("Native forwarding, raw bytes, 64-bit sequence, libc++ strings, account status changes, invalid memory, truncation, bounded overflow, 8 concurrent producers and flush passed.");
    return 0;
}
