#include <cassert>
#include <cstdint>
#include <cstring>
#include <dlfcn.h>
#include <string>
#include <vector>
#include <unistd.h>
#include <cstdio>
#include <cstdlib>
#include <cerrno>
#include <fcntl.h>
#include <thread>

using Function = std::uint64_t(*)(void*,void**,std::uint64_t);
struct Request { std::string command; std::uint64_t padding = 0; std::vector<std::uint8_t>* payload; };
struct Send { Request* request; std::uint8_t padding[80]{}; std::uint64_t sequence = 123; };
struct Receive { std::uint8_t padding[24]{}; std::uint64_t sequence = 123; std::string command; std::vector<std::uint8_t>* payload; };
static_assert(sizeof(std::string)==24);
static_assert(offsetof(Request,payload)==32);
static_assert(offsetof(Send,sequence)==88);
static_assert(offsetof(Receive,command)==32);
static_assert(offsetof(Receive,payload)==56);
int main(int argc,char**argv) {
    assert(argc==2);
    auto* module=dlopen(argv[1],RTLD_NOW); assert(module);
    auto send=reinterpret_cast<Function>(dlsym(module,"qscope_test_send")); assert(send);
    auto receive=reinterpret_cast<Function>(dlsym(module,"qscope_test_receive")); assert(receive);
    std::vector<std::uint8_t> payload{8,1,18,0};
    Request request{"Synthetic.Send",0,&payload};
    Send outgoing{&request,{},123}; void* owner=&outgoing;
    errno=EDOM;
    auto result=send(reinterpret_cast<void*>(5),&owner,9);
    assert(result==reinterpret_cast<std::uintptr_t>(&owner)+14 && errno==EDOM);
    assert(owner==&outgoing);
    Receive incoming{{},123,"Synthetic.Receive.With.Long.Command",&payload}; owner=&incoming;
    result=receive(reinterpret_cast<void*>(5),&owner,9);
    assert(result==reinterpret_cast<std::uintptr_t>(&owner)+21);
    auto* bad=reinterpret_cast<void**>(1);
    assert(send(nullptr,bad,0)==1);
    payload.resize(1024*1024+1,0x42); owner=&outgoing; outgoing.sequence=124;
    send(nullptr,&owner,0);
    std::thread producers[4];
    for(auto& producer:producers) producer=std::thread([send] {
        std::vector<std::uint8_t> bytes{8,1};
        Request request{"Synthetic.Concurrent",0,&bytes};
        Send packet{&request,{},777}; void* owner=&packet;
        for(unsigned i=0;i<100;++i) {
            errno=E2BIG;
            assert(send(nullptr,&owner,0)==reinterpret_cast<std::uintptr_t>(&owner));
            assert(errno==E2BIG && owner==&packet);
        }
    });
    for(auto& producer:producers) producer.join();
    const char* stop=std::getenv("QSCOPE_PROBE_STOP");assert(stop);
    int file=open(stop,O_WRONLY|O_CREAT|O_EXCL,0600);assert(file>=0);close(file);
    for(int i=0;i<400 && access(std::getenv("QSCOPE_PROBE_CLOSED"),F_OK)!=0;++i)usleep(5000);
    assert(access(std::getenv("QSCOPE_PROBE_CLOSED"),F_OK)==0);
    puts("native instrumentation preserved return values, owner and errno");
}
