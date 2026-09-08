use super::Backend;
use crate::app_error::AppError;
use crate::lifecycle::{
    DownloadManager, MaintenanceLease as ManagerMaintenanceLease, TrackedUpdate, UpdateTaskContext,
};
use crate::models::{OperationKind, OperationSnapshot};
use crate::state::StateStore;

struct AppMaintenanceLeaseInner {
    manager_lease: ManagerMaintenanceLease,
    store: StateStore,
    coordinator: DownloadManager,
    operation_id: String,
}

pub(super) struct AppMaintenanceLease {
    inner: Option<AppMaintenanceLeaseInner>,
}

impl AppMaintenanceLease {
    pub(super) async fn release(mut self) {
        if let Some(inner) = self.inner.take() {
            release_app_maintenance(inner).await;
        }
    }

    pub(super) async fn release_for_installer_handoff(mut self) {
        if let Some(inner) = self.inner.take() {
            // The operation remains installing until the next launch reconciles
            // its durable expected version. The coordinator's committed handoff
            // latch prevents this lease release from reopening admission.
            inner.manager_lease.release().await;
        }
    }
}

impl Drop for AppMaintenanceLease {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            let coordinator = inner.coordinator.clone();
            let store = inner.store.clone();
            if let Err(error) =
                coordinator.spawn_cleanup_continuation(release_app_maintenance(inner))
            {
                store.diagnostics().log(
                    "warning",
                    "maintenance_cleanup_registration_failed",
                    &error.correlation_id,
                    &error.summary,
                );
            }
        }
    }
}

async fn release_app_maintenance(inner: AppMaintenanceLeaseInner) {
    match inner
        .store
        .end_maintenance_operation(&inner.operation_id)
        .await
    {
        Ok(Some(_delta)) => {}
        Ok(None) => {}
        Err(error) => inner.store.diagnostics().log(
            "warning",
            "maintenance_state_cleanup_failed",
            &error.correlation_id,
            &error.summary,
        ),
    }
    inner.manager_lease.release().await;
}

pub(super) struct AdmittedUpdate {
    pub(super) operation: OperationSnapshot,
    pub(super) lease: AppMaintenanceLease,
    pub(super) tracked: TrackedUpdate,
    pub(super) context: UpdateTaskContext,
}

pub(super) async fn acquire_app_maintenance(
    backend: &Backend,
    kind: OperationKind,
) -> Result<AdmittedUpdate, AppError> {
    let manager_lease = backend.download_manager.acquire_maintenance().await?;
    let operation_id = uuid::Uuid::new_v4().to_string();
    let (tracked, context) = match backend
        .download_manager
        .register_update(operation_id.clone(), kind)
    {
        Ok(registration) => registration,
        Err(error) => {
            manager_lease.release().await;
            return Err(error);
        }
    };
    let (operation, _deltas) = match backend
        .state_store
        .begin_maintenance_operation_with_id(kind, operation_id)
        .await
    {
        Ok(result) => result,
        Err(error) => {
            drop(tracked);
            manager_lease.release().await;
            return Err(error);
        }
    };
    let lease = AppMaintenanceLease {
        inner: Some(AppMaintenanceLeaseInner {
            manager_lease,
            store: backend.state_store.clone(),
            coordinator: backend.download_manager.clone(),
            operation_id: operation.id.clone(),
        }),
    };
    Ok(AdmittedUpdate {
        operation,
        lease,
        tracked,
        context,
    })
}
