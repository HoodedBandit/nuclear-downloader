use super::{
    terminate_and_reap_child, DownloadJob, MAX_PROCESS_LINE_BYTES, MAX_STDERR_BYTES,
    PROCESS_DRAIN_TIMEOUT,
};
use std::collections::VecDeque;
use std::future::Future;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
const MAX_STDERR_LINES: usize = 256;
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
pub(in crate::downloader) struct StreamedProcessOutput {
    pub(in crate::downloader) status: std::process::ExitStatus,
    pub(in crate::downloader) stderr: String,
    pub(in crate::downloader) cancelled: bool,
}

pub(in crate::downloader) async fn wait_with_streamed_stdout<F, Fut>(
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

pub(in crate::downloader) fn record_streamed_output_bytes(
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

pub(in crate::downloader) async fn wait_with_bounded_output(
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

pub(super) async fn wait_with_bounded_output_and_drain(
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
