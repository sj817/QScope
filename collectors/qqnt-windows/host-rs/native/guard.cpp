#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <shlobj.h>
#include <knownfolders.h>
#include <sddl.h>
#include <aclapi.h>
#include <msctf.h>
#include <string>
#include <cwchar>
#include <cstdio>
#include <vector>

std::wstring env(const wchar_t* name) { wchar_t value[4096]; const auto n=GetEnvironmentVariableW(name,value,4096); return n>0 && n<4096?std::wstring(value,n):L""; }
bool pipe_transfer(HANDLE pipe,bool write,char& value) {
    OVERLAPPED operation{}; operation.hEvent=CreateEventW(nullptr,TRUE,FALSE,nullptr);
    if(!operation.hEvent) return false;
    DWORD bytes=0;
    BOOL ok=write?WriteFile(pipe,&value,1,&bytes,&operation):ReadFile(pipe,&value,1,&bytes,&operation);
    if(!ok && GetLastError()==ERROR_IO_PENDING) {
        if(WaitForSingleObject(operation.hEvent,3000)!=WAIT_OBJECT_0) { CancelIoEx(pipe,&operation); GetOverlappedResult(pipe,&operation,&bytes,TRUE); }
        else ok=GetOverlappedResult(pipe,&operation,&bytes,FALSE);
    }
    CloseHandle(operation.hEvent);
    return ok && bytes==1;
}
extern "C" int qscope_guard(int argc,wchar_t** argv) {
    const bool child=argc>1 && wcscmp(argv[1],L"--qscope-child")==0;
    const auto root=env(L"QSCOPE_STATE_ROOT");
    const auto canary=env(L"QSCOPE_FILE_CANARY");
    const auto registry=env(L"QSCOPE_REGISTRY_CANARY");
    if(root.empty() || canary.empty() || registry.empty()) return 10;
    const bool standard_streams_isolated = GetFileType(GetStdHandle(STD_INPUT_HANDLE)) == FILE_TYPE_CHAR
        && GetFileType(GetStdHandle(STD_OUTPUT_HANDLE)) == FILE_TYPE_CHAR
        && GetFileType(GetStdHandle(STD_ERROR_HANDLE)) == FILE_TYPE_CHAR;
    bool folders=true;
    struct FolderCheck { const KNOWNFOLDERID* id; const wchar_t* suffix; };
    const FolderCheck folder_checks[]={{&FOLDERID_PublicDocuments,L"\\public\\Documents"},{&FOLDERID_Desktop,L"\\user\\Desktop"},{&FOLDERID_Downloads,L"\\user\\Downloads"},{&FOLDERID_Pictures,L"\\user\\Pictures"}};
    for(const auto& check:folder_checks) {
        PWSTR path=nullptr;
        const bool redirected=SUCCEEDED(SHGetKnownFolderPath(*check.id,0,nullptr,&path)) && path && _wcsicmp(path,(root+check.suffix).c_str())==0;
        folders=folders && redirected;
        if(redirected) {
            HANDLE probe=CreateFileW((std::wstring(path)+L"\\qscope-guard-"+std::to_wstring(GetCurrentProcessId())+L".tmp").c_str(),GENERIC_WRITE,0,nullptr,CREATE_NEW,FILE_ATTRIBUTE_TEMPORARY|FILE_FLAG_DELETE_ON_CLOSE,nullptr);
            folders=folders && probe!=INVALID_HANDLE_VALUE;
            if(probe!=INVALID_HANDLE_VALUE) CloseHandle(probe);
        }
        if(path) CoTaskMemFree(path);
    }
    HANDLE input=CreateFileW(canary.c_str(),GENERIC_READ,FILE_SHARE_READ|FILE_SHARE_WRITE,nullptr,OPEN_EXISTING,0,nullptr);
    const bool outside_read=input!=INVALID_HANDLE_VALUE;
    if(outside_read) CloseHandle(input);
    HANDLE outside=CreateFileW(canary.c_str(),GENERIC_WRITE,FILE_SHARE_READ,nullptr,OPEN_EXISTING,0,nullptr);
    const bool file_denied=outside==INVALID_HANDLE_VALUE && GetLastError()==ERROR_ACCESS_DENIED;
    if(outside!=INVALID_HANDLE_VALUE) CloseHandle(outside);
    HKEY key=nullptr;
    const auto code=RegOpenKeyExW(HKEY_USERS,registry.c_str(),0,KEY_SET_VALUE,&key);
    const bool registry_denied=code==ERROR_ACCESS_DENIED;
    if(key) RegCloseKey(key);
    HKEY mapped=nullptr;
    bool registry_inside=RegCreateKeyExW(HKEY_CURRENT_USER,L"QScopeGuardProbe",0,nullptr,0,KEY_SET_VALUE,nullptr,&mapped,nullptr)==ERROR_SUCCESS;
    if(registry_inside) { const DWORD marker=1; registry_inside=RegSetValueExW(mapped,L"marker",0,REG_DWORD,reinterpret_cast<const BYTE*>(&marker),sizeof(marker))==ERROR_SUCCESS; RegCloseKey(mapped); }
    const auto output=root+(child?L"\\guard-child-result.json":L"\\guard-result.json");
    HANDLE result=CreateFileW(output.c_str(),GENERIC_WRITE,FILE_SHARE_READ,nullptr,CREATE_NEW,0,nullptr);
    const bool inside=result!=INVALID_HANDLE_VALUE;
    const auto acl_path=root+(child?L"\\child-acl-probe":L"\\parent-acl-probe");
    HANDLE acl_file=CreateFileW(acl_path.c_str(),GENERIC_READ|GENERIC_WRITE|READ_CONTROL|WRITE_DAC,FILE_SHARE_READ|FILE_SHARE_WRITE,nullptr,CREATE_NEW,0,nullptr);
    bool inside_acl=false;
    DWORD acl_error=GetLastError();
    if(acl_file!=INVALID_HANDLE_VALUE) {
        PACL dacl=nullptr; PSECURITY_DESCRIPTOR sd=nullptr;
        acl_error=GetSecurityInfo(acl_file,SE_FILE_OBJECT,DACL_SECURITY_INFORMATION,nullptr,nullptr,&dacl,nullptr,&sd);
        if(acl_error==ERROR_SUCCESS) { acl_error=SetSecurityInfo(acl_file,SE_FILE_OBJECT,DACL_SECURITY_INFORMATION,nullptr,nullptr,dacl,nullptr); inside_acl=acl_error==ERROR_SUCCESS; }
        if(sd) LocalFree(sd); CloseHandle(acl_file);
    }
    auto acl_directory=acl_path+L"-dir";
    CreateDirectoryW(acl_directory.c_str(),nullptr);
    PACL named_dacl=nullptr; PSECURITY_DESCRIPTOR named_sd=nullptr;
    DWORD named_error=GetNamedSecurityInfoW(acl_directory.data(),SE_FILE_OBJECT,DACL_SECURITY_INFORMATION,nullptr,nullptr,&named_dacl,nullptr,&named_sd);
    bool named_acl_read=named_error==ERROR_SUCCESS;
    bool named_acl_write=false;
    if(named_acl_read) { named_error=SetNamedSecurityInfoW(acl_directory.data(),SE_FILE_OBJECT,DACL_SECURITY_INFORMATION,nullptr,nullptr,named_dacl,nullptr); named_acl_write=named_error==ERROR_SUCCESS; }
    if(named_sd) LocalFree(named_sd);
    const auto outside_acl_path=env(L"QSCOPE_ACL_CANARY");
    PSECURITY_DESCRIPTOR outside_sd=nullptr; PACL outside_dacl=nullptr;
    DWORD outside_acl_error=GetNamedSecurityInfoW(const_cast<wchar_t*>(outside_acl_path.c_str()),SE_FILE_OBJECT,DACL_SECURITY_INFORMATION,nullptr,nullptr,&outside_dacl,nullptr,&outside_sd);
    bool outside_acl_denied=false;
    if(outside_acl_error==ERROR_SUCCESS) { outside_acl_error=SetNamedSecurityInfoW(const_cast<wchar_t*>(outside_acl_path.c_str()),SE_FILE_OBJECT,DACL_SECURITY_INFORMATION,nullptr,nullptr,outside_dacl,nullptr); outside_acl_denied=outside_acl_error==ERROR_ACCESS_DENIED; }
    if(outside_sd) LocalFree(outside_sd);
    WSADATA winsock{};
    int network_error=WSAStartup(MAKEWORD(2,2),&winsock);
    if(network_error==0) {
        SOCKET endpoint=WSASocketW(AF_INET,SOCK_STREAM,IPPROTO_TCP,nullptr,0,WSA_FLAG_OVERLAPPED);
        if(endpoint==INVALID_SOCKET) network_error=WSAGetLastError();
        else {
            sockaddr_in address{}; address.sin_family=AF_INET; address.sin_addr.s_addr=htonl(INADDR_LOOPBACK);
            if(bind(endpoint,reinterpret_cast<sockaddr*>(&address),sizeof(address))!=0) network_error=WSAGetLastError();
            closesocket(endpoint);
        }
        WSACleanup();
    }
    HMODULE crypto=LoadLibraryW(L"bcrypt.dll");
    const DWORD crypto_error=crypto?0:GetLastError();
    using Random=LONG (WINAPI*)(void*,unsigned char*,ULONG,ULONG);
    auto random=crypto?reinterpret_cast<Random>(GetProcAddress(crypto,"BCryptGenRandom")):nullptr;
    unsigned char random_bytes[16]{};
    const LONG random_status=random?random(nullptr,random_bytes,sizeof(random_bytes),2):static_cast<LONG>(0xC0000135);
    HANDLE nul=CreateFileW(L"NUL",GENERIC_READ|GENERIC_WRITE,FILE_SHARE_READ|FILE_SHARE_WRITE,nullptr,OPEN_EXISTING,0,nullptr);
    const DWORD nul_error=nul!=INVALID_HANDLE_VALUE?0:GetLastError();
    if(nul!=INVALID_HANDLE_VALUE) CloseHandle(nul);
    const auto capability_path=root+(child?L"\\crypto-child.json":L"\\crypto-parent.json");
    HANDLE capability=CreateFileW(capability_path.c_str(),GENERIC_WRITE,FILE_SHARE_READ,nullptr,CREATE_NEW,0,nullptr);
    if(capability!=INVALID_HANDLE_VALUE) {
        char bytes[192]; const auto length=std::snprintf(bytes,sizeof(bytes),"{\"bcrypt_load_error\":%lu,\"random_status\":%ld,\"nul_open_error\":%lu}",crypto_error,random_status,nul_error);
        DWORD written;WriteFile(capability,bytes,length,&written,nullptr);CloseHandle(capability);
    }
    if(crypto) FreeLibrary(crypto);
    const HRESULT com_status=CoInitializeEx(nullptr,COINIT_APARTMENTTHREADED);
    IUnknown* text_services=nullptr;
    const HRESULT thread_manager=CoCreateInstance(CLSID_TF_ThreadMgr,nullptr,CLSCTX_INPROC_SERVER,IID_IUnknown,reinterpret_cast<void**>(&text_services));
    bool tsf_disabled=thread_manager==E_NOTIMPL && !text_services;
    if(text_services) text_services->Release();
    text_services=nullptr;
    const HRESULT profiles=CoCreateInstance(CLSID_TF_InputProcessorProfiles,nullptr,CLSCTX_INPROC_SERVER,IID_IUnknown,reinterpret_cast<void**>(&text_services));
    tsf_disabled=tsf_disabled && profiles==E_NOTIMPL && !text_services;
    if(text_services) text_services->Release();
    IGlobalInterfaceTable* global_table=nullptr;
    const bool ordinary_com=SUCCEEDED(CoCreateInstance(CLSID_StdGlobalInterfaceTable,nullptr,CLSCTX_INPROC_SERVER,IID_IGlobalInterfaceTable,reinterpret_cast<void**>(&global_table))) && global_table;
    if(global_table) global_table->Release();
    if(SUCCEEDED(com_status)) CoUninitialize();
    const auto pipe_name=L"\\\\.\\pipe\\QScopeGuard."+std::to_wstring(GetCurrentProcessId());
    const auto sddl=L"D:(A;;GA;;;"+env(L"QSCOPE_CURRENT_USER_SID")+L")";
    PSECURITY_DESCRIPTOR descriptor=nullptr;
    bool pipe_created=false,pipe_connected=false,ansi_pipe_created=false,ansi_pipe_connected=false,ansi_pipe_transfer=false;
    DWORD ansi_create_error=0,ansi_connect_error=0;
    if(ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.c_str(),SDDL_REVISION_1,&descriptor,nullptr)) {
        SECURITY_ATTRIBUTES security{sizeof(SECURITY_ATTRIBUTES),descriptor,FALSE};
        HANDLE server=CreateNamedPipeW(pipe_name.c_str(),PIPE_ACCESS_DUPLEX,PIPE_TYPE_BYTE|PIPE_NOWAIT,1,4096,4096,0,&security);
        pipe_created=server!=INVALID_HANDLE_VALUE;
        if(pipe_created) {
            ConnectNamedPipe(server,nullptr);
            HANDLE client=CreateFileW(pipe_name.c_str(),GENERIC_READ|GENERIC_WRITE,0,nullptr,OPEN_EXISTING,0,nullptr);
            pipe_connected=client!=INVALID_HANDLE_VALUE;
            if(pipe_connected) CloseHandle(client);
            CloseHandle(server);
        }
        const auto ansi_name="\\\\?\\pipe\\QScopeGuardAnsi."+std::to_string(GetCurrentProcessId());
        server=CreateNamedPipeA(ansi_name.c_str(),PIPE_ACCESS_DUPLEX|FILE_FLAG_OVERLAPPED|FILE_FLAG_FIRST_PIPE_INSTANCE,PIPE_TYPE_BYTE|PIPE_WAIT,1,4096,4096,0,&security);
        ansi_pipe_created=server!=INVALID_HANDLE_VALUE;
        ansi_create_error=ansi_pipe_created?0:GetLastError();
        if(ansi_pipe_created) {
            HANDLE client=CreateFileA(ansi_name.c_str(),GENERIC_READ|GENERIC_WRITE,0,nullptr,OPEN_EXISTING,FILE_FLAG_OVERLAPPED,nullptr);
            ansi_pipe_connected=client!=INVALID_HANDLE_VALUE;
            ansi_connect_error=ansi_pipe_connected?0:GetLastError();
            if(ansi_pipe_connected) {
                const bool connected=ConnectNamedPipe(server,nullptr) || GetLastError()==ERROR_PIPE_CONNECTED;
                char sent='Q',received=0;
                ansi_pipe_transfer=connected && pipe_transfer(client,true,sent) && pipe_transfer(server,false,received) && received==sent;
            }
            if(ansi_pipe_connected) CloseHandle(client);
            CloseHandle(server);
        }
        LocalFree(descriptor);
    }
    const auto outside_pipe=env(L"QSCOPE_PIPE_CANARY");
    HANDLE external=CreateFileW(outside_pipe.c_str(),GENERIC_READ|GENERIC_WRITE,0,nullptr,OPEN_EXISTING,0,nullptr);
    const bool external_pipe_denied=external==INVALID_HANDLE_VALUE && GetLastError()==ERROR_ACCESS_DENIED;
    if(external!=INVALID_HANDLE_VALUE) CloseHandle(external);
    const std::string outside_ansi(outside_pipe.begin(),outside_pipe.end());
    external=CreateFileA(outside_ansi.c_str(),GENERIC_READ|GENERIC_WRITE,0,nullptr,OPEN_EXISTING,0,nullptr);
    const bool external_ansi_denied=external==INVALID_HANDLE_VALUE && GetLastError()==ERROR_ACCESS_DENIED;
    if(external!=INVALID_HANDLE_VALUE) CloseHandle(external);
    bool child_ok=true;
    if(!child) {
        wchar_t executable[4096]; GetModuleFileNameW(nullptr,executable,4096);
        std::wstring command=L"\""+std::wstring(executable)+L"\" --qscope-child";
        SIZE_T size=0;
        InitializeProcThreadAttributeList(nullptr,1,0,&size);
        auto list=static_cast<LPPROC_THREAD_ATTRIBUTE_LIST>(HeapAlloc(GetProcessHeap(),0,size));
        SECURITY_ATTRIBUTES security{sizeof(SECURITY_ATTRIBUTES),nullptr,TRUE};
        HANDLE event=CreateEventW(&security,FALSE,FALSE,nullptr);
        STARTUPINFOEXW startup{}; startup.StartupInfo.cb=sizeof(startup); startup.lpAttributeList=list;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
        startup.StartupInfo.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
        startup.StartupInfo.hStdError = GetStdHandle(STD_ERROR_HANDLE);
        std::vector<HANDLE> inherited{event, startup.StartupInfo.hStdInput, startup.StartupInfo.hStdOutput};
        if(startup.StartupInfo.hStdError != startup.StartupInfo.hStdOutput) inherited.push_back(startup.StartupInfo.hStdError);
        PROCESS_INFORMATION info{};
        child_ok=list && event && InitializeProcThreadAttributeList(list,1,0,&size)
            && UpdateProcThreadAttribute(list,0,PROC_THREAD_ATTRIBUTE_HANDLE_LIST,inherited.data(),inherited.size()*sizeof(HANDLE),nullptr,nullptr)
            && CreateProcessW(executable,command.data(),nullptr,nullptr,TRUE,EXTENDED_STARTUPINFO_PRESENT|CREATE_NO_WINDOW,nullptr,nullptr,&startup.StartupInfo,&info);
        if(child_ok) {
            DWORD exit=1;
            child_ok=WaitForSingleObject(info.hProcess,60000)==WAIT_OBJECT_0 && GetExitCodeProcess(info.hProcess,&exit) && exit==0;
            if(!child_ok) TerminateProcess(info.hProcess,1);
            CloseHandle(info.hThread); CloseHandle(info.hProcess);
        }
        if(list) { DeleteProcThreadAttributeList(list); HeapFree(GetProcessHeap(),0,list); }
        if(event) CloseHandle(event);
    }
    const bool crypto_ok=crypto_error==0 && random_status>=0, nul_ok=nul_error==0, loopback_ok=network_error==0;
    char json[2048];
    const auto bytes=std::snprintf(json,sizeof(json),
        "{\"standard_streams_isolated\":%s,\"known_folders\":%s,\"outside_read\":%s,\"outside_file_write_denied\":%s,\"outside_registry_write_denied\":%s,\"private_registry_write\":%s,\"inside_file_write\":%s,\"inside_acl_write\":%s,\"acl_error\":%lu,\"inside_named_acl_write\":%s,\"named_acl_error\":%lu,\"outside_named_acl_denied\":%s,\"named_pipe_create\":%s,\"named_pipe_connect\":%s,\"outside_named_pipe_denied\":%s,\"restricted_child\":%s,\"registry_probe_code\":%lu,\"loopback_bind_error\":%d,\"system_crypto\":%s,\"null_device\":%s,\"loopback_bind\":%s,\"ansi_pipe_create\":%s,\"ansi_pipe_connect\":%s,\"ansi_pipe_transfer\":%s,\"ansi_pipe_create_error\":%lu,\"ansi_pipe_connect_error\":%lu,\"outside_ansi_pipe_denied\":%s,\"process_tsf_disabled\":%s,\"ordinary_com\":%s}",
        standard_streams_isolated?"true":"false",folders?"true":"false",outside_read?"true":"false",file_denied?"true":"false",registry_denied?"true":"false",registry_inside?"true":"false",inside?"true":"false",inside_acl?"true":"false",acl_error,named_acl_write?"true":"false",named_error,outside_acl_denied?"true":"false",pipe_created?"true":"false",pipe_connected?"true":"false",external_pipe_denied?"true":"false",child_ok?"true":"false",static_cast<unsigned long>(code),network_error,crypto_ok?"true":"false",nul_ok?"true":"false",loopback_ok?"true":"false",ansi_pipe_created?"true":"false",ansi_pipe_connected?"true":"false",ansi_pipe_transfer?"true":"false",ansi_create_error,ansi_connect_error,external_ansi_denied?"true":"false",tsf_disabled?"true":"false",ordinary_com?"true":"false");
    if(inside) { DWORD written; WriteFile(result,json,bytes,&written,nullptr); CloseHandle(result); }
    return standard_streams_isolated && folders && outside_read && file_denied && registry_denied && registry_inside && inside && inside_acl && named_acl_write && outside_acl_denied && pipe_created && pipe_connected && ansi_pipe_created && ansi_pipe_connected && ansi_pipe_transfer && external_pipe_denied && external_ansi_denied && child_ok && crypto_ok && nul_ok && loopback_ok && tsf_disabled && ordinary_com?0:1;
}
