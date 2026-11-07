//! Darwin 平台边界负责精确进程身份及受控子进程信号。
use qscope_automation::Error;
use std::{
    os::{unix::ffi::OsStringExt, unix::process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

static STOP: AtomicBool = AtomicBool::new(false);
static STOP_FILE: OnceLock<PathBuf> = OnceLock::new();
const FILETIME_EPOCH: u64 = 116_444_736_000_000_000;

#[link(name = "sandbox")]
unsafe extern "C" {
    fn sandbox_check(
        pid: libc::pid_t,
        operation: *const libc::c_char,
        filter: libc::c_int,
        ...
    ) -> libc::c_int;
}

pub(crate) fn sandbox_denies_write(path: &Path) -> Result<bool, Error> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::new("sandbox", "Sandbox check path contains NUL"))?;
    // 安全约束。sandbox_check 的 PATH 筛选接收以 NUL 结尾的路径，NO_REPORT 避免生成诊断副作用。
    let result = unsafe {
        sandbox_check(
            libc::getpid(),
            c"file-write-data".as_ptr(),
            1 | 0x10000000,
            path.as_ptr(),
        )
    };
    if result < 0 {
        Err(Error::new(
            "sandbox",
            "Cannot query the inherited filesystem sandbox",
        ))
    } else {
        Ok(result != 0)
    }
}

pub fn identity(pid: u32) -> Result<(String, PathBuf), Error> {
    let pid = i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| Error::new("process_identity", "Invalid process PID"))?;
    // 安全约束。proc_bsdinfo 是 C 数据输出结构，调用时使用文档规定类型和精确缓冲大小，并校验完整返回。
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info);
    let count = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size as i32,
        )
    };
    if count != size as i32 {
        let error = std::io::Error::last_os_error();
        return Err(Error::new(
            if matches!(error.raw_os_error(), Some(libc::ESRCH | libc::ENOENT)) {
                "process_missing"
            } else {
                "process_identity"
            },
            format!("Cannot identify PID {pid}: {error}"),
        ));
    }
    if info.pbi_pid != pid as u32 || info.pbi_start_tvusec >= 1_000_000 {
        return Err(Error::new(
            "process_identity",
            "Invalid Darwin process start time",
        ));
    }
    let birth = info
        .pbi_start_tvsec
        .checked_mul(10_000_000)
        .and_then(|seconds| seconds.checked_add(info.pbi_start_tvusec * 10))
        .and_then(|ticks| ticks.checked_add(FILETIME_EPOCH))
        .ok_or_else(|| Error::new("process_identity", "Darwin process start time overflow"))?;
    let mut image = vec![0u8; 4096];
    // 安全约束。路径缓冲区在调用期间保持有效，构造路径前校验返回长度及结束符。
    let count = unsafe { libc::proc_pidpath(pid, image.as_mut_ptr().cast(), image.len() as u32) };
    if count <= 0 || count as usize >= image.len() {
        return Err(Error::new(
            "process_identity",
            "Cannot read the process image path",
        ));
    }
    let end = image
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(count as usize);
    image.truncate(end);
    let image = PathBuf::from(std::ffi::OsString::from_vec(image));
    if !image.is_absolute() {
        return Err(Error::new(
            "process_identity",
            "Process image path is not absolute",
        ));
    }
    Ok((birth.to_string(), image))
}

extern "C" fn request_stop(_signal: libc::c_int) {
    // 支持的 ARM64 目标上 AtomicBool 无锁，信号处理器仅操作原子状态。
    STOP.store(true, Ordering::Relaxed);
}

pub(crate) fn install_stop_handler() -> Result<(), Error> {
    STOP.store(false, Ordering::Relaxed);
    // 安全约束。按文档初始化 sigaction，指向静态且异步信号安全的处理器，作用范围限于当前采集器。
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = request_stop as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

pub(crate) fn stopping() -> bool {
    STOP.load(Ordering::Relaxed) || STOP_FILE.get().is_some_and(|path| path.is_file())
}

pub(crate) fn stop_file(path: PathBuf) {
    let _ = STOP_FILE.set(path);
}

pub(crate) fn signal(
    pid: u32,
    birth: &str,
    image: &Path,
    signal: i32,
    group: bool,
) -> Result<(), Error> {
    let (actual, executable) = identity(pid)?;
    if actual != birth || executable.canonicalize()? != image.canonicalize()? {
        return Err(Error::new(
            "process_identity",
            "Owned process identity changed",
        ));
    }
    let pid = pid as i32;
    // 安全约束。已核验 PID 和创建时间，仅对仍属自身且为进程组组长的子进程发送组信号。
    unsafe {
        if group && libc::getpgid(pid) != pid {
            return Err(Error::new(
                "process_identity",
                "Owned process group changed",
            ));
        }
        if libc::kill(if group { -pid } else { pid }, signal) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

pub(crate) fn spawn_detached(command: &mut Command) -> Result<Child, Error> {
    // 安全约束。setsid 满足异步信号安全要求，fork 与 exec 之间仅分离新采集器子进程。
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    Ok(command.spawn()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifies_only_the_synthetic_test_process_with_filetime_precision() {
        let (birth, image) = identity(std::process::id()).unwrap();
        assert!(birth.parse::<u64>().unwrap() > FILETIME_EPOCH);
        assert_eq!(
            image.canonicalize().unwrap(),
            std::env::current_exe().unwrap().canonicalize().unwrap()
        );
        assert!(identity(0).is_err());
        assert!(identity(u32::MAX).is_err());
        assert!(signal(std::process::id(), "1", &image, libc::SIGTERM, false).is_err());
    }
}
