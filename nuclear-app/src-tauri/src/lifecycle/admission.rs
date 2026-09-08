use super::{
    DrainCompletion, DrainTicket, JobAdmission, LifecycleCoordinator, LifecycleMode,
    MaintenanceLease, WorkerClaim,
};
use crate::app_error::AppError;
use crate::downloader::process::DownloadJob;
use std::collections::HashSet;
use std::sync::atomic::Ordering;

const MAX_REGISTERED_JOBS: usize = 1_000;

impl LifecycleCoordinator {
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
