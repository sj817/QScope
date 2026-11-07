//! Small Darwin boundary for exact identities and signals to owned children.
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
    // SAFETY: sandbox_check's PATH filter takes one NUL-terminated path.
    // NO_REPORT avoids diagnostic side effects; this never opens the path.
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
    // SAFETY: proc_bsdinfo is a C plain-data output structure; its documented
    // flavor and exact buffer size are passed, and only a complete result is read.
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
    // SAFETY: the writable path buffer remains allocated for the call; the
    // returned length and terminator are validated before constructing a path.
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
    // AtomicBool is lock-free on the supported ARM64 target; no allocation or
    // filesystem operation occurs in the signal handler.
    STOP.store(true, Ordering::Relaxed);
}

pub(crate) fn install_stop_handler() -> Result<(), Error> {
    STOP.store(false, Ordering::Relaxed);
    // SAFETY: sigaction is initialized as documented and points at a static
    // async-signal-safe handler. Only this collector process is affected.
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
    // SAFETY: PID and start time have just been validated. A group signal is
    // allowed only for our still-owned child that is its own group leader.
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
    // SAFETY: setsid is async-signal-safe and the closure touches no Rust state
    // between fork and exec. This only detaches the new collector child.
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
