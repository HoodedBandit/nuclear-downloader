use super::{
    LifecycleCoordinator, PublicationGuard, PublicationKind, TaskRecord, TrackedTaskGuard,
    TrackedTaskKind, TrackedUpdate, UpdateCancellation, UpdateRecord, UpdateRunError,
    UpdateTaskContext,
};
use crate::app_error::AppError;
use crate::models::OperationKind;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;

impl LifecycleCoordinator {
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
