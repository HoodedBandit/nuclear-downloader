use crate::runtime::{self, YtdlpCommandConfig};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

pub(super) const MAX_STDERR_BYTES: usize = 64 * 1024;
pub(super) const MAX_PROCESS_LINE_BYTES: usize = 64 * 1024;
const PROCESS_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;
#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x00000004;
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;

mod output;
#[cfg(test)]
use output::wait_with_bounded_output_and_drain;
pub(super) use output::{
    record_streamed_output_bytes, wait_with_bounded_output, wait_with_streamed_stdout,
};
#[derive(Clone)]
pub(crate) struct DownloadJob {
    supervisor: Arc<ProcessSupervisor>,
}

struct ProcessSupervisor {
    cancellation: CancellationToken,
    runtime_tools: std::sync::Mutex<HashMap<String, runtime::RuntimeToolLease>>,
    #[cfg(windows)]
    process_job: Arc<WindowsProcessJob>,
}

#[cfg(windows)]
struct WindowsProcessJob {
    handle: HANDLE,
}

#[cfg(windows)]
unsafe impl Send for WindowsProcessJob {}
#[cfg(windows)]
unsafe impl Sync for WindowsProcessJob {}

#[cfg(windows)]
impl WindowsProcessJob {
    fn new() -> Result<Self, String> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(format!(
                "Failed to create Windows process job: {}",
                std::io::Error::last_os_error()
            ));
        }

        let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &information as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };

        if configured == 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            return Err(format!("Failed to configure Windows process job: {error}"));
        }

        Ok(Self { handle })
    }

    fn assign(&self, child: &tokio::process::Child) -> Result<(), String> {
        let process_handle = child
            .raw_handle()
            .ok_or_else(|| "Downloader process did not expose a Windows handle.".to_string())?;
        let assigned = unsafe { AssignProcessToJobObject(self.handle, process_handle as HANDLE) };
        if assigned == 0 {
            Err(format!(
                "Failed to attach downloader process to its Windows job: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(())
        }
    }

    fn resume(&self, child: &tokio::process::Child) -> Result<(), String> {
        let process_id = child
            .id()
            .ok_or_else(|| "Downloader process did not expose a process ID.".to_string())?;
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(format!(
                "Failed to enumerate the suspended downloader process: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut found = false;
        let mut next = unsafe { Thread32First(snapshot, &mut entry) };
        while next != 0 {
            if entry.th32OwnerProcessID == process_id {
                found = true;
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    unsafe { CloseHandle(snapshot) };
                    return Err(format!(
                        "Failed to open the suspended downloader thread: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                let previous_count = unsafe { ResumeThread(thread) };
                unsafe { CloseHandle(thread) };
                if previous_count == u32::MAX {
                    unsafe { CloseHandle(snapshot) };
                    return Err(format!(
                        "Failed to resume the supervised downloader process: {}",
                        std::io::Error::last_os_error()
                    ));
                }
            }
            next = unsafe { Thread32Next(snapshot, &mut entry) };
        }
        unsafe { CloseHandle(snapshot) };
        if found {
            Ok(())
        } else {
            Err("The suspended downloader process exposed no resumable thread.".into())
        }
    }

    fn terminate(&self) {
        unsafe {
            TerminateJobObject(self.handle, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsProcessJob {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

impl DownloadJob {
    pub(crate) fn new() -> Result<Self, String> {
        Ok(Self {
            supervisor: Arc::new(ProcessSupervisor {
                cancellation: CancellationToken::new(),
                runtime_tools: std::sync::Mutex::new(HashMap::new()),
                #[cfg(windows)]
                process_job: Arc::new(WindowsProcessJob::new()?),
            }),
        })
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.supervisor.cancellation.is_cancelled()
    }

    pub(crate) async fn cancelled(&self) {
        self.supervisor.cancellation.cancelled().await;
    }

    pub(crate) fn cancel(&self) {
        self.supervisor.cancellation.cancel();
        self.terminate_processes();
    }

    pub(crate) fn terminate_processes(&self) {
        #[cfg(windows)]
        self.supervisor.process_job.terminate();
    }

    fn runtime_tool(&self, name: &str, required: bool) -> Result<Option<PathBuf>, String> {
        let mut leases = self
            .supervisor
            .runtime_tools
            .lock()
            .map_err(|_| "Runtime executable lease registry was poisoned.".to_string())?;
        if let Some(lease) = leases.get(name) {
            return Ok(Some(lease.path().to_path_buf()));
        }
        match runtime::resolve_tool_lease(name)? {
            Some(lease) => {
                let path = lease.path().to_path_buf();
                leases.insert(name.to_string(), lease);
                Ok(Some(path))
            }
            None if required => Err(format!("Required runtime executable {name} was not found.")),
            None => Ok(None),
        }
    }

    pub(super) fn required_runtime_tool(&self, name: &str) -> Result<PathBuf, String> {
        self.runtime_tool(name, true)?
            .ok_or_else(|| format!("Required runtime executable {name} was not found."))
    }

    pub(super) fn ytdlp_runtime_config(&self) -> Result<YtdlpCommandConfig, String> {
        let ffmpeg = self.runtime_tool("ffmpeg", true)?;
        let deno = self.runtime_tool("deno", false)?;
        Ok(YtdlpCommandConfig {
            ffmpeg_dir: ffmpeg.and_then(|path| path.parent().map(Path::to_path_buf)),
            deno_path: deno,
            plugin_dir: runtime::plugin_dir(),
        })
    }

    fn attach_process(&self, child: &tokio::process::Child) -> Result<bool, String> {
        #[cfg(windows)]
        {
            self.supervisor.process_job.assign(child)?;
            if self.is_cancelled() {
                return Ok(false);
            }
            self.supervisor.process_job.resume(child)?;
        }
        Ok(!self.is_cancelled())
    }

    pub(super) async fn spawn(
        &self,
        command: &mut Command,
        process_name: &str,
        below_normal_priority: bool,
    ) -> Result<tokio::process::Child, String> {
        if self.is_cancelled() {
            return Err(format!("{process_name} operation was cancelled."));
        }
        #[cfg(windows)]
        command.creation_flags(supervised_process_flags(below_normal_priority));
        #[cfg(not(windows))]
        let _ = below_normal_priority;
        let mut child = command
            .spawn()
            .map_err(|error| format!("Failed to start {process_name}: {error}"))?;
        match self.attach_process(&child) {
            Ok(true) => Ok(child),
            Ok(false) => {
                let _ = terminate_and_reap_child(&mut child, self, process_name).await;
                Err(format!("{process_name} operation was cancelled."))
            }
            Err(error) => {
                let _ = terminate_and_reap_child(&mut child, self, process_name).await;
                Err(error)
            }
        }
    }
}

pub(crate) async fn run_supervised_probe(
    binary: &Path,
    arguments: &[&str],
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<std::process::Output, String> {
    let job = DownloadJob::new()?;
    let mut command = Command::new(binary);
    command
        .kill_on_drop(true)
        .args(arguments)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = job.spawn(&mut command, "runtime probe", false).await?;
    wait_with_bounded_output(
        child,
        &job,
        stdout_limit.min(MAX_PROCESS_LINE_BYTES),
        stderr_limit.min(MAX_PROCESS_LINE_BYTES),
        timeout,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn test_run_supervised_absolute_child(
    job: &DownloadJob,
    executable: &Path,
    arguments: &[String],
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<std::process::Output, String> {
    if !executable.is_absolute() {
        return Err("Test supervisor executable must be an absolute path.".to_string());
    }
    let metadata = std::fs::symlink_metadata(executable)
        .map_err(|error| format!("Failed to inspect test supervisor executable: {error}"))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("Test supervisor executable must be a regular file.".to_string());
    }

    let mut command = Command::new(executable);
    command
        .kill_on_drop(true)
        .args(arguments)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = job
        .spawn(&mut command, "test supervised child", false)
        .await?;
    wait_with_bounded_output(child, job, stdout_limit, stderr_limit, timeout).await
}

#[cfg(all(test, windows))]
pub(crate) struct TestInheritedPipeFixtureOptions<'a> {
    pub caller_root: &'a Path,
    pub parent_script: &'a str,
    pub descendant_script: &'a str,
    pub timeout: Duration,
    pub drain_timeout: Duration,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
}

#[cfg(all(test, windows))]
pub(crate) async fn test_run_supervised_inherited_pipe_fixture(
    job: &DownloadJob,
    options: TestInheritedPipeFixtureOptions<'_>,
) -> Result<std::process::Output, String> {
    let (child, fixture_root) = tests::spawn_inherited_pipe_fixture(
        job,
        options.caller_root,
        options.parent_script,
        options.descendant_script,
    )
    .await?;
    let result = wait_with_bounded_output_and_drain(
        child,
        job,
        options.stdout_limit,
        options.stderr_limit,
        options.timeout,
        options.drain_timeout,
    )
    .await;
    let _ = std::fs::remove_dir_all(fixture_root);
    result
}

fn supervised_process_flags(below_normal_priority: bool) -> u32 {
    CREATE_NO_WINDOW
        | CREATE_SUSPENDED
        | if below_normal_priority {
            BELOW_NORMAL_PRIORITY_CLASS
        } else {
            0
        }
}

async fn terminate_and_reap_child(
    child: &mut tokio::process::Child,
    job: &DownloadJob,
    process_name: &str,
) -> Result<std::process::ExitStatus, String> {
    job.terminate_processes();
    let _ = child.start_kill();
    match tokio::time::timeout(PROCESS_DRAIN_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(error)) => Err(format!(
            "process_reap_failed: failed to reap {process_name}: {error}"
        )),
        Err(_) => Err(format!(
            "process_reap_timeout: timed out while reaping {process_name}"
        )),
    }
}

#[cfg(test)]
mod tests;
