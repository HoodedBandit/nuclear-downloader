use crate::app_error::AppError;
use crate::lifecycle::DownloadManager;
use crate::models::OperationState;
use crate::state::StateStore;

struct InspectionAdmissionCleanup {
    store: StateStore,
    manager: DownloadManager,
    operation_id: String,
}

pub(crate) struct InspectionAdmissionGuard {
    cleanup: Option<InspectionAdmissionCleanup>,
}

impl InspectionAdmissionGuard {
    pub(crate) fn new(store: StateStore, manager: DownloadManager, operation_id: String) -> Self {
        Self {
            cleanup: Some(InspectionAdmissionCleanup {
                store,
                manager,
                operation_id,
            }),
        }
    }

    pub(crate) async fn finalize(mut self, error: AppError) -> AppError {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup_inspection_admission(cleanup, error.clone()).await;
        }
        error
    }

    pub(crate) fn disarm(mut self) {
        self.cleanup = None;
    }
}

impl Drop for InspectionAdmissionGuard {
    fn drop(&mut self) {
        let Some(cleanup) = self.cleanup.take() else {
            return;
        };
        let store = cleanup.store.clone();
        let manager = cleanup.manager.clone();
        let error = AppError::internal("The inspection admission stopped unexpectedly.");
        if let Err(registration_error) =
            manager.spawn_cleanup_continuation(cleanup_inspection_admission(cleanup, error))
        {
            store.diagnostics().log(
                "error",
                "inspection_admission_cleanup_registration_failed",
                &registration_error.correlation_id,
                &registration_error.summary,
            );
        }
    }
}

async fn cleanup_inspection_admission(cleanup: InspectionAdmissionCleanup, error: AppError) {
    match cleanup.manager.cancel(&cleanup.operation_id).await {
        Err(cancel_error) if cancel_error.code != "not_found" => {
            cleanup.store.diagnostics().log(
                "warning",
                "inspection_admission_cancel_failed",
                &cancel_error.correlation_id,
                &cancel_error.summary,
            );
        }
        _ => {}
    }
    let result = cleanup
        .store
        .finalize_operation(&cleanup.operation_id, OperationState::Failed, Some(error))
        .await;
    if let Err(finalization_error) = result {
        cleanup.store.diagnostics().log(
            "error",
            "inspection_admission_finalization_failed",
            &finalization_error.correlation_id,
            &finalization_error.summary,
        );
    }
    cleanup.manager.finish(&cleanup.operation_id).await;
}
