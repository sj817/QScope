#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <winternl.h>
#include <shlobj.h>
#include <knownfolders.h>
#include <objbase.h>
#include <aclapi.h>
#include <sddl.h>
#include <imm.h>
#include <msctf.h>
#include <string>
#include <cstring>
#include <cwchar>
#include <vector>
#include <unordered_map>
#include <algorithm>
#include <cstdio>
#include "MinHook.h"
#include "probe.hpp"
#include "automation-script.hpp"

namespace {
std::wstring root;
using KnownFolder = HRESULT (WINAPI*)(REFKNOWNFOLDERID,DWORD,HANDLE,PWSTR*);
using FolderPath = HRESULT (WINAPI*)(HWND,int,HANDLE,DWORD,LPWSTR);
using SpecialFolder = BOOL (WINAPI*)(HWND,LPWSTR,int,BOOL);
KnownFolder original_known = nullptr;
FolderPath original_folder = nullptr;
SpecialFolder original_special = nullptr;
using NamedPipe = HANDLE (WINAPI*)(LPCWSTR,DWORD,DWORD,DWORD,DWORD,DWORD,DWORD,LPSECURITY_ATTRIBUTES);
NamedPipe original_pipe = nullptr;
PSID write_sid=nullptr, session_sid=nullptr;
BYTE* broker_view=nullptr;
HANDLE broker_mapping=nullptr,broker_request=nullptr,broker_response=nullptr,broker_mutex=nullptr;
bool broker_failed=false;
using FileOpen = HANDLE (WINAPI*)(LPCWSTR,DWORD,DWORD,LPSECURITY_ATTRIBUTES,DWORD,DWORD,HANDLE);
FileOpen original_file=nullptr;
using FileOpenA = HANDLE (WINAPI*)(LPCSTR,DWORD,DWORD,LPSECURITY_ATTRIBUTES,DWORD,DWORD,HANDLE);
FileOpenA original_file_a=nullptr;
using NativeFileOpen=NTSTATUS (NTAPI*)(PHANDLE,ACCESS_MASK,POBJECT_ATTRIBUTES,PIO_STATUS_BLOCK,ULONG,ULONG);
NativeFileOpen original_native_open=nullptr;
using ComCreate=HRESULT (WINAPI*)(REFCLSID,LPUNKNOWN,DWORD,REFIID,LPVOID*);
ComCreate original_com_create=nullptr;
HMODULE self_module=nullptr;
std::wstring helper_path,root_image,ready_path;
using ProcessCreate=BOOL (WINAPI*)(LPCWSTR,LPWSTR,LPSECURITY_ATTRIBUTES,LPSECURITY_ATTRIBUTES,BOOL,DWORD,LPVOID,LPCWSTR,LPSTARTUPINFOW,LPPROCESS_INFORMATION);
using AttributeUpdate=BOOL (WINAPI*)(LPPROC_THREAD_ATTRIBUTE_LIST,DWORD,DWORD_PTR,PVOID,SIZE_T,PVOID,PSIZE_T);
using AttributeDelete=VOID (WINAPI*)(LPPROC_THREAD_ATTRIBUTE_LIST);
ProcessCreate original_process=nullptr;
AttributeUpdate original_update=nullptr;
AttributeDelete original_delete=nullptr;
using NamedSecurity=DWORD (WINAPI*)(LPWSTR,SE_OBJECT_TYPE,SECURITY_INFORMATION,PSID,PSID,PACL,PACL);
NamedSecurity original_named_security=nullptr;
bool shutting_down=false;
HRESULT WINAPI com_create(REFCLSID clsid,LPUNKNOWN outer,DWORD context,REFIID iid,LPVOID* output) {
    // Chromium 在 ImmDisableIME 后仍会激活 TSF。研究副本缺少外部输入法 IPC 能力，通过已有 COM 失败路径保留基本键盘输入。
    if(!shutting_down && (IsEqualCLSID(clsid,CLSID_TF_ThreadMgr) || IsEqualCLSID(clsid,CLSID_TF_InputProcessorProfiles))) {
        if(!output) return E_POINTER;
        *output=nullptr;
        return E_NOTIMPL;
    }
    return original_com_create(clsid,outer,context,iid,output);
}
using LibraryW=HMODULE (WINAPI*)(LPCWSTR,HANDLE,DWORD);
using LibraryA=HMODULE (WINAPI*)(LPCSTR,HANDLE,DWORD);
LibraryW original_library_w=nullptr;
LibraryA original_library_a=nullptr;
bool napi_diagnostics_enabled=false;
bool napi_automation_enabled=false;
using NapiFunction=int (*)(void*,const char*,size_t,void*,void*,void**);
using NapiString=int (*)(void*,const char*,size_t,void**);
using NapiScript=int (*)(void*,void*,void**);
using NapiPending=int (*)(void*,bool*);
using NapiClear=int (*)(void*,void**);
NapiFunction original_napi_function=nullptr;
NapiString napi_string=nullptr;
NapiScript original_napi_script=nullptr;
NapiPending napi_pending=nullptr;
NapiClear napi_clear=nullptr;
volatile LONG napi_hook_started=0,napi_probe_started=0;
void diagnostic(const char* message) {
    const auto file_name=root+L"\\.qscope\\napi-diagnostic.txt";
    HANDLE file=CreateFileW(file_name.c_str(),FILE_APPEND_DATA,FILE_SHARE_READ,nullptr,OPEN_ALWAYS,0,nullptr);
    if(file!=INVALID_HANDLE_VALUE) { DWORD written; WriteFile(file,message,static_cast<DWORD>(std::strlen(message)),&written,nullptr); CloseHandle(file); }
}
void run_napi_source(void* env,const char* script,const char* label) {
    void* source=nullptr; void* ignored=nullptr;
    int outcome=napi_string(env,script,std::strlen(script),&source);
    if(outcome==0) outcome=original_napi_script(env,source,&ignored);
    char text[96]; std::snprintf(text,sizeof(text),"%s script status=%d\n",label,outcome); diagnostic(text);
    if(outcome!=0) { bool raised=false; if(napi_pending(env,&raised)==0 && raised) napi_clear(env,&ignored); }
}
int napi_function(void* env,const char* name,size_t length,void* callback,void* data,void** result) {
    const int status=original_napi_function(env,name,length,callback,data,result);
    bool pending=true;
    if(status==0 && napi_pending(env,&pending)==0 && !pending && InterlockedCompareExchange(&napi_probe_started,1,0)==0) {
        if(napi_diagnostics_enabled) {
        const char* script=R"JS((()=>{
          const r=typeof require==='function'?require:(process.mainModule&&process.mainModule.require.bind(process.mainModule));
          if(!r) throw new Error('QScope: require is unavailable in this context');
          const fs=r('fs');
          const log=process.env.QSCOPE_STATE_ROOT+'/.qscope/node-startup.log';
          let logged=0;const append=fs.appendFileSync.bind(fs);
          const write=x=>{try{const line=String(x).slice(0,8192)+'\n';if((logged+=line.length)<4194304)append(log,line)}catch{}};
          write('diagnostic pid='+process.pid+' main='+(process.mainModule&&process.mainModule.filename));
          const emit=process.emit;
          process.emit=function(event,...args){if(event==='unhandledRejection'||event==='uncaughtExceptionMonitor')write(event+': '+(args[0]&&args[0].stack||args[0]));return Reflect.apply(emit,this,[event,...args])};
          for(const name of ['error','warn']){const original=console[name];console[name]=function(...args){write('console.'+name+': '+args.map(x=>x&&x.stack||String(x)).join(' '));return Reflect.apply(original,this,args)}}
          const Module=r('module');const load=Module._load;
          Module._load=function(...args){try{return Reflect.apply(load,this,args)}catch(e){write('module '+args[0]+': '+e.stack);throw e}};
          r('timers').setTimeout(()=>{write('loaded app modules: '+JSON.stringify(Object.keys(Module._cache).filter(x=>!x.startsWith('node:'))));write('active handles: '+JSON.stringify(process._getActiveHandles().map(x=>x.constructor&&x.constructor.name)))},5000);
        })())JS";
            run_napi_source(env,script,"diagnostic");
        }
        if(napi_automation_enabled) {
            FILETIME created,exited,kernel,user;
            if(GetProcessTimes(GetCurrentProcess(),&created,&exited,&kernel,&user)) {
                ULARGE_INTEGER birth; birth.LowPart=created.dwLowDateTime; birth.HighPart=created.dwHighDateTime;
                const std::string script="globalThis.__qscopeProcessIdentity={pid:"+std::to_string(GetCurrentProcessId())+",birth_filetime:'"+std::to_string(birth.QuadPart)+"'};\n"+qscope_automation_script;
                run_napi_source(env,script.c_str(),"automation");
            }
        }
    }
    return status;
}
void install_napi_diagnostic(HMODULE module) {
    if(!module || module!=GetModuleHandleW(L"QQNT.dll") || InterlockedCompareExchange(&napi_hook_started,1,0)!=0) return;
    auto target=GetProcAddress(module,"napi_create_function");
    napi_string=reinterpret_cast<NapiString>(GetProcAddress(module,"napi_create_string_utf8"));
    original_napi_script=reinterpret_cast<NapiScript>(GetProcAddress(module,"napi_run_script"));
    napi_pending=reinterpret_cast<NapiPending>(GetProcAddress(module,"napi_is_exception_pending"));
    napi_clear=reinterpret_cast<NapiClear>(GetProcAddress(module,"napi_get_and_clear_last_exception"));
    if(!target || !napi_string || !original_napi_script || !napi_pending || !napi_clear) { diagnostic("N-API diagnostic exports missing\n"); return; }
    const auto created=MH_CreateHook(reinterpret_cast<void*>(target),reinterpret_cast<void*>(napi_function),reinterpret_cast<void**>(&original_napi_function));
    const auto enabled=created==MH_OK?MH_EnableHook(reinterpret_cast<void*>(target)):created;
    char text[96]; std::snprintf(text,sizeof(text),"N-API diagnostic hook create=%d enable=%d\n",created,enabled); diagnostic(text);
}
HMODULE WINAPI library_w(LPCWSTR name,HANDLE file,DWORD flags) {
    const auto module=original_library_w(name,file,flags); const auto error=GetLastError();
    if(!shutting_down) { if(napi_diagnostics_enabled || napi_automation_enabled) install_napi_diagnostic(module); qscope_capture_module_loaded(module); }
    SetLastError(error); return module;
}
HMODULE WINAPI library_a(LPCSTR name,HANDLE file,DWORD flags) {
    const auto module=original_library_a(name,file,flags); const auto error=GetLastError();
    if(!shutting_down) { if(napi_diagnostics_enabled || napi_automation_enabled) install_napi_diagnostic(module); qscope_capture_module_loaded(module); }
    SetLastError(error); return module;
}
SRWLOCK attribute_lock=SRWLOCK_INIT;
// 其他 DLL 的卸载回调可能继续调用钩子，表的生命周期延续至进程地址空间回收。
auto* inherited_lists=new std::unordered_map<PVOID,std::vector<HANDLE>>;

std::wstring env_string(const wchar_t* name) {
    wchar_t value[4096]; const DWORD n=GetEnvironmentVariableW(name,value,4096);
    return n>0 && n<4096?std::wstring(value,n):L"";
}
HANDLE broker_call(DWORD,LPCWSTR,DWORD,DWORD,DWORD,DWORD,DWORD,DWORD,LPSECURITY_ATTRIBUTES,ULONG_PTR* information=nullptr);
DWORD WINAPI named_security(LPWSTR name,SE_OBJECT_TYPE type,SECURITY_INFORMATION info,PSID owner,PSID group,PACL dacl,PACL sacl) {
    const auto supported=DACL_SECURITY_INFORMATION|PROTECTED_DACL_SECURITY_INFORMATION|UNPROTECTED_DACL_SECURITY_INFORMATION;
    if(!shutting_down && type==SE_FILE_OBJECT && (info & DACL_SECURITY_INFORMATION) && !(info & ~supported) && name) {
        SECURITY_DESCRIPTOR descriptor; InitializeSecurityDescriptor(&descriptor,SECURITY_DESCRIPTOR_REVISION);
        SetSecurityDescriptorDacl(&descriptor,TRUE,dacl,FALSE);
        SECURITY_ATTRIBUTES security{sizeof(SECURITY_ATTRIBUTES),&descriptor,FALSE};
        broker_call(3,name,info,0,0,0,0,0,&security);
        return GetLastError();
    }
    return original_named_security(name,type,info,owner,group,dacl,sacl);
}

HANDLE env_handle(const wchar_t* name) {
    wchar_t value[64]; const DWORD n=GetEnvironmentVariableW(name,value,64);
    return n>0 && n<64?reinterpret_cast<HANDLE>(static_cast<ULONG_PTR>(_wcstoui64(value,nullptr,10))):nullptr;
}
HANDLE broker_call(DWORD operation,LPCWSTR name,DWORD a,DWORD b,DWORD c,DWORD output,DWORD input,DWORD timeout,LPSECURITY_ATTRIBUTES security,ULONG_PTR* information) {
    const auto length=name?wcsnlen_s(name,1033):0;
    std::wstring canonical;
    if((operation==1 || operation==2) && length>=9 && length<=1023 && _wcsnicmp(name,L"\\\\?\\pipe\\",9)==0) {
        canonical.assign(name,length); canonical[2]=L'.'; name=canonical.c_str();
    }
    if(!length || length>1023 || ((operation==1 || operation==2) && _wcsnicmp(name,L"\\\\.\\pipe\\",9)!=0)) { SetLastError(ERROR_INVALID_NAME); return INVALID_HANDLE_VALUE; }
    std::vector<BYTE> descriptor;
    if(security && security->lpSecurityDescriptor) {
        SECURITY_DESCRIPTOR_CONTROL control; DWORD revision;
        if(!GetSecurityDescriptorControl(security->lpSecurityDescriptor,&control,&revision)) return INVALID_HANDLE_VALUE;
        if(control & SE_SELF_RELATIVE) {
            const auto bytes=GetSecurityDescriptorLength(security->lpSecurityDescriptor);
            if(!bytes || bytes>8192) { SetLastError(ERROR_INVALID_PARAMETER); return INVALID_HANDLE_VALUE; }
            descriptor.resize(bytes); std::memcpy(descriptor.data(),security->lpSecurityDescriptor,bytes);
        } else {
            DWORD bytes=0; MakeSelfRelativeSD(security->lpSecurityDescriptor,nullptr,&bytes);
            if(!bytes || bytes>8192) { SetLastError(ERROR_INVALID_PARAMETER); return INVALID_HANDLE_VALUE; }
            descriptor.resize(bytes);
            if(!MakeSelfRelativeSD(security->lpSecurityDescriptor,descriptor.data(),&bytes)) return INVALID_HANDLE_VALUE;
        }
    }
    const DWORD acquired=WaitForSingleObject(broker_mutex,5000);
    if(acquired!=WAIT_OBJECT_0) { if(acquired==WAIT_ABANDONED) { InterlockedExchange(reinterpret_cast<volatile LONG*>(broker_view+16380),1); ReleaseMutex(broker_mutex); } SetLastError(ERROR_TIMEOUT); return INVALID_HANDLE_VALUE; }
    HANDLE handle=INVALID_HANDLE_VALUE; DWORD error=ERROR_BROKEN_PIPE;
    if(!broker_failed && broker_view && *reinterpret_cast<DWORD*>(broker_view)==0x31505351 && *reinterpret_cast<DWORD*>(broker_view+16380)==0 && *reinterpret_cast<DWORD*>(broker_view+4)==0) {
        auto put=[&](int offset,DWORD value) { *reinterpret_cast<DWORD*>(broker_view+offset)=value; };
        put(8,a); put(12,b); put(16,c); put(20,output); put(24,input); put(28,timeout);
        put(32,static_cast<DWORD>(length)); put(36,0); put(48,static_cast<DWORD>(descriptor.size())); put(52,GetCurrentProcessId()); put(56,security && security->bInheritHandle?1:0); put(60,operation);
        std::memcpy(broker_view+64,name,length*sizeof(wchar_t));
        if(!descriptor.empty()) std::memcpy(broker_view+2112,descriptor.data(),descriptor.size());
        InterlockedExchange(reinterpret_cast<volatile LONG*>(broker_view+4),1);
        SetEvent(broker_request);
        if(WaitForSingleObject(broker_response,5000)==WAIT_OBJECT_0 && *reinterpret_cast<DWORD*>(broker_view+4)==2) {
            MemoryBarrier();
            error=*reinterpret_cast<DWORD*>(broker_view+36);
            auto raw=*reinterpret_cast<ULONG_PTR*>(broker_view+40);
            if(raw) handle=reinterpret_cast<HANDLE>(raw);
            if(information) *information=*reinterpret_cast<ULONG_PTR*>(broker_view+16);
            InterlockedExchange(reinterpret_cast<volatile LONG*>(broker_view+4),0);
        } else { broker_failed=true; InterlockedExchange(reinterpret_cast<volatile LONG*>(broker_view+16380),1); error=ERROR_TIMEOUT; }
    }
    ReleaseMutex(broker_mutex);
    SetLastError(error);
    return handle;
}

BOOL WINAPI update_attribute(LPPROC_THREAD_ATTRIBUTE_LIST list,DWORD flags,DWORD_PTR attribute,PVOID value,SIZE_T bytes,PVOID previous,PSIZE_T returned) {
    if(shutting_down || attribute!=PROC_THREAD_ATTRIBUTE_HANDLE_LIST || !value || bytes%sizeof(HANDLE)!=0 || bytes>65536)
        return original_update(list,flags,attribute,value,bytes,previous,returned);
    AcquireSRWLockExclusive(&attribute_lock);
    auto& handles=(*inherited_lists)[list];
    handles.assign(static_cast<HANDLE*>(value),static_cast<HANDLE*>(value)+bytes/sizeof(HANDLE));
    for(auto handle:{broker_mapping,broker_request,broker_response,broker_mutex})
        if(handle && std::find(handles.begin(),handles.end(),handle)==handles.end()) handles.push_back(handle);
    const BOOL result=original_update(list,flags,attribute,handles.data(),handles.size()*sizeof(HANDLE),previous,returned);
    const DWORD error=GetLastError();
    if(!result) inherited_lists->erase(list);
    ReleaseSRWLockExclusive(&attribute_lock);
    SetLastError(error);
    return result;
}
VOID WINAPI delete_attributes(LPPROC_THREAD_ATTRIBUTE_LIST list) {
    original_delete(list);
    if(shutting_down) return;
    AcquireSRWLockExclusive(&attribute_lock);
    inherited_lists->erase(list);
    ReleaseSRWLockExclusive(&attribute_lock);
}
bool inject_child(HANDLE process,DWORD pid) {
    USHORT machine=0,native=0;
    if(!IsWow64Process2(process,&machine,&native) || machine!=IMAGE_FILE_MACHINE_UNKNOWN) { SetLastError(ERROR_EXE_MACHINE_TYPE_MISMATCH); return false; }
    const SIZE_T bytes=(helper_path.size()+1)*sizeof(wchar_t);
    auto remote=VirtualAllocEx(process,nullptr,bytes,MEM_RESERVE|MEM_COMMIT,PAGE_READWRITE);
    if(!remote) return false;
    SIZE_T written=0;
    HANDLE worker=nullptr;
    bool complete=false;
    DWORD loaded=0;
    if(WriteProcessMemory(process,remote,helper_path.c_str(),bytes,&written) && written==bytes) {
        const auto load=reinterpret_cast<LPTHREAD_START_ROUTINE>(GetProcAddress(GetModuleHandleW(L"kernel32.dll"),"LoadLibraryW"));
        worker=CreateRemoteThread(process,nullptr,0,load,remote,0,nullptr);
        if(worker) { complete=WaitForSingleObject(worker,10000)==WAIT_OBJECT_0; if(complete) GetExitCodeThread(worker,&loaded); CloseHandle(worker); }
    }
    if(complete) VirtualFreeEx(process,remote,0,MEM_RELEASE);
    if(!complete || !loaded) { SetLastError(ERROR_DLL_INIT_FAILED); return false; }
    const auto path=ready_path+L".pid-"+std::to_wstring(pid);
    for(int attempt=0;attempt<100;attempt++) {
        HANDLE file=original_file(path.c_str(),GENERIC_READ,FILE_SHARE_READ|FILE_SHARE_WRITE,nullptr,OPEN_EXISTING,0,nullptr);
        if(file!=INVALID_HANDLE_VALUE) {
            char message[64]{}; DWORD read=0;
            const bool ready=ReadFile(file,message,sizeof(message)-1,&read,nullptr) && std::strncmp(message,"ready:",6)==0;
            CloseHandle(file); return ready;
        }
        if(WaitForSingleObject(process,0)==WAIT_OBJECT_0) return false;
        Sleep(100);
    }
    return false;
}
void spawn_report(DWORD pid,DWORD error,const char* stage) {
    const auto path=ready_path+L".spawn-"+std::to_wstring(pid)+L".txt";
    HANDLE file=original_file(path.c_str(),GENERIC_WRITE,FILE_SHARE_READ,nullptr,CREATE_ALWAYS,0,nullptr);
    if(file==INVALID_HANDLE_VALUE) return;
    char text[256]; const int length=std::snprintf(text,sizeof(text),"stage=%s error=%lu caller=%lu child=%lu",stage,error,GetCurrentProcessId(),pid);
    DWORD written; WriteFile(file,text,length,&written,nullptr); CloseHandle(file);
}
BOOL WINAPI create_process(LPCWSTR application,LPWSTR command,LPSECURITY_ATTRIBUTES process_security,LPSECURITY_ATTRIBUTES thread_security,BOOL inherit,DWORD flags,LPVOID environment,LPCWSTR directory,LPSTARTUPINFOW startup,LPPROCESS_INFORMATION info) {
    if(shutting_down) return original_process(application,command,process_security,thread_security,inherit,flags,environment,directory,startup,info);
    std::wstring image=application?application:L"";
    if(image.empty() && command) {
        const auto count=wcsnlen_s(command,32768);
        if(count<32768) {
            std::wstring text(command,count);
            if(!text.empty() && text[0]==L'"') { const auto end=text.find(L'"',1); if(end!=std::wstring::npos) image=text.substr(1,end-1); }
            else image=text.substr(0,text.find(L' '));
        }
    }
    const bool same_image=_wcsicmp(image.c_str(),root_image.c_str())==0;
    const bool chromium_child=command && wcsstr(command,L"--type=");
    const auto runtime=root_image.substr(0,root_image.find_last_of(L"\\/"));
    const auto crashpad=runtime+L"\\versions\\"+env_string(L"QSCOPE_CLIENT_BUILD")+L"\\resources\\app\\crashpad_handler.exe";
    const auto crashpad64=runtime+L"\\versions\\"+env_string(L"QSCOPE_CLIENT_BUILD")+L"\\resources\\app\\win64\\crashpad_handler.exe";
    const bool bundled_handler=_wcsicmp(image.c_str(),crashpad.c_str())==0 || _wcsicmp(image.c_str(),crashpad64.c_str())==0;
    const bool self_test=command && (wcsstr(command,L"--qscope-child") || wcsstr(command,L"--qscope-ime")) && env_string(L"QSCOPE_SELF_TEST")==L"1";
    if(!bundled_handler && (!same_image || (!chromium_child && !self_test)))
        return original_process(application,command,process_security,thread_security,inherit,flags,environment,directory,startup,info);
    if(!inherit) { spawn_report(0,ERROR_NOT_SUPPORTED,"no-handle-inheritance"); SetLastError(ERROR_NOT_SUPPORTED); return FALSE; }
    if(environment) {
        if(!(flags & CREATE_UNICODE_ENVIRONMENT)) { spawn_report(0,ERROR_NOT_SUPPORTED,"non-unicode-environment"); SetLastError(ERROR_NOT_SUPPORTED); return FALSE; }
        bool found=false;
        const auto* block=static_cast<const wchar_t*>(environment);
        for(size_t offset=0;offset<65536 && block[offset];) {
            const auto count=wcsnlen_s(block+offset,65536-offset);
            if(count>=65536-offset) break;
            if(_wcsnicmp(block+offset,L"QSCOPE_STATE_ROOT=",18)==0 && root==block+offset+18) found=true;
            offset+=count+1;
        }
        if(!found) { spawn_report(0,ERROR_NOT_SUPPORTED,"missing-private-environment"); SetLastError(ERROR_NOT_SUPPORTED); return FALSE; }
    }
    if(!original_process(application,command,process_security,thread_security,inherit,flags|CREATE_SUSPENDED,environment,directory,startup,info)) { const auto error=GetLastError(); spawn_report(0,error,"create-process"); SetLastError(error); return FALSE; }
    if(!inject_child(info->hProcess,info->dwProcessId)) {
        spawn_report(info->dwProcessId,GetLastError(),"helper-not-ready");
        TerminateProcess(info->hProcess,ERROR_DLL_INIT_FAILED);
        WaitForSingleObject(info->hProcess,5000);
        CloseHandle(info->hThread); CloseHandle(info->hProcess); *info={};
        SetLastError(ERROR_DLL_INIT_FAILED); return FALSE;
    }
    spawn_report(info->dwProcessId,0,"ready");
    if(!(flags & CREATE_SUSPENDED) && ResumeThread(info->hThread)==static_cast<DWORD>(-1)) {
        const DWORD error=GetLastError();
        TerminateProcess(info->hProcess,error); CloseHandle(info->hThread); CloseHandle(info->hProcess); *info={};
        SetLastError(error); return FALSE;
    }
    return TRUE;
}
HANDLE WINAPI file_open(LPCWSTR name,DWORD access,DWORD share,LPSECURITY_ATTRIBUTES security,DWORD disposition,DWORD flags,HANDLE file_template) {
    if(shutting_down) return original_file(name,access,share,security,disposition,flags,file_template);
    if(write_sid && name && (_wcsnicmp(name,L"\\\\.\\pipe\\",9)==0 || _wcsnicmp(name,L"\\\\?\\pipe\\",9)==0) && disposition==OPEN_EXISTING) {
        HANDLE handle=broker_call(2,name,access,share,flags,0,0,0,security);
        if(handle==INVALID_HANDLE_VALUE && GetLastError()==ERROR_NOT_SUPPORTED) SetLastError(ERROR_ACCESS_DENIED);
        return handle;
    }
    if(write_sid && name && (_wcsicmp(name,L"NUL")==0 || _wcsicmp(name,L"\\\\.\\NUL")==0) && disposition==OPEN_EXISTING && !file_template)
        return broker_call(5,L"NUL",access,share,flags,0,0,0,security);
    return original_file(name,access,share,security,disposition,flags,file_template);
}

HANDLE WINAPI file_open_a(LPCSTR name,DWORD access,DWORD share,LPSECURITY_ATTRIBUTES security,DWORD disposition,DWORD flags,HANDLE file_template) {
    if(!shutting_down && write_sid && name && (_strnicmp(name,"\\\\.\\pipe\\",9)==0 || _strnicmp(name,"\\\\?\\pipe\\",9)==0 || _stricmp(name,"NUL")==0 || _stricmp(name,"\\\\.\\NUL")==0)) {
        const auto length=strnlen_s(name,1033);
        if(length==1033) { SetLastError(ERROR_INVALID_NAME); return INVALID_HANDLE_VALUE; }
        wchar_t wide[1033];
        if(!MultiByteToWideChar(AreFileApisANSI()?CP_ACP:CP_OEMCP,0,name,static_cast<int>(length+1),wide,1033)) return INVALID_HANDLE_VALUE;
        return file_open(wide,access,share,security,disposition,flags,file_template);
    }
    return original_file_a(name,access,share,security,disposition,flags,file_template);
}

NTSTATUS NTAPI native_file_open(PHANDLE output,ACCESS_MASK access,POBJECT_ATTRIBUTES attributes,PIO_STATUS_BLOCK status,ULONG share,ULONG options) {
    const NTSTATUS direct=original_native_open(output,access,attributes,status,share,options);
    constexpr wchar_t device[]=L"\\Device\\KsecDD";
    if(shutting_down || !write_sid || direct!=static_cast<NTSTATUS>(0xC0000022) || !output || !status || !attributes || attributes->Length!=sizeof(OBJECT_ATTRIBUTES) || attributes->RootDirectory || attributes->SecurityDescriptor || attributes->SecurityQualityOfService || attributes->Attributes!=0 || !attributes->ObjectName || !attributes->ObjectName->Buffer || attributes->ObjectName->Length!=sizeof(device)-sizeof(wchar_t) || _wcsnicmp(attributes->ObjectName->Buffer,device,(sizeof(device)/sizeof(wchar_t))-1)!=0 || access!=0x100003 || share!=7 || options!=0x20) return direct;
    const DWORD last_error=GetLastError();
    ULONG_PTR information=0;
    HANDLE handle=broker_call(4,device,access,share,options,0,0,0,nullptr,&information);
    const DWORD error=GetLastError();
    NTSTATUS result=0;
    if(handle!=INVALID_HANDLE_VALUE) { *output=handle; status->Information=information; }
    else { result=(error & 0x80000000)?static_cast<NTSTATUS>(error):static_cast<NTSTATUS>(error==ERROR_INVALID_PARAMETER?0xC000000D:0xC0000022); status->Information=0; }
    status->Status=result;
    SetLastError(last_error);
    return result;
}

HANDLE WINAPI named_pipe(LPCWSTR name,DWORD mode,DWORD type,DWORD count,DWORD output,DWORD input,DWORD timeout,LPSECURITY_ATTRIBUTES security) {
    if(shutting_down || !write_sid) return original_pipe(name,mode,type,count,output,input,timeout,security);
    if(!security || !security->lpSecurityDescriptor) return broker_call(1,name,mode,type,count,output,input,timeout,security);
    PACL existing=nullptr,combined=nullptr;
    BOOL present=FALSE,defaulted=FALSE;
    if(!GetSecurityDescriptorDacl(security->lpSecurityDescriptor,&present,&existing,&defaulted)) return INVALID_HANDLE_VALUE;
    if(!present || !existing) return broker_call(1,name,mode,type,count,output,input,timeout,security);
    EXPLICIT_ACCESSW grants[2]{};
    const PSID identities[]={write_sid,session_sid};
    for(int i=0;i<2;i++) {
        grants[i].grfAccessPermissions=GENERIC_ALL;
        grants[i].grfAccessMode=GRANT_ACCESS;
        grants[i].grfInheritance=NO_INHERITANCE;
        grants[i].Trustee.TrusteeForm=TRUSTEE_IS_SID;
        grants[i].Trustee.TrusteeType=TRUSTEE_IS_UNKNOWN;
        grants[i].Trustee.ptstrName=static_cast<LPWSTR>(identities[i]);
    }
    const DWORD result=SetEntriesInAclW(session_sid?2:1,grants,existing,&combined);
    if(result!=ERROR_SUCCESS) { SetLastError(result); return INVALID_HANDLE_VALUE; }
    SECURITY_DESCRIPTOR descriptor;
    InitializeSecurityDescriptor(&descriptor,SECURITY_DESCRIPTOR_REVISION);
    SetSecurityDescriptorDacl(&descriptor,TRUE,combined,defaulted);
    PSID owner=nullptr,group=nullptr; BOOL inherited=FALSE;
    if(GetSecurityDescriptorOwner(security->lpSecurityDescriptor,&owner,&inherited)) SetSecurityDescriptorOwner(&descriptor,owner,inherited);
    if(GetSecurityDescriptorGroup(security->lpSecurityDescriptor,&group,&inherited)) SetSecurityDescriptorGroup(&descriptor,group,inherited);
    PACL sacl=nullptr; BOOL sacl_present=FALSE;
    if(GetSecurityDescriptorSacl(security->lpSecurityDescriptor,&sacl_present,&sacl,&inherited) && sacl_present) SetSecurityDescriptorSacl(&descriptor,TRUE,sacl,inherited);
    SECURITY_DESCRIPTOR_CONTROL control; DWORD revision;
    if(GetSecurityDescriptorControl(security->lpSecurityDescriptor,&control,&revision)) {
        const SECURITY_DESCRIPTOR_CONTROL mask=SE_DACL_PROTECTED|SE_SACL_PROTECTED|SE_DACL_AUTO_INHERIT_REQ|SE_SACL_AUTO_INHERIT_REQ;
        SetSecurityDescriptorControl(&descriptor,mask,control & mask);
    }
    SECURITY_ATTRIBUTES adjusted=*security;
    adjusted.lpSecurityDescriptor=&descriptor;
    HANDLE handle=broker_call(1,name,mode,type,count,output,input,timeout,&adjusted);
    const auto error=GetLastError();
    LocalFree(combined);
    SetLastError(error);
    return handle;
}

std::wstring redirected(REFKNOWNFOLDERID id) {
    if(IsEqualGUID(id,FOLDERID_Profile)) return root+L"\\user";
    if(IsEqualGUID(id,FOLDERID_Documents)) return root+L"\\user\\Documents";
    if(IsEqualGUID(id,FOLDERID_Desktop)) return root+L"\\user\\Desktop";
    if(IsEqualGUID(id,FOLDERID_Downloads)) return root+L"\\user\\Downloads";
    if(IsEqualGUID(id,FOLDERID_Pictures)) return root+L"\\user\\Pictures";
    if(IsEqualGUID(id,FOLDERID_Music)) return root+L"\\user\\Music";
    if(IsEqualGUID(id,FOLDERID_Videos)) return root+L"\\user\\Videos";
    if(IsEqualGUID(id,FOLDERID_Public)) return root+L"\\public";
    if(IsEqualGUID(id,FOLDERID_PublicDocuments)) return root+L"\\public\\Documents";
    if(IsEqualGUID(id,FOLDERID_PublicDesktop)) return root+L"\\public\\Desktop";
    if(IsEqualGUID(id,FOLDERID_PublicDownloads)) return root+L"\\public\\Downloads";
    if(IsEqualGUID(id,FOLDERID_PublicPictures)) return root+L"\\public\\Pictures";
    if(IsEqualGUID(id,FOLDERID_PublicMusic)) return root+L"\\public\\Music";
    if(IsEqualGUID(id,FOLDERID_PublicVideos)) return root+L"\\public\\Videos";
    if(IsEqualGUID(id,FOLDERID_RoamingAppData)) return root+L"\\user\\AppData\\Roaming";
    if(IsEqualGUID(id,FOLDERID_LocalAppData)) return root+L"\\user\\AppData\\Local";
    if(IsEqualGUID(id,FOLDERID_LocalAppDataLow)) return root+L"\\user\\AppData\\LocalLow";
    if(IsEqualGUID(id,FOLDERID_ProgramData)) return root+L"\\programData";
    return {};
}
std::wstring redirected(int id) {
    switch(id & 0xff) {
    case CSIDL_PROFILE: return root+L"\\user";
    case CSIDL_PERSONAL: return root+L"\\user\\Documents";
    case CSIDL_DESKTOP: case CSIDL_DESKTOPDIRECTORY: return root+L"\\user\\Desktop";
    case CSIDL_MYPICTURES: return root+L"\\user\\Pictures";
    case CSIDL_MYMUSIC: return root+L"\\user\\Music";
    case CSIDL_MYVIDEO: return root+L"\\user\\Videos";
    case CSIDL_COMMON_DOCUMENTS: return root+L"\\public\\Documents";
    case CSIDL_COMMON_DESKTOPDIRECTORY: return root+L"\\public\\Desktop";
    case CSIDL_COMMON_PICTURES: return root+L"\\public\\Pictures";
    case CSIDL_COMMON_MUSIC: return root+L"\\public\\Music";
    case CSIDL_COMMON_VIDEO: return root+L"\\public\\Videos";
    case CSIDL_APPDATA: return root+L"\\user\\AppData\\Roaming";
    case CSIDL_LOCAL_APPDATA: return root+L"\\user\\AppData\\Local";
    case CSIDL_COMMON_APPDATA: return root+L"\\programData";
    default: return {};
    }
}
HRESULT WINAPI known_folder(REFKNOWNFOLDERID id,DWORD flags,HANDLE token,PWSTR* output) {
    if(shutting_down) return original_known(id,flags,token,output);
    const auto value=redirected(id);
    if(value.empty()) return original_known(id,flags,token,output);
    if(!output) return E_POINTER;
    *output=static_cast<PWSTR>(CoTaskMemAlloc((value.size()+1)*sizeof(wchar_t)));
    if(!*output) return E_OUTOFMEMORY;
    std::memcpy(*output,value.c_str(),(value.size()+1)*sizeof(wchar_t));
    return S_OK;
}
HRESULT WINAPI folder_path(HWND window,int id,HANDLE token,DWORD flags,LPWSTR output) {
    if(shutting_down) return original_folder(window,id,token,flags,output);
    const auto value=redirected(id);
    if(value.empty()) return original_folder(window,id,token,flags,output);
    if(!output) return E_POINTER;
    if(value.size()>=MAX_PATH) return HRESULT_FROM_WIN32(ERROR_FILENAME_EXCED_RANGE);
    std::memcpy(output,value.c_str(),(value.size()+1)*sizeof(wchar_t));
    return S_OK;
}
BOOL WINAPI special_folder(HWND window,LPWSTR output,int id,BOOL create) {
    if(shutting_down) return original_special(window,output,id,create);
    const auto value=redirected(id);
    if(value.empty()) return original_special(window,output,id,create);
    if(!output || value.size()>=MAX_PATH) { SetLastError(ERROR_FILENAME_EXCED_RANGE); return FALSE; }
    std::memcpy(output,value.c_str(),(value.size()+1)*sizeof(wchar_t));
    return TRUE;
}
void report(const char* message) {
    auto path=(ready_path.empty()?root+L"\\directory-hook.ready":ready_path)+L".pid-"+std::to_wstring(GetCurrentProcessId());
    HANDLE file=CreateFileW(path.c_str(),GENERIC_WRITE,FILE_SHARE_READ,nullptr,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,nullptr);
    if(file==INVALID_HANDLE_VALUE) return;
    DWORD written=0;
    WriteFile(file,message,static_cast<DWORD>(std::strlen(message)),&written,nullptr);
    CloseHandle(file);
}
DWORD WINAPI initialise(void*) {
    wchar_t buffer[4096];
    DWORD length=GetEnvironmentVariableW(L"QSCOPE_STATE_ROOT",buffer,4096);
    if(!length || length>=4096) return 1;
    root.assign(buffer,length);
    ready_path=env_string(L"QSCOPE_READY_PATH");
    root_image=env_string(L"QSCOPE_ROOT_IMAGE");
    length=GetModuleFileNameW(self_module,buffer,4096);
    if(!length || length>=4096) { report("failed: helper path"); return 12; }
    helper_path.assign(buffer,length);
    // 受限研究进程禁用文本输入服务，用户的系统输入法设置独立保存。
    if(!ImmDisableIME(static_cast<DWORD>(-1))) { report("failed: process-local IME compatibility"); return 14; }
    wchar_t identity[512];
    DWORD identity_length=GetEnvironmentVariableW(L"QSCOPE_WRITE_SID",identity,512);
    if(identity_length>0 && identity_length<512) {
        if(!ConvertStringSidToSidW(identity,&write_sid)) { report("failed: write SID"); return 7; }
        identity_length=GetEnvironmentVariableW(L"QSCOPE_SESSION_SID",identity,512);
        if(identity_length>0 && identity_length<512 && !ConvertStringSidToSidW(identity,&session_sid)) { report("failed: session SID"); return 8; }
        broker_mapping=env_handle(L"QSCOPE_BROKER_MAPPING");
        broker_request=env_handle(L"QSCOPE_BROKER_REQUEST"); broker_response=env_handle(L"QSCOPE_BROKER_RESPONSE");
        broker_mutex=env_handle(L"QSCOPE_BROKER_MUTEX");
        broker_view=static_cast<BYTE*>(MapViewOfFile(broker_mapping,FILE_MAP_ALL_ACCESS,0,0,16384));
        if(!broker_view || !broker_request || !broker_response || !broker_mutex || *reinterpret_cast<DWORD*>(broker_view)!=0x31505351) { report("failed: scoped pipe broker"); return 10; }
    }
    wchar_t registry[1024];
    const DWORD registry_length=GetEnvironmentVariableW(L"QSCOPE_REGISTRY_ROOT",registry,1024);
    if(registry_length>0) {
        HKEY isolated=nullptr;
        if(registry_length>=1024 || RegOpenKeyExW(HKEY_CURRENT_USER,registry,0,KEY_ALL_ACCESS,&isolated)!=ERROR_SUCCESS
            || RegOverridePredefKey(HKEY_CURRENT_USER,isolated)!=ERROR_SUCCESS) {
            report("failed: private HKCU mapping"); return 6;
        }
        // 私有根目录句柄需要贯穿受控进程的整个生命周期。
    }
    if(!LoadLibraryW(L"shell32.dll")) { report("failed: shell32 unavailable"); return 5; }
    if(MH_Initialize()!=MH_OK) { report("failed: MH_Initialize"); return 2; }
    const auto a=MH_CreateHookApi(L"shell32.dll","SHGetKnownFolderPath",reinterpret_cast<void*>(known_folder),reinterpret_cast<void**>(&original_known));
    const auto b=MH_CreateHookApi(L"shell32.dll","SHGetFolderPathW",reinterpret_cast<void*>(folder_path),reinterpret_cast<void**>(&original_folder));
    const auto c=MH_CreateHookApi(L"shell32.dll","SHGetSpecialFolderPathW",reinterpret_cast<void*>(special_folder),reinterpret_cast<void**>(&original_special));
    if(a!=MH_OK || b!=MH_OK || c!=MH_OK) { report("failed: MH_CreateHookApi"); return 3; }
    // Kernel32 的 ANSI 封装直接调用 KernelBase，绕过 Kernel32 的宽字符入口。补齐这些入口，避免 libuv 创建子进程时反复重试被拒绝的管道而阻塞主线程。
    if(write_sid && MH_CreateHookApi(L"kernelbase.dll","CreateNamedPipeW",reinterpret_cast<void*>(named_pipe),reinterpret_cast<void**>(&original_pipe))!=MH_OK) { report("failed: private IPC hook"); return 9; }
    if(write_sid && MH_CreateHookApi(L"kernelbase.dll","CreateFileW",reinterpret_cast<void*>(file_open),reinterpret_cast<void**>(&original_file))!=MH_OK) { report("failed: private pipe client hook"); return 11; }
    if(write_sid && MH_CreateHookApi(L"kernel32.dll","CreateFileA",reinterpret_cast<void*>(file_open_a),reinterpret_cast<void**>(&original_file_a))!=MH_OK) { report("failed: ANSI pipe client compatibility"); return 18; }
    if(write_sid && MH_CreateHookApi(L"ntdll.dll","NtOpenFile",reinterpret_cast<void*>(native_file_open),reinterpret_cast<void**>(&original_native_open))!=MH_OK) { report("failed: CNG device compatibility"); return 17; }
    if(MH_CreateHookApi(L"combase.dll","CoCreateInstance",reinterpret_cast<void*>(com_create),reinterpret_cast<void**>(&original_com_create))!=MH_OK) { report("failed: process-local TSF compatibility"); return 19; }
    if(write_sid && (
        MH_CreateHookApi(L"kernel32.dll","UpdateProcThreadAttribute",reinterpret_cast<void*>(update_attribute),reinterpret_cast<void**>(&original_update))!=MH_OK ||
        MH_CreateHookApi(L"kernel32.dll","DeleteProcThreadAttributeList",reinterpret_cast<void*>(delete_attributes),reinterpret_cast<void**>(&original_delete))!=MH_OK ||
        MH_CreateHookApi(L"kernel32.dll","CreateProcessW",reinterpret_cast<void*>(create_process),reinterpret_cast<void**>(&original_process))!=MH_OK)) { report("failed: owned child bootstrap hooks"); return 13; }
    if(write_sid && MH_CreateHookApi(L"advapi32.dll","SetNamedSecurityInfoW",reinterpret_cast<void*>(named_security),reinterpret_cast<void**>(&original_named_security))!=MH_OK) { report("failed: private directory ACL compatibility"); return 15; }
    if(!qscope_capture_prepare()) { report("failed: raw capture preparation"); return 20; }
    napi_diagnostics_enabled=env_string(L"QSCOPE_STARTUP_DIAGNOSTICS")==L"1" && !wcsstr(GetCommandLineW(),L"--type=");
    napi_automation_enabled=!env_string(L"QSCOPE_DEVTOOLS_PORT").empty() && !wcsstr(GetCommandLineW(),L"--type=");
    if(napi_diagnostics_enabled || napi_automation_enabled || qscope_capture_enabled()) {
        if(MH_CreateHookApi(L"kernel32.dll","LoadLibraryExW",reinterpret_cast<void*>(library_w),reinterpret_cast<void**>(&original_library_w))!=MH_OK ||
           MH_CreateHookApi(L"kernel32.dll","LoadLibraryExA",reinterpret_cast<void*>(library_a),reinterpret_cast<void**>(&original_library_a))!=MH_OK) { report("failed: optional N-API diagnostics"); return 16; }
    }
    if(MH_EnableHook(MH_ALL_HOOKS)!=MH_OK) { report("failed: MH_EnableHook"); return 4; }
    qscope_capture_module_loaded(GetModuleHandleW(L"wrapper.node"));
    report(qscope_capture_enabled()?"ready: known folders redirected; raw capture armed, protocol hooks await wrapper module":"ready: known folders redirected; no QQ protocol hooks installed");
    return 0;
}
}
BOOL WINAPI DllMain(HINSTANCE module,DWORD reason,LPVOID) {
    if(reason==DLL_PROCESS_ATTACH) {
        self_module=module;
        DisableThreadLibraryCalls(module);
        HANDLE worker=CreateThread(nullptr,0,initialise,nullptr,0,nullptr);
        if(!worker) return FALSE;
        CloseHandle(worker);
    }
    if(reason==DLL_PROCESS_DETACH) shutting_down=true;
    return TRUE;
}
