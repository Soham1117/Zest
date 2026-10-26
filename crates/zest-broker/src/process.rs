//! Tool process isolation: no console window, below-normal priority, Windows
//! Job Object with kill-on-close and a memory limit. Cancel = terminate the job.

use crate::adapter::CommandSpec;
use std::ffi::c_void;
use std::io::{self, BufRead, BufReader, Read};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
};

pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
/// Bytes of stderr kept for the error message.
pub const STDERR_TAIL_BYTES: usize = 4096;
/// Default memory limit per tool process.
pub const DEFAULT_MEMORY_LIMIT: u64 = 4 * 1024 * 1024 * 1024;

fn win_err(e: windows::core::Error) -> io::Error {
    io::Error::other(e.to_string())
}

/// A Windows Job Object. Closing the handle kills all processes in it.
pub struct JobObject {
    handle: HANDLE,
}

// SAFETY: a job handle can be used from any thread.
unsafe impl Send for JobObject {}
unsafe impl Sync for JobObject {}

impl JobObject {
    pub fn new(memory_limit: Option<u64>) -> io::Result<JobObject> {
        // SAFETY: plain Win32 calls with valid arguments; the handle is closed in Drop.
        unsafe {
            let handle = CreateJobObjectW(None, windows::core::PCWSTR::null()).map_err(win_err)?;
            let job = JobObject { handle };
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if let Some(bytes) = memory_limit {
                info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
                info.ProcessMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
            }
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(win_err)?;
            Ok(job)
        }
    }

    pub fn assign(&self, child: &Child) -> io::Result<()> {
        let process = HANDLE(child.as_raw_handle());
        // SAFETY: both handles are valid for the duration of the call.
        unsafe { AssignProcessToJobObject(self.handle, process).map_err(win_err) }
    }

    pub fn terminate(&self) {
        // SAFETY: valid job handle. Errors are ignored: the job may be empty already.
        unsafe {
            let _ = TerminateJobObject(self.handle, 1);
        }
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        // SAFETY: handle was created by CreateJobObjectW and is closed once.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RunLimits {
    pub memory_bytes: Option<u64>,
}

impl Default for RunLimits {
    fn default() -> Self {
        RunLimits {
            memory_bytes: Some(DEFAULT_MEMORY_LIMIT),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    /// Last 4 KB of stderr.
    pub stderr_tail: String,
}

fn command_for(spec: &CommandSpec) -> Command {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    cmd
}

/// Run one tool process to the end.
///
/// `on_stdout` gets every stdout line (from a reader thread). `on_tick` runs on the calling
/// thread about every 20 ms while the process runs. If `cancel` becomes true, the whole
/// job object is terminated.
pub fn run_tool(
    spec: &CommandSpec,
    limits: &RunLimits,
    cancel: &AtomicBool,
    on_stdout: &(dyn Fn(&str) + Sync),
    on_tick: &mut dyn FnMut(),
) -> io::Result<RunOutcome> {
    if cancel.load(Ordering::SeqCst) {
        return Ok(RunOutcome {
            exit_code: None,
            cancelled: true,
            stderr_tail: String::new(),
        });
    }
    let mut cmd = command_for(spec);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let job = match JobObject::new(limits.memory_bytes) {
        Ok(j) => match j.assign(&child) {
            Ok(()) => Some(j),
            Err(_) => None, // already in a job that forbids nesting: fall back to kill()
        },
        Err(_) => None,
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let mut cancelled = false;
    let mut status_code: Option<i32> = None;
    let mut wait_err: Option<io::Error> = None;
    let mut tail: Vec<u8> = Vec::new();

    std::thread::scope(|s| {
        if let Some(out) = stdout {
            s.spawn(move || {
                let mut r = BufReader::new(out);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match r.read_until(b'\n', &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => on_stdout(String::from_utf8_lossy(&buf).trim_end()),
                    }
                }
            });
        }
        let tail_handle = stderr.map(|mut err| {
            s.spawn(move || {
                let mut keep: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 2048];
                loop {
                    match err.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            keep.extend_from_slice(&chunk[..n]);
                            if keep.len() > STDERR_TAIL_BYTES {
                                let cut = keep.len() - STDERR_TAIL_BYTES;
                                keep.drain(..cut);
                            }
                        }
                    }
                }
                keep
            })
        });

        loop {
            if !cancelled && cancel.load(Ordering::SeqCst) {
                cancelled = true;
                match &job {
                    Some(j) => j.terminate(),
                    None => {
                        let _ = child.kill();
                    }
                }
            }
            match child.try_wait() {
                Ok(Some(st)) => {
                    status_code = st.code();
                    break;
                }
                Ok(None) => {
                    on_tick();
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => {
                    wait_err = Some(e);
                    let _ = child.kill();
                    break;
                }
            }
        }
        // Make sure nothing keeps the pipes open, so the reader threads end.
        if let Some(j) = &job {
            j.terminate();
        }
        if let Some(h) = tail_handle {
            tail = h.join().unwrap_or_default();
        }
    });
    drop(job);
    if let Some(e) = wait_err {
        return Err(e);
    }
    Ok(RunOutcome {
        exit_code: status_code,
        cancelled,
        stderr_tail: String::from_utf8_lossy(&tail).into_owned(),
    })
}

/// Run a short helper process and capture stdout. `None` on spawn failure, timeout or
/// a non-zero exit code.
pub fn run_capture(spec: &CommandSpec, timeout: Duration) -> Option<String> {
    let mut cmd = command_for(spec);
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let job = JobObject::new(Some(DEFAULT_MEMORY_LIMIT)).ok();
    if let Some(j) = &job {
        let _ = j.assign(&child);
    }
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = out.read_to_end(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    let ok = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                match &job {
                    Some(j) => j.terminate(),
                    None => {
                        let _ = child.kill();
                    }
                }
                let _ = child.wait();
                break false;
            }
        }
    };
    let bytes = reader.join().unwrap_or_default();
    ok.then(|| String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::Mutex;

    fn cmd(args: &[&str]) -> CommandSpec {
        CommandSpec {
            program: PathBuf::from("cmd.exe"),
            args: args.iter().map(OsString::from).collect(),
        }
    }

    #[test]
    fn captures_stdout_stderr_and_exit_code() {
        let lines = Mutex::new(Vec::new());
        let cancel = AtomicBool::new(false);
        let out = run_tool(
            &cmd(&["/C", "echo one&echo two&echo oops 1>&2&exit 3"]),
            &RunLimits::default(),
            &cancel,
            &|l| lines.lock().unwrap().push(l.to_owned()),
            &mut || {},
        )
        .unwrap();
        assert_eq!(out.exit_code, Some(3));
        assert!(!out.cancelled);
        assert!(out.stderr_tail.contains("oops"));
        assert_eq!(*lines.lock().unwrap(), vec!["one", "two"]);
    }

    #[test]
    fn stderr_is_trimmed_to_tail() {
        let cancel = AtomicBool::new(false);
        // 3 x 3000 bytes to stderr.
        let script = "for /L %i in (1,1,150) do @echo 12345678901234567890123456789012345678901234567890 1>&2";
        let out = run_tool(
            &cmd(&["/C", script]),
            &RunLimits::default(),
            &cancel,
            &|_| {},
            &mut || {},
        )
        .unwrap();
        assert!(out.stderr_tail.len() <= STDERR_TAIL_BYTES);
        assert!(out.stderr_tail.len() > 3000);
    }

    #[test]
    fn cancel_terminates_process() {
        let cancel = AtomicBool::new(false);
        let start = Instant::now();
        let mut ticks = 0;
        let out = run_tool(
            &cmd(&["/C", "ping -n 30 127.0.0.1 >nul"]),
            &RunLimits::default(),
            &cancel,
            &|_| {},
            &mut || {
                ticks += 1;
                if ticks == 5 {
                    cancel.store(true, Ordering::SeqCst);
                }
            },
        )
        .unwrap();
        assert!(out.cancelled);
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn capture_times_out() {
        let t = Instant::now();
        let r = run_capture(&cmd(&["/C", "ping -n 30 127.0.0.1 >nul"]), Duration::from_millis(300));
        assert!(r.is_none());
        assert!(t.elapsed() < Duration::from_secs(10));
        let r = run_capture(&cmd(&["/C", "echo hi"]), Duration::from_secs(5));
        assert_eq!(r.unwrap().trim(), "hi");
    }
}
