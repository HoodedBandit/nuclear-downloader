use crate::runtime::{self, YtdlpCommandConfig};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
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

const MAX_STDERR_LINES: usize = 256;
pub(super) const MAX_STDERR_BYTES: usize = 64 * 1024;
pub(super) const MAX_PROCESS_LINE_BYTES: usize = 64 * 1024;
const PROCESS_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;
#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x00000004;
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;

struct TailBuffer {
    lines: VecDeque<String>,
    bytes: usize,
}

impl TailBuffer {
    fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
        }
    }

    fn push(&mut self, line: String) {
        self.bytes += line.len();
        self.lines.push_back(line);

        while self.lines.len() > MAX_STDERR_LINES || self.bytes > MAX_STDERR_BYTES {
            if let Some(removed) = self.lines.pop_front() {
                self.bytes = self.bytes.saturating_sub(removed.len());
            } else {
                break;
            }
        }
    }

    fn into_string(self) -> String {
        self.lines.into_iter().collect::<Vec<_>>().join("\n")
    }
}

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

fn spawn_stderr_tail_reader(
    mut stderr: tokio::process::ChildStderr,
) -> tokio::task::JoinHandle<Result<String, String>> {
    tokio::spawn(async move {
        let mut tail = TailBuffer::new();
        let mut buffer = [0_u8; 8 * 1024];
        let mut line = Vec::new();
        loop {
            let read = stderr.read(&mut buffer).await.map_err(|error| {
                format!("process_output_failed: could not read stderr: {error}")
            })?;
            if read == 0 {
                break;
            }
            for byte in &buffer[..read] {
                if *byte == b'\n' {
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    tail.push(String::from_utf8_lossy(&line).into_owned());
                    line.clear();
                } else if line.len() < MAX_PROCESS_LINE_BYTES {
                    line.push(*byte);
                } else {
                    return Err(
                        "process_output_limit: stderr contained a line larger than 64 KiB"
                            .to_string(),
                    );
                }
            }
        }
        if !line.is_empty() {
            tail.push(String::from_utf8_lossy(&line).into_owned());
        }

        Ok(tail.into_string())
    })
}

fn flatten_stderr_result(
    result: Result<Result<String, String>, tokio::task::JoinError>,
) -> Result<String, String> {
    result.map_err(|_| "process_output_failed: stderr reader stopped unexpectedly".to_string())?
}

enum StdoutEvent {
    Line(String),
    End,
    Error(String),
}

struct OwnedReaderTask<T> {
    handle: Option<tokio::task::JoinHandle<T>>,
}

impl<T> OwnedReaderTask<T> {
    fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    fn handle_mut(&mut self) -> &mut tokio::task::JoinHandle<T> {
        self.handle.as_mut().expect("reader task is still active")
    }

    fn is_active(&self) -> bool {
        self.handle.is_some()
    }

    fn disarm_completed(&mut self) {
        let _ = self.handle.take();
    }

    async fn join(&mut self) -> Result<T, tokio::task::JoinError> {
        self.handle
            .take()
            .expect("reader task is still active")
            .await
    }

    async fn abort_and_wait(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
            let _ = handle.await;
        }
    }
}

impl<T> Drop for OwnedReaderTask<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

fn spawn_stdout_line_reader(
    stdout: tokio::process::ChildStdout,
) -> (
    tokio::sync::mpsc::Receiver<StdoutEvent>,
    OwnedReaderTask<()>,
) {
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let handle = tokio::spawn(async move {
        let mut stdout = BufReader::new(stdout);
        loop {
            let event = match read_bounded_line(&mut stdout, MAX_PROCESS_LINE_BYTES).await {
                Ok(Some(line)) => StdoutEvent::Line(line),
                Ok(None) => StdoutEvent::End,
                Err(error) => StdoutEvent::Error(format!(
                    "process_output_failed: failed to read process output: {error}"
                )),
            };
            let terminal = !matches!(event, StdoutEvent::Line(_));
            if sender.send(event).await.is_err() || terminal {
                return;
            }
        }
    });
    (receiver, OwnedReaderTask::new(handle))
}

#[derive(Debug)]
pub(super) struct StreamedProcessOutput {
    pub(super) status: std::process::ExitStatus,
    pub(super) stderr: String,
    pub(super) cancelled: bool,
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

pub(super) async fn wait_with_streamed_stdout<F, Fut>(
    mut child: tokio::process::Child,
    job: &DownloadJob,
    process_name: &str,
    mut on_stdout_line: F,
) -> Result<StreamedProcessOutput, String>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = terminate_and_reap_child(&mut child, job, process_name).await;
            return Err(format!("Failed to capture {process_name} output."));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = terminate_and_reap_child(&mut child, job, process_name).await;
            return Err(format!("Failed to capture {process_name} errors."));
        }
    };
    // The owned reader task is the only future that consumes stdout. Keeping its
    // partial-line buffer outside select! prevents child-exit/cancellation races
    // from dropping a prefix that has already been read from the pipe.
    let (mut stdout_events, mut stdout_task) = spawn_stdout_line_reader(stdout);
    let mut stdout_open = true;
    let mut pending_callback: Option<std::pin::Pin<Box<Fut>>> = None;
    let mut stderr_task = OwnedReaderTask::new(spawn_stderr_tail_reader(stderr));
    let mut stderr_output: Option<String> = None;
    let mut terminal_error: Option<String> = None;
    let mut cancelled = false;

    let status = loop {
        tokio::select! {
            callback_result = async {
                pending_callback
                    .as_mut()
                    .expect("guarded pending callback")
                    .await
            }, if pending_callback.is_some() => {
                pending_callback = None;
                if let Err(error) = callback_result {
                    terminal_error = Some(error);
                    stdout_task.abort_and_wait().await;
                    stderr_task.abort_and_wait().await;
                    stdout_open = false;
                    stderr_output = Some(String::new());
                    break terminate_and_reap_child(&mut child, job, process_name).await?;
                }
            }
            status = child.wait() => {
                match status {
                    Ok(status) => break status,
                    Err(error) => {
                        drop(pending_callback.take());
                        stdout_task.abort_and_wait().await;
                        stderr_task.abort_and_wait().await;
                        let _ = terminate_and_reap_child(&mut child, job, process_name).await;
                        return Err(format!(
                            "process_wait_failed: failed to wait for {process_name}: {error}"
                        ));
                    }
                }
            }
            event = stdout_events.recv(), if stdout_open && pending_callback.is_none() => {
                match event {
                    Some(StdoutEvent::Line(line)) => {
                        pending_callback = Some(Box::pin(on_stdout_line(line)));
                    }
                    Some(StdoutEvent::End) => stdout_open = false,
                    Some(StdoutEvent::Error(error)) => {
                        terminal_error = Some(error);
                        let _ = stdout_task.join().await;
                        stderr_task.abort_and_wait().await;
                        stdout_open = false;
                        stderr_output = Some(String::new());
                        break terminate_and_reap_child(&mut child, job, process_name).await?;
                    }
                    None => {
                        terminal_error = Some(
                            "process_output_failed: stdout reader stopped unexpectedly".to_string(),
                        );
                        let _ = stdout_task.join().await;
                        stderr_task.abort_and_wait().await;
                        stdout_open = false;
                        stderr_output = Some(String::new());
                        break terminate_and_reap_child(&mut child, job, process_name).await?;
                    }
                }
            }
            result = async { stderr_task.handle_mut().await }, if stderr_output.is_none() => {
                stderr_task.disarm_completed();
                match flatten_stderr_result(result) {
                    Ok(output) => stderr_output = Some(output),
                    Err(error) => {
                        pending_callback = None;
                        stdout_task.abort_and_wait().await;
                        stdout_open = false;
                        stderr_output = Some(String::new());
                        terminal_error = Some(error);
                        break terminate_and_reap_child(&mut child, job, process_name).await?;
                    }
                }
            }
            _ = job.cancelled() => {
                cancelled = true;
                pending_callback = None;
                stdout_task.abort_and_wait().await;
                stderr_task.abort_and_wait().await;
                stdout_open = false;
                stderr_output = Some(String::new());
                break terminate_and_reap_child(&mut child, job, process_name).await?;
            }
        }
    };

    // This deadline starts as soon as the direct child exits or is reaped. It
    // bounds inherited pipes and any progress callback that was already pending.
    let drain_deadline = tokio::time::Instant::now() + PROCESS_DRAIN_TIMEOUT;
    while stdout_open || stderr_output.is_none() || pending_callback.is_some() {
        tokio::select! {
            callback_result = async {
                pending_callback
                    .as_mut()
                    .expect("guarded pending callback")
                    .await
            }, if pending_callback.is_some() => {
                pending_callback = None;
                if let Err(error) = callback_result {
                    terminal_error.get_or_insert(error);
                    job.terminate_processes();
                }
            }
            event = stdout_events.recv(), if stdout_open && pending_callback.is_none() => {
                match event {
                    Some(StdoutEvent::Line(line)) => {
                        if terminal_error.is_none() && !cancelled {
                            pending_callback = Some(Box::pin(on_stdout_line(line)));
                        }
                    }
                    Some(StdoutEvent::End) => stdout_open = false,
                    Some(StdoutEvent::Error(error)) => {
                        terminal_error.get_or_insert(error);
                        job.terminate_processes();
                        stdout_open = false;
                    }
                    None => {
                        terminal_error.get_or_insert_with(|| {
                            "process_output_failed: stdout reader stopped unexpectedly".to_string()
                        });
                        job.terminate_processes();
                        stdout_open = false;
                    }
                }
            }
            result = async { stderr_task.handle_mut().await }, if stderr_output.is_none() => {
                stderr_task.disarm_completed();
                match flatten_stderr_result(result) {
                    Ok(output) => stderr_output = Some(output),
                    Err(error) => {
                        terminal_error.get_or_insert(error);
                        job.terminate_processes();
                        stderr_output = Some(String::new());
                    }
                }
            }
            _ = job.cancelled(), if !cancelled => {
                cancelled = true;
                pending_callback = None;
                job.terminate_processes();
            }
            _ = tokio::time::sleep_until(drain_deadline) => {
                drop(pending_callback.take());
                job.terminate_processes();
                stdout_task.abort_and_wait().await;
                if stderr_output.is_none() {
                    stderr_task.abort_and_wait().await;
                    stderr_output = Some(String::new());
                }
                if !cancelled {
                    terminal_error.get_or_insert_with(|| {
                        "process_drain_timeout: process output or progress callback did not finish"
                            .to_string()
                    });
                }
                break;
            }
        }
    }

    if stdout_task.is_active() {
        stdout_task
            .join()
            .await
            .map_err(|_| "process_output_failed: stdout reader stopped unexpectedly".to_string())?;
    }
    if let Some(error) = terminal_error {
        return Err(error);
    }

    Ok(StreamedProcessOutput {
        status,
        stderr: stderr_output.unwrap_or_default(),
        cancelled,
    })
}
async fn read_bounded_line<R>(reader: &mut R, max_bytes: usize) -> std::io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
            };
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|position| position + 1)
            .unwrap_or(available.len());
        if bytes.len().saturating_add(take) > max_bytes {
            reader.consume(take);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "process output line exceeded 64 KiB",
            ));
        }
        bytes.extend_from_slice(&available[..take]);
        let ended = available[take - 1] == b'\n';
        reader.consume(take);
        if ended {
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
        }
    }
}

pub(super) fn record_streamed_output_bytes(
    total: &mut usize,
    line_bytes: usize,
    limit: usize,
) -> Result<(), String> {
    *total = total.saturating_add(line_bytes.saturating_add(1));
    if *total > limit {
        Err(format!(
            "process_output_limit: inspection output exceeded the {limit}-byte limit"
        ))
    } else {
        Ok(())
    }
}

async fn read_stream_bounded<R>(mut reader: R, max_bytes: usize) -> Result<Vec<u8>, String>
where
    R: AsyncRead + Unpin,
{
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    let mut total = 0usize;
    let mut current_line = 0usize;
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| format!("Failed to read process output: {error}"))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read);
        if total > max_bytes {
            return Err(format!(
                "process_output_limit: output exceeded the {max_bytes}-byte limit"
            ));
        }
        for byte in &buffer[..read] {
            if *byte == b'\n' {
                current_line = 0;
            } else {
                current_line = current_line.saturating_add(1);
                if current_line > MAX_PROCESS_LINE_BYTES {
                    return Err(
                        "process_output_limit: output contained a line larger than 64 KiB"
                            .to_string(),
                    );
                }
            }
        }
        retained.extend_from_slice(&buffer[..read]);
    }
    Ok(retained)
}

pub(super) async fn wait_with_bounded_output(
    child: tokio::process::Child,
    job: &DownloadJob,
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    wait_with_bounded_output_and_drain(
        child,
        job,
        stdout_limit,
        stderr_limit,
        timeout,
        PROCESS_DRAIN_TIMEOUT,
    )
    .await
}

async fn wait_with_bounded_output_and_drain(
    mut child: tokio::process::Child,
    job: &DownloadJob,
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: Duration,
    drain_timeout: Duration,
) -> Result<std::process::Output, String> {
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = terminate_and_reap_child(&mut child, job, "process").await;
            return Err("Failed to capture process output.".to_string());
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = terminate_and_reap_child(&mut child, job, "process").await;
            return Err("Failed to capture process errors.".to_string());
        }
    };
    let mut stdout_reader =
        OwnedReaderTask::new(tokio::spawn(read_stream_bounded(stdout, stdout_limit)));
    let mut stderr_reader =
        OwnedReaderTask::new(tokio::spawn(read_stream_bounded(stderr, stderr_limit)));
    let mut stdout_result: Option<Vec<u8>> = None;
    let mut stderr_result: Option<Vec<u8>> = None;
    let mut terminal_error: Option<String> = None;
    let deadline = tokio::time::Instant::now() + timeout;

    let status = loop {
        tokio::select! {
            status = child.wait() => {
                match status {
                    Ok(status) => break status,
                    Err(error) => {
                        job.terminate_processes();
                        stdout_reader.abort_and_wait().await;
                        stderr_reader.abort_and_wait().await;
                        let _ = terminate_and_reap_child(&mut child, job, "process").await;
                        return Err(format!("process_wait_failed: {error}"));
                    }
                }
            }
            result = async { stdout_reader.handle_mut().await }, if stdout_result.is_none() => {
                stdout_reader.disarm_completed();
                match result {
                    Ok(Ok(output)) => stdout_result = Some(output),
                    Ok(Err(error)) => {
                        stdout_result = Some(Vec::new());
                        terminal_error = Some(error);
                        break terminate_and_reap_child(&mut child, job, "process").await?;
                    }
                    Err(_) => {
                        stdout_result = Some(Vec::new());
                        terminal_error = Some("process_output_failed: stdout reader stopped unexpectedly".to_string());
                        break terminate_and_reap_child(&mut child, job, "process").await?;
                    }
                }
            }
            result = async { stderr_reader.handle_mut().await }, if stderr_result.is_none() => {
                stderr_reader.disarm_completed();
                match result {
                    Ok(Ok(output)) => stderr_result = Some(output),
                    Ok(Err(error)) => {
                        stderr_result = Some(Vec::new());
                        terminal_error = Some(error);
                        break terminate_and_reap_child(&mut child, job, "process").await?;
                    }
                    Err(_) => {
                        stderr_result = Some(Vec::new());
                        terminal_error = Some("process_output_failed: stderr reader stopped unexpectedly".to_string());
                        break terminate_and_reap_child(&mut child, job, "process").await?;
                    }
                }
            }
            _ = job.cancelled() => {
                terminal_error = Some("process_cancelled: operation was cancelled".to_string());
                break terminate_and_reap_child(&mut child, job, "process").await?;
            }
            _ = tokio::time::sleep_until(deadline) => {
                terminal_error = Some("process_timeout: operation timed out".to_string());
                break terminate_and_reap_child(&mut child, job, "process").await?;
            }
        }
    };

    let drain_deadline = tokio::time::Instant::now() + drain_timeout;
    while stdout_result.is_none() || stderr_result.is_none() {
        tokio::select! {
            result = async { stdout_reader.handle_mut().await }, if stdout_result.is_none() => {
                stdout_reader.disarm_completed();
                stdout_result = Some(match result {
                    Ok(Ok(output)) => output,
                    Ok(Err(error)) => {
                        terminal_error.get_or_insert(error);
                        job.terminate_processes();
                        Vec::new()
                    }
                    Err(_) => {
                        terminal_error.get_or_insert_with(|| "process_output_failed: stdout reader stopped unexpectedly".to_string());
                        job.terminate_processes();
                        Vec::new()
                    }
                });
            }
            result = async { stderr_reader.handle_mut().await }, if stderr_result.is_none() => {
                stderr_reader.disarm_completed();
                stderr_result = Some(match result {
                    Ok(Ok(output)) => output,
                    Ok(Err(error)) => {
                        terminal_error.get_or_insert(error);
                        job.terminate_processes();
                        Vec::new()
                    }
                    Err(_) => {
                        terminal_error.get_or_insert_with(|| "process_output_failed: stderr reader stopped unexpectedly".to_string());
                        job.terminate_processes();
                        Vec::new()
                    }
                });
            }
            _ = job.cancelled(), if terminal_error.is_none() => {
                terminal_error = Some("process_cancelled: operation was cancelled during output drain".to_string());
                job.terminate_processes();
            }
            _ = tokio::time::sleep_until(drain_deadline) => {
                job.terminate_processes();
                if stdout_result.is_none() {
                    stdout_reader.abort_and_wait().await;
                }
                if stderr_result.is_none() {
                    stderr_reader.abort_and_wait().await;
                }
                return Err("process_drain_timeout: a descendant kept a process output pipe open".to_string());
            }
        }
    }
    if let Some(error) = terminal_error {
        return Err(error);
    }
    Ok(std::process::Output {
        status,
        stdout: stdout_result.unwrap_or_default(),
        stderr: stderr_result.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::{wait_with_bounded_output_and_drain, wait_with_streamed_stdout, DownloadJob};
    use std::time::Duration;

    #[cfg(windows)]
    async fn spawn_powershell_fixture(job: &DownloadJob, script: &str) -> tokio::process::Child {
        let readiness_root =
            std::env::temp_dir().join(format!("nuclear-process-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&readiness_root).unwrap();
        let readiness_path = readiness_root.join("ready");
        let escaped_readiness_path = readiness_path.to_string_lossy().replace('\'', "''");
        let synchronized_script =
            format!("[IO.File]::WriteAllText('{escaped_readiness_path}', 'ready'); {script}");
        let mut command = tokio::process::Command::new("powershell.exe");
        command
            .kill_on_drop(true)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &synchronized_script,
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = job
            .spawn(&mut command, "test fixture", false)
            .await
            .unwrap();

        let readiness = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if readiness_path.is_file() {
                    return Ok(());
                }
                if let Some(status) = child
                    .try_wait()
                    .map_err(|error| format!("fixture readiness check failed: {error}"))?
                {
                    return Err(format!(
                        "fixture exited before signalling readiness: {status}"
                    ));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| Err("fixture readiness timed out after 60 seconds".to_string()));

        let cleanup_readiness = || {
            let _ = std::fs::remove_file(&readiness_path);
            let _ = std::fs::remove_dir(&readiness_root);
        };
        if let Err(error) = readiness {
            job.cancel();
            let _ = child.kill().await;
            let _ = child.wait().await;
            cleanup_readiness();
            panic!("{error}");
        }
        cleanup_readiness();
        child
    }

    #[cfg(windows)]
    pub(crate) async fn spawn_inherited_pipe_fixture(
        job: &DownloadJob,
        caller_root: &std::path::Path,
        parent_script: &str,
        descendant_script: &str,
    ) -> Result<(tokio::process::Child, std::path::PathBuf), String> {
        let root_metadata = std::fs::symlink_metadata(caller_root)
            .map_err(|error| format!("Failed to inspect inherited-pipe fixture root: {error}"))?;
        use std::os::windows::fs::MetadataExt;
        let is_reparse = root_metadata.file_attributes() & 0x400 != 0;
        if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() || is_reparse {
            return Err(
                "Inherited-pipe fixture root must be a regular non-symlink directory.".to_string(),
            );
        }
        let caller_root = caller_root
            .canonicalize()
            .map_err(|error| format!("Failed to resolve inherited-pipe fixture root: {error}"))?;
        let fixture_root = caller_root.join(format!(
            "nuclear-inherited-pipe-fixture-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&fixture_root)
            .map_err(|error| format!("Failed to create inherited-pipe fixture root: {error}"))?;
        let descendant_path = fixture_root.join("descendant.ps1");
        std::fs::write(&descendant_path, descendant_script)
            .map_err(|error| format!("Failed to write inherited-pipe descendant: {error}"))?;
        let parent_path = fixture_root.join("parent.ps1");
        let escaped_descendant_path = descendant_path.to_string_lossy().replace('\'', "''");
        let script = r#"
$nativeSource = @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class NuclearFixtureNativeMethods {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct STARTUPINFO {
        public Int32 cb;
        public string lpReserved;
        public string lpDesktop;
        public string lpTitle;
        public Int32 dwX;
        public Int32 dwY;
        public Int32 dwXSize;
        public Int32 dwYSize;
        public Int32 dwXCountChars;
        public Int32 dwYCountChars;
        public Int32 dwFillAttribute;
        public Int32 dwFlags;
        public Int16 wShowWindow;
        public Int16 cbReserved2;
        public IntPtr lpReserved2;
        public IntPtr hStdInput;
        public IntPtr hStdOutput;
        public IntPtr hStdError;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct PROCESS_INFORMATION {
        public IntPtr hProcess;
        public IntPtr hThread;
        public Int32 dwProcessId;
        public Int32 dwThreadId;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern IntPtr GetCurrentProcess();

    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern IntPtr GetStdHandle(Int32 handleKind);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool DuplicateHandle(
        IntPtr sourceProcess,
        IntPtr sourceHandle,
        IntPtr targetProcess,
        out IntPtr targetHandle,
        UInt32 desiredAccess,
        [MarshalAs(UnmanagedType.Bool)] bool inheritHandle,
        UInt32 options);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool CreateProcess(
        string applicationName,
        StringBuilder commandLine,
        IntPtr processAttributes,
        IntPtr threadAttributes,
        [MarshalAs(UnmanagedType.Bool)] bool inheritHandles,
        UInt32 creationFlags,
        IntPtr environment,
        string currentDirectory,
        ref STARTUPINFO startupInfo,
        out PROCESS_INFORMATION processInformation);

    [DllImport("kernel32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool CloseHandle(IntPtr handle);

    public static Int32 SpawnPowerShellWithInheritedOutput(string executable, string scriptPath) {
        IntPtr currentProcess = GetCurrentProcess();
        IntPtr stdoutCopy;
        if (!DuplicateHandle(
            currentProcess,
            GetStdHandle(-11),
            currentProcess,
            out stdoutCopy,
            0,
            true,
            2)) {
            return Marshal.GetLastWin32Error();
        }
        IntPtr stderrCopy;
        if (!DuplicateHandle(
            currentProcess,
            GetStdHandle(-12),
            currentProcess,
            out stderrCopy,
            0,
            true,
            2)) {
            Int32 error = Marshal.GetLastWin32Error();
            CloseHandle(stdoutCopy);
            return error;
        }

        STARTUPINFO startupInfo = new STARTUPINFO();
        startupInfo.cb = Marshal.SizeOf(typeof(STARTUPINFO));
        startupInfo.dwFlags = 0x100;
        startupInfo.hStdInput = IntPtr.Zero;
        startupInfo.hStdOutput = stdoutCopy;
        startupInfo.hStdError = stderrCopy;
        PROCESS_INFORMATION processInformation;
        StringBuilder commandLine = new StringBuilder(
            "\"" + executable +
            "\" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"" +
            scriptPath + "\"");
        bool created = CreateProcess(
            executable,
            commandLine,
            IntPtr.Zero,
            IntPtr.Zero,
            true,
            0x08000000,
            IntPtr.Zero,
            null,
            ref startupInfo,
            out processInformation);
        Int32 createError = created ? 0 : Marshal.GetLastWin32Error();
        CloseHandle(stdoutCopy);
        CloseHandle(stderrCopy);
        if (created) {
            CloseHandle(processInformation.hThread);
            CloseHandle(processInformation.hProcess);
        }
        return createError;
    }
}
'@

Add-Type -TypeDefinition $nativeSource
__PARENT_SCRIPT__

$powershellPath = Join-Path $PSHOME 'powershell.exe'
$createError = [NuclearFixtureNativeMethods]::SpawnPowerShellWithInheritedOutput(
    $powershellPath,
    '__DESCENDANT_PATH__')
if ($createError -ne 0) {
    throw "failed to create fixture descendant: $createError"
}
"#
        .replace("__PARENT_SCRIPT__", parent_script)
        .replace("__DESCENDANT_PATH__", &escaped_descendant_path);
        std::fs::write(&parent_path, script)
            .map_err(|error| format!("Failed to write inherited-pipe parent: {error}"))?;
        let mut command = tokio::process::Command::new("powershell.exe");
        command
            .kill_on_drop(true)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&parent_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = match job
            .spawn(&mut command, "inherited-pipe fixture", false)
            .await
        {
            Ok(child) => child,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&fixture_root);
                return Err(error);
            }
        };
        Ok((child, fixture_root))
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn supervised_process_cancels_and_reaps_promptly() {
        let job = DownloadJob::new().unwrap();
        let child = spawn_powershell_fixture(&job, "Start-Sleep -Seconds 30").await;
        let cancellation = job.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancellation.cancel();
        });

        let started = std::time::Instant::now();
        let error = wait_with_bounded_output_and_drain(
            child,
            &job,
            64 * 1024,
            64 * 1024,
            Duration::from_secs(30),
            Duration::from_millis(500),
        )
        .await
        .unwrap_err();

        assert!(error.starts_with("process_cancelled:"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cancellation_remains_live_while_progress_callback_is_blocked() {
        let job = DownloadJob::new().unwrap();
        let child = spawn_powershell_fixture(
            &job,
            "[Console]::Out.WriteLine('callback-ready'); Start-Sleep -Seconds 30",
        )
        .await;
        let callback_entered = std::sync::Arc::new(tokio::sync::Notify::new());
        let callback_release = std::sync::Arc::new(tokio::sync::Notify::new());
        let wait_job = job.clone();
        let wait_entered = callback_entered.clone();
        let wait_release = callback_release.clone();
        let waiter = tokio::spawn(async move {
            wait_with_streamed_stdout(child, &wait_job, "test fixture", move |_| {
                let entered = wait_entered.clone();
                let release = wait_release.clone();
                async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok(())
                }
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), callback_entered.notified())
            .await
            .expect("progress callback did not start");

        let started = std::time::Instant::now();
        job.cancel();
        let output = tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("cancellation was blocked by the progress callback")
            .expect("stream waiter panicked")
            .expect("cancelled stream should return its terminal output");

        assert!(output.cancelled);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn child_exit_starts_drain_deadline_while_progress_callback_is_blocked() {
        let job = DownloadJob::new().unwrap();
        let (child, fixture_root) = spawn_inherited_pipe_fixture(
            &job,
            &std::env::temp_dir(),
            "[Console]::Out.WriteLine('callback-ready')",
            "Start-Sleep -Seconds 30",
        )
        .await
        .unwrap();
        let callback_entered = std::sync::Arc::new(tokio::sync::Notify::new());
        let callback_release = std::sync::Arc::new(tokio::sync::Notify::new());
        let wait_entered = callback_entered.clone();
        let wait_release = callback_release.clone();
        let started = std::time::Instant::now();
        let waiter = tokio::spawn(async move {
            wait_with_streamed_stdout(child, &job, "test fixture", move |_| {
                let entered = wait_entered.clone();
                let release = wait_release.clone();
                async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok(())
                }
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), callback_entered.notified())
            .await
            .expect("progress callback did not start");

        let error = tokio::time::timeout(Duration::from_secs(12), waiter)
            .await
            .expect("post-exit progress callback bypassed the drain deadline")
            .expect("stream waiter panicked")
            .unwrap_err();

        assert!(error.starts_with("process_drain_timeout:"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(12));
        let _ = std::fs::remove_dir_all(fixture_root);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn split_stdout_line_survives_parent_exit_selection() {
        let job = DownloadJob::new().unwrap();
        let (child, fixture_root) = spawn_inherited_pipe_fixture(
            &job,
            &std::env::temp_dir(),
            "[Console]::Out.Write('prefix-'); [Console]::Out.Flush()",
            "Start-Sleep -Milliseconds 500; [Console]::Out.WriteLine('suffix')",
        )
        .await
        .unwrap();
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = lines.clone();

        let output = tokio::time::timeout(
            Duration::from_secs(10),
            wait_with_streamed_stdout(child, &job, "test fixture", move |line| {
                recorded.lock().unwrap().push(line);
                std::future::ready(Ok(()))
            }),
        )
        .await
        .expect("split stdout fixture exceeded its bound")
        .expect("split stdout fixture failed");

        assert!(
            output.status.success(),
            "fixture status was {:?}; stderr: {}",
            output.status,
            output.stderr
        );
        assert!(!output.cancelled);
        assert_eq!(*lines.lock().unwrap(), vec!["prefix-suffix".to_string()]);
        let _ = std::fs::remove_dir_all(fixture_root);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cancelled_job_never_runs_suspended_child_first_instruction() {
        let root = std::env::temp_dir().join(format!(
            "nuclear-suspended-child-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let sentinel = root.join("child-ran.txt");
        let escaped = sentinel.to_string_lossy().replace('\'', "''");
        let script = format!("[IO.File]::WriteAllText('{escaped}', 'ran')");
        let job = DownloadJob::new().unwrap();
        job.cancel();

        let mut command = tokio::process::Command::new("powershell.exe");
        command
            .kill_on_drop(true)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &script,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let error = job
            .spawn(&mut command, "cancelled fixture", false)
            .await
            .unwrap_err();

        assert!(error.contains("cancelled"), "{error}");
        assert!(!sentinel.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn supervised_process_stops_oversized_output_without_draining_producer() {
        let job = DownloadJob::new().unwrap();
        let child = spawn_powershell_fixture(
            &job,
            "[Console]::Out.Write('x' * 70000); Start-Sleep -Seconds 30",
        )
        .await;

        let started = std::time::Instant::now();
        let error = wait_with_bounded_output_and_drain(
            child,
            &job,
            128 * 1024,
            64 * 1024,
            Duration::from_secs(30),
            Duration::from_millis(500),
        )
        .await
        .unwrap_err();

        assert!(error.starts_with("process_output_limit:"), "{error}");
        // Readiness excludes cold PowerShell startup while still rejecting a full fixture drain.
        assert!(started.elapsed() < Duration::from_secs(15));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn supervised_process_bounds_inherited_stderr_after_parent_exit() {
        let job = DownloadJob::new().unwrap();
        let (child, fixture_root) = spawn_inherited_pipe_fixture(
            &job,
            &std::env::temp_dir(),
            "",
            "Start-Sleep -Seconds 30",
        )
        .await
        .unwrap();

        let started = std::time::Instant::now();
        let error = wait_with_bounded_output_and_drain(
            child,
            &job,
            64 * 1024,
            64 * 1024,
            Duration::from_secs(30),
            Duration::from_millis(250),
        )
        .await
        .unwrap_err();

        assert!(error.starts_with("process_drain_timeout:"), "{error}");
        // Readiness excludes cold PowerShell startup while still rejecting a full fixture drain.
        assert!(started.elapsed() < Duration::from_secs(15));
        let _ = std::fs::remove_dir_all(fixture_root);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn streamed_process_bounds_inherited_pipes_after_parent_exit() {
        let job = DownloadJob::new().unwrap();
        let (child, fixture_root) = spawn_inherited_pipe_fixture(
            &job,
            &std::env::temp_dir(),
            "",
            "Start-Sleep -Seconds 30",
        )
        .await
        .unwrap();

        let started = std::time::Instant::now();
        let error =
            wait_with_streamed_stdout(child, &job, "test fixture", |_| std::future::ready(Ok(())))
                .await
                .unwrap_err();

        assert!(error.starts_with("process_drain_timeout:"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(15));
        let _ = std::fs::remove_dir_all(fixture_root);
    }
}
