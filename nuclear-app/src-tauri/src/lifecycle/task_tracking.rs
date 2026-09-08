use super::{
    LifecycleCoordinator, StartupOutcome, TaskRecord, TaskRegistry, TrackedTaskDiagnostic,
    TrackedTaskGuard, TrackedTaskKind, TrackedTaskTimeout,
};
use crate::app_error::AppError;
use futures_util::FutureExt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::time::Duration;

impl LifecycleCoordinator {
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

    pub(super) fn register_task(
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

    pub(super) fn task_diagnostics(&self) -> Vec<TrackedTaskDiagnostic> {
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

    pub(super) fn registry(&self) -> Result<std::sync::MutexGuard<'_, TaskRegistry>, AppError> {
        self.inner
            .registry
            .lock()
            .map_err(|_| AppError::internal("The backend task registry is unavailable."))
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
