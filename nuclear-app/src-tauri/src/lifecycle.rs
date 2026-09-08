mod admission;
mod capacity;
mod publication;
mod task_tracking;

use crate::app_error::AppError;
use crate::downloader::process::DownloadJob;
use crate::models::OperationKind;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as SyncMutex};
use tokio::sync::{Mutex, Notify, OwnedMutexGuard, Semaphore};
use tokio_util::sync::CancellationToken;

pub(crate) const DOWNLOAD_CAPACITY: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleMode {
    Starting,
    Accepting,
    Paused(u64),
    Draining(u64),
    ShuttingDown,
}

struct LifecycleState {
    mode: LifecycleMode,
    jobs: HashMap<String, DownloadJob>,
    next_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum TrackedTaskKind {
    Startup,
    Admission,
    Inspection,
    Health,
    Network,
    Drain,
    Update,
    Worker,
    Cleanup,
    Events,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublicationKind {
    RuntimeCommit,
    InstallerCache,
    InstallerHandoff,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DrainTicket(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DrainCompletion {
    Idle,
    SupersededByShutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UpdateCancellation {
    Requested,
    DeferredPublication,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TrackedTaskDiagnostic {
    pub(crate) id: u64,
    pub(crate) kind: TrackedTaskKind,
    pub(crate) operation_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TrackedTaskTimeout {
    pub(crate) outstanding: Vec<TrackedTaskDiagnostic>,
}

impl Display for TrackedTaskTimeout {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Timed out while waiting for {} backend task(s) to finish.",
            self.outstanding.len()
        )
    }
}

impl std::error::Error for TrackedTaskTimeout {}

#[derive(Debug)]
pub(crate) enum UpdateRunError {
    Cancelled,
    Failed(AppError),
}

impl Display for UpdateRunError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("The update was cancelled."),
            Self::Failed(error) => Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for UpdateRunError {}

impl From<AppError> for UpdateRunError {
    fn from(error: AppError) -> Self {
        Self::Failed(error)
    }
}

impl From<String> for UpdateRunError {
    fn from(summary: String) -> Self {
        Self::Failed(AppError::internal(summary))
    }
}

impl From<&str> for UpdateRunError {
    fn from(summary: &str) -> Self {
        Self::Failed(AppError::internal(summary))
    }
}

struct TaskRecord {
    kind: TrackedTaskKind,
    operation_id: Option<String>,
}

struct UpdateRecord {
    task_id: u64,
    cancel: CancellationToken,
    publications: usize,
    published: bool,
    cancel_requested: bool,
    task_active: bool,
}

struct TaskRegistry {
    shutting_down: bool,
    next_id: u64,
    tasks: HashMap<u64, TaskRecord>,
    updates: HashMap<String, UpdateRecord>,
    active_publications: usize,
}

#[derive(Clone)]
enum StartupOutcome {
    NotStarted,
    Running,
    Complete(Result<(), AppError>),
}

struct LifecycleInner {
    transition: Arc<Mutex<()>>,
    state: Mutex<LifecycleState>,
    mode_changed: Notify,
    idle: Notify,
    download_slots: Arc<Semaphore>,
    inspection_slots: Arc<Semaphore>,
    conversion_slots: Arc<Semaphore>,
    expected_workers: usize,
    shutdown_started: AtomicBool,
    installer_handoff_committed: AtomicBool,
    shutdown: CancellationToken,
    registry: SyncMutex<TaskRegistry>,
    tasks_changed: Notify,
    startup_outcome: SyncMutex<StartupOutcome>,
    startup_changed: Notify,
}

#[derive(Clone)]
pub(crate) struct LifecycleCoordinator {
    inner: Arc<LifecycleInner>,
}

pub(crate) type DownloadManager = LifecycleCoordinator;

pub(crate) struct JobAdmission {
    lifecycle: LifecycleCoordinator,
    _transition: OwnedMutexGuard<()>,
    jobs: Vec<DownloadJob>,
}

pub(crate) struct WorkerClaim {
    lifecycle: LifecycleCoordinator,
    _transition: OwnedMutexGuard<()>,
}

pub(crate) struct MaintenanceLease {
    lifecycle: LifecycleCoordinator,
    generation: u64,
    active: bool,
}

struct TrackedTaskGuard {
    lifecycle: LifecycleCoordinator,
    id: u64,
}

pub(crate) struct TrackedUpdate {
    _task: TrackedTaskGuard,
}

#[derive(Clone)]
pub(crate) struct UpdateTaskContext {
    operation_id: String,
    kind: OperationKind,
    cancel: CancellationToken,
    lifecycle: LifecycleCoordinator,
}

pub(crate) struct PublicationGuard {
    lifecycle: LifecycleCoordinator,
    operation_id: String,
    kind: PublicationKind,
    active: bool,
}

impl LifecycleCoordinator {
    pub(crate) fn new(
        max_downloads: usize,
        max_inspections: usize,
        max_conversions: usize,
    ) -> Self {
        let expected_workers = max_downloads.max(1);
        Self {
            inner: Arc::new(LifecycleInner {
                transition: Arc::new(Mutex::new(())),
                state: Mutex::new(LifecycleState {
                    mode: LifecycleMode::Starting,
                    jobs: HashMap::new(),
                    next_generation: 1,
                }),
                mode_changed: Notify::new(),
                idle: Notify::new(),
                download_slots: Arc::new(Semaphore::new(expected_workers)),
                inspection_slots: Arc::new(Semaphore::new(max_inspections.max(1))),
                conversion_slots: Arc::new(Semaphore::new(max_conversions.max(1))),
                expected_workers,
                shutdown_started: AtomicBool::new(false),
                installer_handoff_committed: AtomicBool::new(false),
                shutdown: CancellationToken::new(),
                registry: SyncMutex::new(TaskRegistry {
                    shutting_down: false,
                    next_id: 1,
                    tasks: HashMap::new(),
                    updates: HashMap::new(),
                    active_publications: 0,
                }),
                tasks_changed: Notify::new(),
                startup_outcome: SyncMutex::new(StartupOutcome::NotStarted),
                startup_changed: Notify::new(),
            }),
        }
    }

    pub(crate) async fn wait_for_startup(&self) -> Result<(), AppError> {
        loop {
            let changed = self.inner.startup_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let outcome = self
                .inner
                .startup_outcome
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            if let StartupOutcome::Complete(result) = outcome {
                return result;
            }
            if self.inner.shutdown_started.load(Ordering::SeqCst) {
                return Err(AppError::new(
                    "shutting_down",
                    "The application is shutting down.",
                ));
            }
            changed.await;
        }
    }

    pub(crate) async fn open_after_startup(&self) -> Result<(), AppError> {
        let _transition = self.inner.transition.clone().lock_owned().await;
        if self
            .inner
            .installer_handoff_committed
            .load(Ordering::SeqCst)
        {
            return Err(AppError::busy("An application update is ready to launch."));
        }
        let workers = {
            let registry = self.registry()?;
            registry
                .tasks
                .values()
                .filter(|task| task.kind == TrackedTaskKind::Worker)
                .count()
        };
        if workers != self.inner.expected_workers {
            return Err(AppError::internal(
                "The fixed download worker pool was not fully published.",
            ));
        }
        let mut state = self.inner.state.lock().await;
        if state.mode != LifecycleMode::Starting {
            return Err(AppError::busy(
                "Application startup admission was already resolved.",
            ));
        }
        if self.inner.shutdown_started.load(Ordering::SeqCst) {
            return Err(AppError::busy("The application is shutting down."));
        }
        state.mode = LifecycleMode::Accepting;
        drop(state);
        self.inner.mode_changed.notify_waiters();
        Ok(())
    }

    pub(crate) async fn begin_shutdown(&self) {
        if !self.inner.shutdown_started.swap(true, Ordering::SeqCst) {
            let tokens = {
                let mut tokens = Vec::new();
                if let Ok(mut registry) = self.registry() {
                    registry.shutting_down = true;
                    for update in registry.updates.values_mut() {
                        update.cancel_requested = true;
                        if update.publications == 0 && !update.published {
                            tokens.push(update.cancel.clone());
                        }
                    }
                }
                tokens
            };
            self.inner.shutdown.cancel();
            self.inner.startup_changed.notify_waiters();
            for token in tokens {
                token.cancel();
            }
        }
        let jobs = {
            let mut state = self.inner.state.lock().await;
            state.mode = LifecycleMode::ShuttingDown;
            state.jobs.values().cloned().collect::<Vec<_>>()
        };
        for job in jobs {
            job.cancel();
        }
        self.inner.mode_changed.notify_waiters();
        self.inner.tasks_changed.notify_waiters();
    }

    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.inner.shutdown.clone()
    }
}

pub(crate) fn create_download_manager() -> DownloadManager {
    LifecycleCoordinator::new(DOWNLOAD_CAPACITY, 1, 1)
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests;
