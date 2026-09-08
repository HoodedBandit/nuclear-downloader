use crate::app_error::AppError;
use crate::diagnostics::Diagnostics;
use crate::lifecycle::{DownloadManager, TrackedTaskKind};
use crate::notifications::{DownloadProgressSink, RuntimeProgressSink, UpdateProgressSink};
use crate::outbox::StatePublication;
use crate::services::updates::InstallerActions;
use crate::state::StateStore;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

fn record_event_delivery_failure(diagnostics: &Diagnostics, event: &str, error: &str) {
    diagnostics.log(
        "error",
        "event_delivery_failed",
        &uuid::Uuid::new_v4().to_string(),
        &format!("Event {event} could not be delivered: {error}"),
    );
}

pub(crate) fn download_progress_sink(app: &AppHandle, store: &StateStore) -> DownloadProgressSink {
    let app = app.clone();
    let diagnostics = store.diagnostics().clone();
    Arc::new(move |progress| {
        if let Err(error) = app.emit("download-progress", progress) {
            record_event_delivery_failure(&diagnostics, "download-progress", &error.to_string());
        }
    })
}

pub(crate) fn runtime_progress_sink(app: &AppHandle, store: &StateStore) -> RuntimeProgressSink {
    let app = app.clone();
    let diagnostics = store.diagnostics().clone();
    Arc::new(move |progress| {
        if let Err(error) = app.emit("downloader-runtime-update-progress", progress) {
            record_event_delivery_failure(
                &diagnostics,
                "downloader-runtime-update-progress",
                &error.to_string(),
            );
        }
    })
}

pub(crate) fn update_progress_sink(app: &AppHandle, store: &StateStore) -> UpdateProgressSink {
    let app = app.clone();
    let diagnostics = store.diagnostics().clone();
    Arc::new(move |progress| {
        if let Err(error) = app.emit("update-install-progress", progress) {
            record_event_delivery_failure(
                &diagnostics,
                "update-install-progress",
                &error.to_string(),
            );
        }
    })
}

pub(crate) fn installer_actions(app: &AppHandle) -> InstallerActions {
    let app = app.clone();
    InstallerActions {
        launch: Arc::new(|handoff| {
            std::process::Command::new(handoff.installer_path())
                .args(["/S", "/R"])
                .spawn()
                .map(|_| ())
                .map_err(|error| {
                    AppError::new(
                        "installer_launch_failed",
                        "The verified installer could not be started.",
                    )
                    .with_detail(error.to_string())
                    .retryable(true)
                })
        }),
        exit: Arc::new(move || app.exit(0)),
    }
}
pub(crate) fn spawn_state_events(
    app: &tauri::AppHandle,
    store: &StateStore,
    manager: &DownloadManager,
) -> Result<(), AppError> {
    let reader = store.take_outbox_reader()?;
    let app = app.clone();
    let store = store.clone();
    let coordinator = manager.clone();
    manager.spawn_tracked(TrackedTaskKind::Events, async move {
        crate::state_events::publish_state_events(&store, &coordinator, reader, |publication| {
            match publication {
                StatePublication::Deltas(deltas) => {
                    for delta in deltas.iter() {
                        app.emit("app-state-changed", delta)
                            .map_err(|error| error.to_string())?;
                    }
                    Ok(())
                }
                StatePublication::ResyncRequired(required) => app
                    .emit("app-state-resync-required", required)
                    .map_err(|error| error.to_string()),
            }
        })
        .await;
    })
}

#[cfg(windows)]
pub(crate) fn report_startup_failure(error: &AppError) {
    use std::os::windows::ffi::OsStrExt;
    const MB_ICONERROR: u32 = 0x10;
    const MB_OK: u32 = 0;
    #[link(name = "User32")]
    extern "system" {
        fn MessageBoxW(window: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    let text = std::ffi::OsStr::new(&format!(
        "{}\n\nThe existing application data was left unchanged.\nCorrelation ID: {}",
        error.summary, error.correlation_id
    ))
    .encode_wide()
    .chain(Some(0))
    .collect::<Vec<_>>();
    let caption = std::ffi::OsStr::new("Nuclear Downloader could not start")
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        MessageBoxW(0, text.as_ptr(), caption.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
pub(crate) fn report_startup_failure(error: &AppError) {
    eprintln!(
        "Nuclear Downloader could not start: {} ({})",
        error.summary, error.correlation_id
    );
}
