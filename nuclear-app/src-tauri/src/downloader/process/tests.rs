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
    let (child, fixture_root) =
        spawn_inherited_pipe_fixture(&job, &std::env::temp_dir(), "", "Start-Sleep -Seconds 30")
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
    let (child, fixture_root) =
        spawn_inherited_pipe_fixture(&job, &std::env::temp_dir(), "", "Start-Sleep -Seconds 30")
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
