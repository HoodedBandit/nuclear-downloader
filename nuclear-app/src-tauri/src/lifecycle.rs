use crate::app_error::AppError;
use crate::downloader::process::DownloadJob;
use crate::models::OperationKind;
use futures_util::FutureExt;
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as SyncMutex};
use std::time::Duration;
use tokio::sync::{Mutex, Notify, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

const MAX_REGISTERED_JOBS: usize = 1_000;

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

    #[cfg(test)]
    pub(crate) async fn open_for_test(&self) {
        let _transition = self.inner.transition.clone().lock_owned().await;
        let mut state = self.inner.state.lock().await;
        assert_eq!(state.mode, LifecycleMode::Starting);
        state.mode = LifecycleMode::Accepting;
        drop(state);
        self.inner.mode_changed.notify_waiters();
    }

    pub(crate) async fn begin_job_admission(&self, count: usize) -> Result<JobAdmission, AppError> {
        if count == 0 {
            return Err(AppError::invalid(
                "At least one operation must be admitted.",
            ));
        }
        if self.inner.shutdown_started.load(Ordering::SeqCst) {
            return Err(AppError::busy("The application is shutting down."));
        }
        if self
            .inner
            .installer_handoff_committed
            .load(Ordering::SeqCst)
        {
            return Err(AppError::busy("An application update is ready to launch."));
        }
        let transition = self.inner.transition.clone().lock_owned().await;
        if self
            .inner
            .installer_handoff_committed
            .load(Ordering::SeqCst)
        {
            return Err(AppError::busy("An application update is ready to launch."));
        }
        let state = self.inner.state.lock().await;
        if state.mode != LifecycleMode::Accepting {
            return Err(AppError::busy(
                "New work is paused during startup, maintenance, cancellation, or shutdown.",
            ));
        }
        if state.jobs.len().saturating_add(count) > MAX_REGISTERED_JOBS {
            return Err(AppError::busy(
                "The operation queue is limited to 1,000 items.",
            ));
        }
        drop(state);
        let jobs = (0..count)
            .map(|_| DownloadJob::new().map_err(AppError::internal))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(JobAdmission {
            lifecycle: self.clone(),
            _transition: transition,
            jobs,
        })
    }

    #[cfg(test)]
    pub(crate) async fn register(&self, id: &str) -> Result<DownloadJob, String> {
        let admission = self
            .begin_job_admission(1)
            .await
            .map_err(|error| error.summary)?;
        let mut jobs = admission
            .publish(&[id.to_string()])
            .await
            .map_err(|error| error.summary)?;
        Ok(jobs.remove(0))
    }

    pub(crate) async fn wait_worker_claim(&self) -> Result<Option<WorkerClaim>, String> {
        loop {
            if self
                .inner
                .installer_handoff_committed
                .load(Ordering::SeqCst)
            {
                return Ok(None);
            }
            let changed = self.inner.mode_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let transition = self.inner.transition.clone().lock_owned().await;
            if self
                .inner
                .installer_handoff_committed
                .load(Ordering::SeqCst)
            {
                return Ok(None);
            }
            let mode = self.inner.state.lock().await.mode;
            match mode {
                LifecycleMode::Accepting => {
                    return Ok(Some(WorkerClaim {
                        lifecycle: self.clone(),
                        _transition: transition,
                    }));
                }
                LifecycleMode::ShuttingDown => return Ok(None),
                LifecycleMode::Starting | LifecycleMode::Paused(_) | LifecycleMode::Draining(_) => {
                    drop(transition);
                    changed.await;
                }
            }
        }
    }

    pub(crate) async fn cancel(&self, id: &str) -> Result<(), AppError> {
        if let Some(job) = self.inner.state.lock().await.jobs.get(id).cloned() {
            job.cancel();
            return Ok(());
        }

        let _transition = self.inner.transition.clone().lock_owned().await;
        let job = self.inner.state.lock().await.jobs.get(id).cloned();
        if let Some(job) = job {
            job.cancel();
            Ok(())
        } else {
            self.cancel_update(id).map(|_| ())
        }
    }

    pub(crate) async fn finish(&self, id: &str) {
        let removed = self.inner.state.lock().await.jobs.remove(id).is_some();
        if removed {
            self.inner.idle.notify_waiters();
        }
    }

    pub(crate) async fn active_ids(&self) -> Vec<String> {
        let mut ids = self
            .inner
            .state
            .lock()
            .await
            .jobs
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    #[cfg(test)]
    pub(crate) async fn active_count(&self) -> usize {
        self.inner.state.lock().await.jobs.len()
    }

    pub(crate) async fn acquire_maintenance(&self) -> Result<MaintenanceLease, AppError> {
        let _transition = self.inner.transition.clone().lock_owned().await;
        if self
            .inner
            .installer_handoff_committed
            .load(Ordering::SeqCst)
        {
            return Err(AppError::busy("An application update is ready to launch."));
        }
        let mut state = self.inner.state.lock().await;
        if state.mode != LifecycleMode::Accepting {
            return Err(AppError::busy(
                "Downloader maintenance is already in progress.",
            ));
        }
        if !state.jobs.is_empty() {
            return Err(AppError::busy(
                "Finish or cancel active operations before installing updates.",
            ));
        }
        let generation = state.next_generation;
        state.next_generation = state.next_generation.wrapping_add(1).max(1);
        state.mode = LifecycleMode::Paused(generation);
        Ok(MaintenanceLease {
            lifecycle: self.clone(),
            generation,
            active: true,
        })
    }

    pub(crate) async fn begin_cancel_all(&self) -> Result<DrainTicket, AppError> {
        let _transition = self.inner.transition.clone().lock_owned().await;
        let jobs = {
            let mut state = self.inner.state.lock().await;
            match state.mode {
                LifecycleMode::ShuttingDown => {
                    return Err(AppError::busy("The application is shutting down."));
                }
                LifecycleMode::Starting => {
                    return Err(AppError::busy("Application startup is still in progress."));
                }
                LifecycleMode::Paused(_) => {
                    return Err(AppError::busy(
                        "Cancellation cannot start while update maintenance is active.",
                    ));
                }
                LifecycleMode::Draining(_) => {
                    return Err(AppError::busy("Cancellation is already in progress."));
                }
                LifecycleMode::Accepting => {}
            }
            let generation = state.next_generation;
            state.next_generation = state.next_generation.wrapping_add(1).max(1);
            state.mode = LifecycleMode::Draining(generation);
            (
                DrainTicket(generation),
                state.jobs.values().cloned().collect::<Vec<_>>(),
            )
        };
        for job in jobs.1 {
            job.cancel();
        }
        self.inner.mode_changed.notify_waiters();
        Ok(jobs.0)
    }

    pub(crate) async fn wait_for_drain(&self, ticket: DrainTicket) -> DrainCompletion {
        loop {
            let idle = self.inner.idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            let state = self.inner.state.lock().await;
            match state.mode {
                LifecycleMode::Draining(generation) if generation == ticket.0 => {
                    if state.jobs.is_empty() {
                        return DrainCompletion::Idle;
                    }
                }
                LifecycleMode::ShuttingDown => return DrainCompletion::SupersededByShutdown,
                _ => return DrainCompletion::SupersededByShutdown,
            }
            drop(state);
            idle.await;
        }
    }

    pub(crate) async fn owns_drain(&self, ticket: DrainTicket) -> bool {
        self.inner.state.lock().await.mode == LifecycleMode::Draining(ticket.0)
    }

    pub(crate) async fn resume_after_drain(&self, ticket: DrainTicket) -> bool {
        let _transition = self.inner.transition.clone().lock_owned().await;
        let mut state = self.inner.state.lock().await;
        let can_resume = state.mode == LifecycleMode::Draining(ticket.0)
            && state.jobs.is_empty()
            && !self
                .inner
                .installer_handoff_committed
                .load(Ordering::SeqCst);
        if can_resume {
            state.mode = LifecycleMode::Accepting;
        }
        drop(state);
        if can_resume {
            self.inner.mode_changed.notify_waiters();
        }
        can_resume
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

    pub(crate) fn spawn_tracked<F>(&self, kind: TrackedTaskKind, future: F) -> Result<(), AppError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let guard = self.register_task(kind, None, false)?;
        tauri::async_runtime::spawn(async move {
            let _guard = guard;
            future.await;
        });
        Ok(())
    }

    pub(crate) fn spawn_startup<F, C>(&self, future: F, on_complete: C) -> Result<(), AppError>
    where
        F: Future<Output = Result<(), AppError>> + Send + 'static,
        C: FnOnce(Result<(), AppError>) + Send + 'static,
    {
        let lifecycle = self.clone();
        let guard = self.register_task(TrackedTaskKind::Startup, None, false)?;
        {
            let mut outcome = self
                .inner
                .startup_outcome
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !matches!(*outcome, StartupOutcome::NotStarted) {
                return Err(AppError::busy(
                    "Application startup cleanup was already started.",
                ));
            }
            *outcome = StartupOutcome::Running;
        }
        tauri::async_runtime::spawn(async move {
            let _guard = guard;
            let result = match AssertUnwindSafe(future).catch_unwind().await {
                Ok(Ok(())) => lifecycle.open_after_startup().await,
                Ok(Err(error)) => Err(error),
                Err(_) => Err(AppError::internal(
                    "The startup cleanup task stopped unexpectedly.",
                )),
            };
            *lifecycle
                .inner
                .startup_outcome
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                StartupOutcome::Complete(result.clone());
            lifecycle.inner.startup_changed.notify_waiters();
            on_complete(result);
        });
        Ok(())
    }

    pub(crate) fn spawn_cleanup_continuation<F>(&self, future: F) -> Result<(), AppError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let guard = self.register_task(TrackedTaskKind::Cleanup, None, true)?;
        tauri::async_runtime::spawn(async move {
            let _guard = guard;
            future.await;
        });
        Ok(())
    }

    pub(crate) fn register_update(
        &self,
        operation_id: String,
        kind: OperationKind,
    ) -> Result<(TrackedUpdate, UpdateTaskContext), AppError> {
        let cancel = CancellationToken::new();
        let id = {
            let mut registry = self.registry()?;
            if registry.shutting_down
                || self
                    .inner
                    .installer_handoff_committed
                    .load(Ordering::SeqCst)
            {
                return Err(AppError::busy(
                    "The application is shutting down or launching an update.",
                ));
            }
            if registry.updates.contains_key(&operation_id) {
                return Err(AppError::busy(
                    "The update operation is already registered.",
                ));
            }
            let id = registry.next_id;
            registry.next_id = registry.next_id.wrapping_add(1).max(1);
            registry.tasks.insert(
                id,
                TaskRecord {
                    kind: TrackedTaskKind::Update,
                    operation_id: Some(operation_id.clone()),
                },
            );
            registry.updates.insert(
                operation_id.clone(),
                UpdateRecord {
                    task_id: id,
                    cancel: cancel.clone(),
                    publications: 0,
                    published: false,
                    cancel_requested: false,
                    task_active: true,
                },
            );
            id
        };
        let context = UpdateTaskContext {
            operation_id,
            kind,
            cancel,
            lifecycle: self.clone(),
        };
        Ok((
            TrackedUpdate {
                _task: TrackedTaskGuard {
                    lifecycle: self.clone(),
                    id,
                },
            },
            context,
        ))
    }

    pub(crate) fn cancel_update(&self, operation_id: &str) -> Result<UpdateCancellation, AppError> {
        let (token, outcome) = {
            let mut registry = self.registry()?;
            let update = registry
                .updates
                .get_mut(operation_id)
                .ok_or_else(|| AppError::not_found("update operation"))?;
            update.cancel_requested = true;
            if update.publications == 0 && !update.published {
                (Some(update.cancel.clone()), UpdateCancellation::Requested)
            } else {
                (None, UpdateCancellation::DeferredPublication)
            }
        };
        if let Some(token) = token {
            token.cancel();
        }
        Ok(outcome)
    }

    pub(crate) async fn wait_for_shutdown_tasks(
        &self,
        grace: Duration,
    ) -> Result<(), TrackedTaskTimeout> {
        if tokio::time::timeout(grace, self.wait_for_all_tasks())
            .await
            .is_ok()
        {
            return Ok(());
        }
        loop {
            let changed = self.inner.tasks_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let publications = self
                .registry()
                .map(|registry| registry.active_publications)
                .unwrap_or(0);
            if publications == 0 {
                break;
            }
            changed.await;
        }
        tokio::task::yield_now().await;
        let outstanding = self.task_diagnostics();
        if outstanding.is_empty() {
            Ok(())
        } else {
            Err(TrackedTaskTimeout { outstanding })
        }
    }

    pub(crate) async fn wait_for_all_producers_done_on_shutdown(&self) {
        loop {
            let changed = self.inner.tasks_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let producers_done = self
                .registry()
                .map(|registry| {
                    registry
                        .tasks
                        .values()
                        .all(|task| task.kind == TrackedTaskKind::Events)
                })
                .unwrap_or(true);
            if producers_done {
                return;
            }
            changed.await;
        }
    }

    pub(crate) async fn acquire_download_slot(&self) -> Result<OwnedSemaphorePermit, String> {
        if self.inner.shutdown.is_cancelled() {
            return Err("Download scheduler is shutting down.".to_string());
        }
        tokio::select! {
            biased;
            _ = self.inner.shutdown.cancelled() => {
                Err("Download scheduler is shutting down.".to_string())
            }
            permit = self.inner.download_slots.clone().acquire_owned() => {
                permit.map_err(|_| "Download scheduler is unavailable.".to_string())
            }
        }
    }

    pub(crate) async fn acquire_inspection(
        &self,
        job: &DownloadJob,
    ) -> Result<Option<OwnedSemaphorePermit>, String> {
        if job.is_cancelled() || self.inner.shutdown.is_cancelled() {
            return Ok(None);
        }
        tokio::select! {
            biased;
            _ = job.cancelled() => Ok(None),
            _ = self.inner.shutdown.cancelled() => Ok(None),
            permit = self.inner.inspection_slots.clone().acquire_owned() => {
                permit.map(Some).map_err(|_| "Inspection scheduler is unavailable.".to_string())
            }
        }
    }

    pub(crate) async fn acquire_conversion(
        &self,
        job: &DownloadJob,
    ) -> Result<Option<OwnedSemaphorePermit>, String> {
        if job.is_cancelled() || self.inner.shutdown.is_cancelled() {
            return Ok(None);
        }
        tokio::select! {
            biased;
            _ = job.cancelled() => Ok(None),
            _ = self.inner.shutdown.cancelled() => Ok(None),
            permit = self.inner.conversion_slots.clone().acquire_owned() => {
                permit.map(Some).map_err(|_| "WebM conversion scheduler is unavailable.".to_string())
            }
        }
    }

    fn register_task(
        &self,
        kind: TrackedTaskKind,
        operation_id: Option<String>,
        allow_during_shutdown: bool,
    ) -> Result<TrackedTaskGuard, AppError> {
        let id = {
            let mut registry = self.registry()?;
            if registry.shutting_down && !allow_during_shutdown {
                return Err(AppError::busy("The application is shutting down."));
            }
            let id = registry.next_id;
            registry.next_id = registry.next_id.wrapping_add(1).max(1);
            registry.tasks.insert(id, TaskRecord { kind, operation_id });
            id
        };
        Ok(TrackedTaskGuard {
            lifecycle: self.clone(),
            id,
        })
    }

    async fn wait_for_all_tasks(&self) {
        loop {
            let changed = self.inner.tasks_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let empty = self
                .registry()
                .map(|registry| registry.tasks.is_empty())
                .unwrap_or(true);
            if empty {
                return;
            }
            changed.await;
        }
    }

    fn task_diagnostics(&self) -> Vec<TrackedTaskDiagnostic> {
        let Ok(registry) = self.registry() else {
            return Vec::new();
        };
        let mut tasks = registry
            .tasks
            .iter()
            .map(|(id, task)| TrackedTaskDiagnostic {
                id: *id,
                kind: task.kind,
                operation_id: task.operation_id.clone(),
            })
            .collect::<Vec<_>>();
        tasks.sort_by_key(|task| task.id);
        tasks
    }

    fn registry(&self) -> Result<std::sync::MutexGuard<'_, TaskRegistry>, AppError> {
        self.inner
            .registry
            .lock()
            .map_err(|_| AppError::internal("The backend task registry is unavailable."))
    }
}

impl JobAdmission {
    pub(crate) async fn publish(mut self, ids: &[String]) -> Result<Vec<DownloadJob>, AppError> {
        if ids.len() != self.jobs.len() {
            return Err(AppError::internal(
                "The durable operation batch did not match its job reservation.",
            ));
        }
        let unique = ids.iter().collect::<HashSet<_>>();
        if unique.len() != ids.len() {
            return Err(AppError::invalid(
                "An operation ID was registered more than once.",
            ));
        }
        let mut state = self.lifecycle.inner.state.lock().await;
        if state.mode == LifecycleMode::ShuttingDown {
            return Err(AppError::new(
                "shutting_down",
                "The application is shutting down.",
            ));
        }
        if self
            .lifecycle
            .inner
            .installer_handoff_committed
            .load(Ordering::SeqCst)
        {
            return Err(AppError::busy("An application update is ready to launch."));
        }
        if state.mode != LifecycleMode::Accepting {
            return Err(AppError::busy("New work is no longer being accepted."));
        }
        if ids.iter().any(|id| state.jobs.contains_key(id)) {
            return Err(AppError::busy(
                "An operation with this ID is already active.",
            ));
        }
        let jobs = std::mem::take(&mut self.jobs);
        for (id, job) in ids.iter().cloned().zip(jobs.iter().cloned()) {
            state.jobs.insert(id, job);
        }
        Ok(jobs)
    }
}

impl WorkerClaim {
    pub(crate) async fn registered_job(&self, id: &str) -> Result<DownloadJob, AppError> {
        let state = self.lifecycle.inner.state.lock().await;
        if self
            .lifecycle
            .inner
            .installer_handoff_committed
            .load(Ordering::SeqCst)
        {
            return Err(AppError::busy("An application update is ready to launch."));
        }
        if !matches!(
            state.mode,
            LifecycleMode::Accepting | LifecycleMode::ShuttingDown
        ) {
            return Err(AppError::busy("New work is no longer being accepted."));
        }
        let job = state
            .jobs
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::not_found("operation job"))?;
        if state.mode == LifecycleMode::ShuttingDown {
            job.cancel();
        }
        Ok(job)
    }
}

impl MaintenanceLease {
    pub(crate) async fn release(mut self) {
        self.release_inner().await;
    }

    async fn release_inner(&mut self) {
        if !self.active {
            return;
        }
        let _transition = self.lifecycle.inner.transition.clone().lock_owned().await;
        let mut state = self.lifecycle.inner.state.lock().await;
        if state.mode == LifecycleMode::Paused(self.generation)
            && !self
                .lifecycle
                .inner
                .installer_handoff_committed
                .load(Ordering::SeqCst)
        {
            state.mode = LifecycleMode::Accepting;
        }
        self.active = false;
        drop(state);
        self.lifecycle.inner.mode_changed.notify_waiters();
    }
}

impl Drop for MaintenanceLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let lifecycle = self.lifecycle.clone();
        let generation = self.generation;
        self.active = false;
        let continuation = lifecycle.clone();
        let _ = lifecycle.spawn_cleanup_continuation(async move {
            let _transition = continuation.inner.transition.clone().lock_owned().await;
            let mut state = continuation.inner.state.lock().await;
            if state.mode == LifecycleMode::Paused(generation)
                && !continuation
                    .inner
                    .installer_handoff_committed
                    .load(Ordering::SeqCst)
            {
                state.mode = LifecycleMode::Accepting;
            }
            drop(state);
            continuation.inner.mode_changed.notify_waiters();
        });
    }
}

impl UpdateTaskContext {
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub(crate) async fn cancelled(&self) {
        self.cancel.cancelled().await;
    }

    pub(crate) fn check_cancelled(&self) -> Result<(), UpdateRunError> {
        if self.is_cancelled() {
            Err(UpdateRunError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub(crate) fn enter_publication(
        &self,
        kind: PublicationKind,
    ) -> Result<PublicationGuard, UpdateRunError> {
        let valid_kind = matches!(
            (self.kind, kind),
            (OperationKind::RuntimeUpdate, PublicationKind::RuntimeCommit)
                | (OperationKind::AppUpdate, PublicationKind::InstallerCache)
                | (OperationKind::AppUpdate, PublicationKind::InstallerHandoff)
        );
        if !valid_kind {
            return Err(UpdateRunError::Failed(AppError::internal(
                "The update attempted an invalid publication phase.",
            )));
        }
        let mut registry = self.lifecycle.registry().map_err(UpdateRunError::Failed)?;
        let update = registry
            .updates
            .get_mut(&self.operation_id)
            .ok_or_else(|| UpdateRunError::Failed(AppError::not_found("update task")))?;
        if update.cancel_requested || self.cancel.is_cancelled() {
            return Err(UpdateRunError::Cancelled);
        }
        update.publications = update.publications.saturating_add(1);
        registry.active_publications = registry.active_publications.saturating_add(1);
        Ok(PublicationGuard {
            lifecycle: self.lifecycle.clone(),
            operation_id: self.operation_id.clone(),
            kind,
            active: true,
        })
    }
}

impl PublicationGuard {
    pub(crate) fn commit(mut self) {
        if self.kind == PublicationKind::InstallerHandoff {
            self.lifecycle
                .inner
                .installer_handoff_committed
                .store(true, Ordering::SeqCst);
            self.lifecycle.inner.mode_changed.notify_waiters();
        }
        if matches!(
            self.kind,
            PublicationKind::RuntimeCommit | PublicationKind::InstallerHandoff
        ) {
            if let Ok(mut registry) = self.lifecycle.registry() {
                if let Some(update) = registry.updates.get_mut(&self.operation_id) {
                    update.published = true;
                }
            }
        }
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if !self.active {
            return;
        }
        let mut cancel = None;
        if let Ok(mut registry) = self.lifecycle.registry() {
            registry.active_publications = registry.active_publications.saturating_sub(1);
            let mut remove_update = false;
            if let Some(update) = registry.updates.get_mut(&self.operation_id) {
                update.publications = update.publications.saturating_sub(1);
                if update.publications == 0 && update.cancel_requested && !update.published {
                    cancel = Some(update.cancel.clone());
                }
                remove_update = update.publications == 0 && !update.task_active;
            }
            if remove_update {
                registry.updates.remove(&self.operation_id);
            }
        }
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        self.active = false;
        self.lifecycle.inner.tasks_changed.notify_waiters();
    }
}

impl Drop for PublicationGuard {
    fn drop(&mut self) {
        self.release_inner();
    }
}

impl Drop for TrackedTaskGuard {
    fn drop(&mut self) {
        if let Ok(mut registry) = self.lifecycle.registry() {
            let operation_id = registry
                .tasks
                .remove(&self.id)
                .and_then(|task| task.operation_id);
            if let Some(operation_id) = operation_id {
                let mut remove_update = false;
                if let Some(update) = registry.updates.get_mut(&operation_id) {
                    if update.task_id == self.id {
                        update.task_active = false;
                        remove_update = update.publications == 0;
                    }
                }
                if remove_update {
                    registry.updates.remove(&operation_id);
                }
            }
        }
        self.lifecycle.inner.tasks_changed.notify_waiters();
    }
}

pub(crate) fn create_download_manager() -> DownloadManager {
    LifecycleCoordinator::new(5, 1, 1)
}

#[cfg(test)]
mod tests {
    use super::{
        DrainCompletion, LifecycleCoordinator, PublicationKind, TrackedTaskKind, UpdateCancellation,
    };
    use crate::app_error::AppError;
    use crate::models::OperationKind;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::{Barrier, Notify};

    #[tokio::test]
    async fn startup_gate_blocks_admission_and_worker_claim_until_open() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        assert!(lifecycle.begin_job_admission(1).await.is_err());

        let worker_guard = lifecycle
            .register_task(TrackedTaskKind::Worker, None, false)
            .unwrap();
        let claim_lifecycle = lifecycle.clone();
        let claim = tokio::spawn(async move { claim_lifecycle.wait_worker_claim().await.unwrap() });
        tokio::task::yield_now().await;
        assert!(!claim.is_finished());

        lifecycle.open_after_startup().await.unwrap();
        assert!(claim.await.unwrap().is_some());
        drop(worker_guard);
    }

    #[tokio::test]
    async fn tracked_startup_task_opens_admission_before_it_unregisters() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        let shutdown = lifecycle.shutdown_token();
        lifecycle
            .spawn_tracked(TrackedTaskKind::Worker, async move {
                shutdown.cancelled().await;
            })
            .unwrap();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        lifecycle
            .spawn_startup(async { Ok(()) }, move |result| {
                let _ = sender.send(result);
            })
            .unwrap();

        receiver.await.unwrap().unwrap();
        let job = lifecycle.register("after-startup").await.unwrap();
        assert!(!job.is_cancelled());
        lifecycle.finish("after-startup").await;
        lifecycle.begin_shutdown().await;
        lifecycle
            .wait_for_shutdown_tasks(Duration::from_secs(1))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn startup_waiter_blocks_during_cleanup_and_receives_failure() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let (finish_sender, finish_receiver) = tokio::sync::oneshot::channel();
        lifecycle
            .spawn_startup(
                async move {
                    let _ = started_sender.send(());
                    let _ = finish_receiver.await;
                    Err(AppError::internal("Startup recovery failed."))
                },
                |_| {},
            )
            .unwrap();
        started_receiver.await.unwrap();

        let waiting_lifecycle = lifecycle.clone();
        let waiter = tokio::spawn(async move { waiting_lifecycle.wait_for_startup().await });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());

        finish_sender.send(()).unwrap();
        let error = waiter.await.unwrap().unwrap_err();
        assert_eq!(error.code, "internal_error");
        assert_eq!(error.summary, "Startup recovery failed.");
    }

    #[tokio::test]
    async fn startup_waiter_returns_typed_shutdown_error() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.begin_shutdown().await;

        let error = lifecycle.wait_for_startup().await.unwrap_err();
        assert_eq!(error.code, "shutting_down");
    }

    #[tokio::test]
    async fn shutdown_during_startup_never_reopens_admission() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        let worker_guard = lifecycle
            .register_task(TrackedTaskKind::Worker, None, false)
            .unwrap();

        lifecycle.begin_shutdown().await;
        assert!(lifecycle.open_after_startup().await.is_err());
        assert!(lifecycle.begin_job_admission(1).await.is_err());
        assert!(lifecycle.wait_worker_claim().await.unwrap().is_none());
        drop(worker_guard);
    }

    #[tokio::test]
    async fn cancellation_waits_for_atomic_job_publication() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;
        let admission = lifecycle.begin_job_admission(1).await.unwrap();
        let operation_id = "operation".to_string();
        let cancel_started = Arc::new(Notify::new());
        let cancel_lifecycle = lifecycle.clone();
        let cancel_id = operation_id.clone();
        let cancel_signal = cancel_started.clone();
        let cancellation = tokio::spawn(async move {
            cancel_signal.notify_one();
            cancel_lifecycle.cancel(&cancel_id).await
        });
        cancel_started.notified().await;
        tokio::task::yield_now().await;
        assert!(!cancellation.is_finished());

        let jobs = admission
            .publish(std::slice::from_ref(&operation_id))
            .await
            .unwrap();
        cancellation.await.unwrap().unwrap();
        assert!(jobs[0].is_cancelled());
        lifecycle.finish(&operation_id).await;
        assert_eq!(lifecycle.active_count().await, 0);
    }

    #[tokio::test]
    async fn cancellation_signals_a_registered_job_while_worker_claim_is_held() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;
        let job = lifecycle.register("dequeued").await.unwrap();
        let claim = lifecycle.wait_worker_claim().await.unwrap().unwrap();

        tokio::time::timeout(Duration::from_millis(100), lifecycle.cancel("dequeued"))
            .await
            .expect("registered job cancellation must not wait for the worker claim")
            .unwrap();
        assert!(job.is_cancelled());

        drop(claim);
        lifecycle.finish("dequeued").await;
    }

    #[tokio::test]
    async fn shutdown_closes_immediately_while_admission_owns_transition_gate() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;
        let admission = lifecycle.begin_job_admission(1).await.unwrap();

        tokio::time::timeout(Duration::from_millis(100), lifecycle.begin_shutdown())
            .await
            .expect("shutdown must not wait for a durable admission commit");
        let error = admission
            .publish(&["committed-during-shutdown".to_string()])
            .await
            .err()
            .expect("publication must fail after shutdown starts");
        assert_eq!(error.code, "shutting_down");
        assert_eq!(lifecycle.active_count().await, 0);
    }

    #[tokio::test]
    async fn shutdown_after_worker_claim_returns_the_existing_job_cancelled() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;
        lifecycle.register("dequeued").await.unwrap();
        let claim = lifecycle.wait_worker_claim().await.unwrap().unwrap();

        lifecycle.begin_shutdown().await;
        let job = claim.registered_job("dequeued").await.unwrap();
        assert!(job.is_cancelled());
        drop(claim);
        lifecycle.finish("dequeued").await;
    }

    #[tokio::test]
    async fn batch_admission_and_maintenance_cannot_partially_interleave() {
        let lifecycle = LifecycleCoordinator::new(3, 1, 1);
        lifecycle.open_for_test().await;
        let admission = lifecycle.begin_job_admission(3).await.unwrap();
        let maintenance_lifecycle = lifecycle.clone();
        let started = Arc::new(Barrier::new(2));
        let maintenance_started = started.clone();
        let maintenance = tokio::spawn(async move {
            maintenance_started.wait().await;
            maintenance_lifecycle.acquire_maintenance().await
        });
        started.wait().await;
        tokio::task::yield_now().await;
        assert!(!maintenance.is_finished());

        let ids = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        admission.publish(&ids).await.unwrap();
        let error = match maintenance.await.unwrap() {
            Ok(_) => panic!("maintenance unexpectedly acquired"),
            Err(error) => error,
        };
        assert_eq!(error.code, "busy");
        let mut expected_ids = ids.clone();
        expected_ids.sort();
        assert_eq!(lifecycle.active_ids().await, expected_ids);
        for id in &ids {
            lifecycle.finish(id).await;
        }
    }

    #[tokio::test]
    async fn cancellation_is_idempotent_until_a_registered_job_finishes() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;
        let job = lifecycle.register("cancel-before-spawn").await.unwrap();

        lifecycle.cancel("cancel-before-spawn").await.unwrap();
        lifecycle.cancel("cancel-before-spawn").await.unwrap();
        assert!(job.is_cancelled());
        lifecycle.finish("cancel-before-spawn").await;
        assert!(lifecycle.cancel("cancel-before-spawn").await.is_err());
        assert_eq!(lifecycle.active_count().await, 0);
    }

    #[tokio::test]
    async fn duplicate_ids_and_active_jobs_exclude_maintenance() {
        let lifecycle = LifecycleCoordinator::new(2, 1, 1);
        lifecycle.open_for_test().await;
        lifecycle.register("same-id").await.unwrap();

        assert!(lifecycle.register("same-id").await.is_err());
        assert!(lifecycle.acquire_maintenance().await.is_err());
        lifecycle.finish("same-id").await;

        let lease = lifecycle.acquire_maintenance().await.unwrap();
        assert!(lifecycle.register("paused").await.is_err());
        lease.release().await;
        lifecycle.register("resumed").await.unwrap();
        lifecycle.finish("resumed").await;
    }

    #[tokio::test]
    async fn download_and_conversion_slots_preserve_backpressure() {
        let lifecycle = LifecycleCoordinator::new(2, 1, 1);
        lifecycle.open_for_test().await;
        let first_job = lifecycle.register("first").await.unwrap();
        let second_job = lifecycle.register("second").await.unwrap();
        let first_download = lifecycle.acquire_download_slot().await.unwrap();
        let second_download = lifecycle.acquire_download_slot().await.unwrap();
        let first_conversion = lifecycle
            .acquire_conversion(&first_job)
            .await
            .unwrap()
            .unwrap();

        let download_waiter = lifecycle.acquire_download_slot();
        tokio::pin!(download_waiter);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut download_waiter)
                .await
                .is_err()
        );
        let conversion_waiter = lifecycle.acquire_conversion(&second_job);
        tokio::pin!(conversion_waiter);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut conversion_waiter)
                .await
                .is_err()
        );

        drop(first_download);
        drop(first_conversion);
        assert!(download_waiter.await.is_ok());
        assert!(conversion_waiter.await.unwrap().is_some());
        drop(second_download);
        lifecycle.finish("first").await;
        lifecycle.finish("second").await;
    }

    #[tokio::test]
    async fn cancel_all_cannot_release_an_update_maintenance_lease() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;
        let lease = lifecycle.acquire_maintenance().await.unwrap();

        assert!(lifecycle.begin_cancel_all().await.is_err());
        assert!(lifecycle.register("must-remain-paused").await.is_err());
        lease.release().await;
        lifecycle.register("after-update").await.unwrap();
        lifecycle.finish("after-update").await;
    }

    #[tokio::test]
    async fn late_cancel_all_completion_resumes_the_same_generation() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;
        let job = lifecycle.register("slow").await.unwrap();

        let ticket = lifecycle.begin_cancel_all().await.unwrap();
        assert!(job.is_cancelled());
        assert!(lifecycle.begin_job_admission(1).await.is_err());
        lifecycle.finish("slow").await;
        assert_eq!(
            lifecycle.wait_for_drain(ticket).await,
            DrainCompletion::Idle
        );
        assert!(lifecycle.resume_after_drain(ticket).await);

        let resumed = lifecycle.register("resumed").await.unwrap();
        assert!(!resumed.is_cancelled());
        lifecycle.finish("resumed").await;
    }

    #[tokio::test]
    async fn a_second_cancel_all_cannot_share_or_resume_an_active_generation() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;
        lifecycle.register("slow").await.unwrap();
        let ticket = lifecycle.begin_cancel_all().await.unwrap();

        let error = lifecycle.begin_cancel_all().await.unwrap_err();
        assert_eq!(error.code, "busy");
        lifecycle.finish("slow").await;
        assert_eq!(
            lifecycle.wait_for_drain(ticket).await,
            DrainCompletion::Idle
        );
        assert!(lifecycle.resume_after_drain(ticket).await);
    }

    #[tokio::test]
    async fn shutdown_supersedes_a_late_cancel_all_completion() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;
        lifecycle.register("slow").await.unwrap();
        let ticket = lifecycle.begin_cancel_all().await.unwrap();

        lifecycle.begin_shutdown().await;
        lifecycle.finish("slow").await;
        assert_eq!(
            lifecycle.wait_for_drain(ticket).await,
            DrainCompletion::SupersededByShutdown
        );
        assert!(!lifecycle.resume_after_drain(ticket).await);
        assert!(lifecycle.begin_job_admission(1).await.is_err());
    }

    #[tokio::test]
    async fn versioned_maintenance_release_cannot_reopen_shutdown() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;
        let lease = lifecycle.acquire_maintenance().await.unwrap();

        lifecycle.begin_shutdown().await;
        lease.release().await;
        assert!(lifecycle.begin_job_admission(1).await.is_err());
        assert!(lifecycle.wait_worker_claim().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn publication_defers_update_cancellation_and_shutdown_deadline() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;
        let (tracked, context) = lifecycle
            .register_update("runtime-update".to_string(), OperationKind::RuntimeUpdate)
            .unwrap();
        let publication = context
            .enter_publication(PublicationKind::RuntimeCommit)
            .unwrap();

        assert_eq!(
            lifecycle.cancel_update("runtime-update").unwrap(),
            UpdateCancellation::DeferredPublication
        );
        assert!(!context.is_cancelled());
        lifecycle.begin_shutdown().await;

        let waiting_lifecycle = lifecycle.clone();
        let waiter = tokio::spawn(async move {
            waiting_lifecycle
                .wait_for_shutdown_tasks(Duration::ZERO)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());

        drop(publication);
        context.cancelled().await;
        drop(tracked);
        assert!(waiter.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn committed_publication_cannot_be_cancelled_before_terminal_finalization() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;
        let (tracked, context) = lifecycle
            .register_update("published-update".to_string(), OperationKind::RuntimeUpdate)
            .unwrap();
        context
            .enter_publication(PublicationKind::RuntimeCommit)
            .unwrap()
            .commit();

        assert_eq!(
            lifecycle.cancel_update("published-update").unwrap(),
            UpdateCancellation::DeferredPublication
        );
        assert!(!context.is_cancelled());
        drop(tracked);
        let job = lifecycle.register("after-runtime-commit").await.unwrap();
        assert!(!job.is_cancelled());
        lifecycle.finish("after-runtime-commit").await;
    }

    #[tokio::test]
    async fn committed_installer_handoff_permanently_closes_admission() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;

        let cache_lease = lifecycle.acquire_maintenance().await.unwrap();
        let (cache_task, cache_context) = lifecycle
            .register_update("cache-only".to_string(), OperationKind::AppUpdate)
            .unwrap();
        drop(
            cache_context
                .enter_publication(PublicationKind::InstallerCache)
                .unwrap(),
        );
        drop(cache_task);
        cache_lease.release().await;
        let cache_did_not_latch = lifecycle.register("after-cache").await.unwrap();
        assert!(!cache_did_not_latch.is_cancelled());
        lifecycle.finish("after-cache").await;

        let handoff_lease = lifecycle.acquire_maintenance().await.unwrap();
        let (handoff_task, handoff_context) = lifecycle
            .register_update("installer-handoff".to_string(), OperationKind::AppUpdate)
            .unwrap();
        handoff_context
            .enter_publication(PublicationKind::InstallerHandoff)
            .unwrap()
            .commit();
        drop(handoff_task);
        handoff_lease.release().await;

        assert!(lifecycle.begin_job_admission(1).await.is_err());
        assert!(lifecycle.acquire_maintenance().await.is_err());
        assert!(lifecycle
            .register_update("too-late".to_string(), OperationKind::RuntimeUpdate)
            .is_err());
        assert!(lifecycle.wait_worker_claim().await.unwrap().is_none());

        lifecycle.begin_shutdown().await;
        lifecycle
            .wait_for_shutdown_tasks(Duration::from_millis(100))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_accounts_for_cleanup_and_rejects_unrelated_new_tasks() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;
        let cleanup = lifecycle
            .register_task(TrackedTaskKind::Cleanup, None, false)
            .unwrap();
        lifecycle.begin_shutdown().await;
        assert!(lifecycle
            .register_task(TrackedTaskKind::Cleanup, None, false)
            .is_err());
        let continuation = lifecycle
            .register_task(TrackedTaskKind::Cleanup, None, true)
            .unwrap();

        let waiting_lifecycle = lifecycle.clone();
        let waiter = tokio::spawn(async move {
            waiting_lifecycle
                .wait_for_shutdown_tasks(Duration::from_secs(60))
                .await
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(cleanup);
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(continuation);
        assert!(waiter.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn event_pump_waits_for_producers_without_waiting_for_itself() {
        let lifecycle = LifecycleCoordinator::new(1, 1, 1);
        lifecycle.open_for_test().await;
        let event_pump = lifecycle
            .register_task(TrackedTaskKind::Events, None, false)
            .unwrap();
        let producer = lifecycle
            .register_task(TrackedTaskKind::Admission, None, false)
            .unwrap();

        lifecycle.begin_shutdown().await;
        let waiting_lifecycle = lifecycle.clone();
        let waiter = tokio::spawn(async move {
            waiting_lifecycle
                .wait_for_all_producers_done_on_shutdown()
                .await;
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());

        drop(producer);
        waiter.await.unwrap();
        let diagnostics = lifecycle.task_diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].kind, TrackedTaskKind::Events);

        drop(event_pump);
        lifecycle
            .wait_for_shutdown_tasks(Duration::from_millis(100))
            .await
            .unwrap();
    }
}
