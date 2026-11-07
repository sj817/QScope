pub fn notify_environment_changed() {
    const ENVIRONMENT: &[u16] = &[69, 110, 118, 105, 114, 111, 110, 109, 101, 110, 116, 0];
    let mut result = 0;
    // WM_SETTINGCHANGE 将此固定字符串传递给用户 shell。
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SendMessageTimeoutW(
            windows_sys::Win32::UI::WindowsAndMessaging::HWND_BROADCAST,
            windows_sys::Win32::UI::WindowsAndMessaging::WM_SETTINGCHANGE,
            0,
            ENVIRONMENT.as_ptr() as isize,
            windows_sys::Win32::UI::WindowsAndMessaging::SMTO_ABORTIFHUNG,
            2000,
            &mut result,
        );
    }
}
use qscope_automation::Error;
use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::Path};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, FILETIME, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    },
    System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    },
};

/// 防止后台子进程保留调用方的标准流；重定向时由 Rust 单独复制所需句柄。
pub fn isolate_standard_handles() -> Result<(), Error> {
    use std::os::windows::io::AsRawHandle;
    for handle in [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ] {
        if !handle.is_null()
            && handle != INVALID_HANDLE_VALUE
            && unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0
        {
            return Err(Error::new(
                "standard_handles",
                std::io::Error::last_os_error().to_string(),
            ));
        }
    }
    Ok(())
}

#[repr(C)]
struct SessionOptions {
    executable: *const u16,
    state: *const u16,
    helper: *const u16,
    build: *const u16,
    registry: *const u16,
    run: *const u16,
    capture: *const u16,
    port: u32,
    flags: u32,
    send_rva: u32,
    recv_rva: u32,
}

unsafe extern "C" {
    fn qscope_session_error() -> *const u16;
    fn qscope_session_start(options: *const SessionOptions) -> *mut c_void;
    fn qscope_session_pid(session: *mut c_void) -> u32;
    fn qscope_session_running(session: *mut c_void) -> i32;
    fn qscope_session_exit(session: *mut c_void) -> u32;
    fn qscope_session_resume(session: *mut c_void) -> i32;
    fn qscope_session_finish(session: *mut c_void) -> u32;
    fn qscope_guard(count: i32, arguments: *mut *mut u16) -> i32;
}

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}
fn native_error() -> Error {
    // C++ 适配层拥有线程局部、以 NUL 结尾的错误缓冲区。
    unsafe {
        let message = qscope_session_error();
        let mut length = 0;
        while length < 4096 && *message.add(length) != 0 {
            length += 1;
        }
        Error::new(
            "qq_isolation",
            String::from_utf16_lossy(std::slice::from_raw_parts(message, length)),
        )
    }
}

pub struct ResearchOptions<'a> {
    pub executable: &'a Path,
    pub state: &'a Path,
    pub helper: &'a Path,
    pub build: &'a str,
    pub registry: &'a str,
    pub run: &'a Path,
    pub capture: Option<&'a Path>,
    pub port: u16,
    pub self_test: bool,
    pub diagnostics: bool,
    pub paused: bool,
    pub targets: [u32; 2],
}

pub struct Session(*mut c_void);

impl Session {
    pub fn start(options: ResearchOptions<'_>) -> Result<Self, Error> {
        let executable = wide(options.executable.as_os_str());
        let state = wide(options.state.as_os_str());
        let helper = wide(options.helper.as_os_str());
        let build = wide(std::ffi::OsStr::new(options.build));
        let registry = wide(std::ffi::OsStr::new(options.registry));
        let run = wide(options.run.as_os_str());
        let capture = options
            .capture
            .map(|path| wide(path.as_os_str()))
            .unwrap_or_else(|| vec![0]);
        let raw = SessionOptions {
            executable: executable.as_ptr(),
            state: state.as_ptr(),
            helper: helper.as_ptr(),
            build: build.as_ptr(),
            registry: registry.as_ptr(),
            run: run.as_ptr(),
            capture: capture.as_ptr(),
            port: u32::from(options.port),
            flags: u32::from(options.self_test)
                | (u32::from(options.diagnostics) << 1)
                | (u32::from(options.paused) << 2),
            send_rva: options.targets[0],
            recv_rva: options.targets[1],
        };
        let context = unsafe { qscope_session_start(&raw) };
        if context.is_null() {
            return Err(native_error());
        }
        Ok(Self(context))
    }
    pub fn pid(&self) -> u32 {
        unsafe { qscope_session_pid(self.0) }
    }
    pub fn running(&self) -> bool {
        unsafe { qscope_session_running(self.0) != 0 }
    }
    pub fn exit_code(&self) -> u32 {
        unsafe { qscope_session_exit(self.0) }
    }
    pub fn resume(&self) -> Result<(), Error> {
        if unsafe { qscope_session_resume(self.0) } == 0 {
            Err(native_error())
        } else {
            Ok(())
        }
    }
    pub fn finish(mut self) -> Result<(), Error> {
        let context = std::mem::replace(&mut self.0, std::ptr::null_mut());
        if unsafe { qscope_session_finish(context) } != 7 {
            return Err(Error::new("qq_guard", "An isolation canary changed"));
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                qscope_session_finish(self.0);
            }
        }
    }
}

pub fn guard() -> Result<i32, Error> {
    if std::env::var("QSCOPE_SELF_TEST").as_deref() != Ok("1") {
        return Err(Error::new(
            "qq_guard",
            "The guard requires an owned self-test session",
        ));
    }
    let mut arguments: Vec<Vec<u16>> = std::env::args_os()
        .map(|argument| wide(&argument))
        .collect();
    let mut pointers: Vec<_> = arguments
        .iter_mut()
        .map(|argument| argument.as_mut_ptr())
        .collect();
    Ok(unsafe { qscope_guard(pointers.len() as i32, pointers.as_mut_ptr()) })
}

pub fn identity(pid: u32) -> Result<(String, String), Error> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            let error = std::io::Error::last_os_error();
            return Err(if matches!(error.raw_os_error(), Some(87 | 1168)) {
                Error::new("process_missing", "The process has exited")
            } else {
                error.into()
            });
        }
        let result = (|| {
            let mut creation: FILETIME = std::mem::zeroed();
            let mut exit = creation;
            let mut kernel = creation;
            let mut user = creation;
            if GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            if exit.dwHighDateTime != 0 || exit.dwLowDateTime != 0 {
                return Err(Error::new("process_missing", "The process has exited"));
            }
            let mut path = [0u16; 32768];
            let mut length = path.len() as u32;
            if QueryFullProcessImageNameW(handle, 0, path.as_mut_ptr(), &mut length) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok((
                ((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
                    .to_string(),
                String::from_utf16_lossy(&path[..length as usize]),
            ))
        })();
        CloseHandle(handle);
        result
    }
}

pub fn stop(pid: u32, birth: &str) -> Result<(), Error> {
    use windows_sys::Win32::System::Threading::{
        PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };
    unsafe {
        let handle = OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | 0x00100000,
            0,
            pid,
        );
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let result = (|| -> Result<(), Error> {
            let mut creation: FILETIME = std::mem::zeroed();
            let mut exit = creation;
            let mut kernel = creation;
            let mut user = creation;
            if GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            if ((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
                .to_string()
                != birth
            {
                return Err(Error::new(
                    "process_identity",
                    "Worker identity changed before shutdown",
                ));
            }
            if TerminateProcess(handle, 1) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            if WaitForSingleObject(handle, 10000) != 0 {
                return Err(Error::new(
                    "process_timeout",
                    "Worker did not finish its shutdown",
                ));
            }
            Ok(())
        })();
        CloseHandle(handle);
        result?;
    }
    Ok(())
}

pub fn close_window(pid: u32, title: &str) -> Result<bool, Error> {
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM},
        UI::WindowsAndMessaging::{
            EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW,
            WM_CLOSE,
        },
    };
    struct Search {
        pid: u32,
        title: String,
        window: HWND,
    }
    unsafe extern "system" fn visit(window: HWND, context: LPARAM) -> i32 {
        // EnumWindows 在 Search 存活期间同步调用此回调。
        let search = unsafe { &mut *(context as *mut Search) };
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(window, &mut pid);
        }
        if pid != search.pid || unsafe { IsWindowVisible(window) } == 0 {
            return 1;
        }
        let mut text = [0u16; 1024];
        let count = unsafe { GetWindowTextW(window, text.as_mut_ptr(), text.len() as i32) };
        if String::from_utf16_lossy(&text[..count as usize]) == search.title {
            search.window = window;
            return 0;
        }
        1
    }
    let mut search = Search {
        pid,
        title: title.into(),
        window: std::ptr::null_mut(),
    };
    unsafe {
        EnumWindows(Some(visit), (&mut search as *mut Search) as LPARAM);
    }
    Ok(!search.window.is_null() && unsafe { PostMessageW(search.window, WM_CLOSE, 0, 0) } != 0)
}
