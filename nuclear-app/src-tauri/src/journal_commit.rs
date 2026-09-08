use crate::app_error::AppError;
use crate::journal::JournalStore;
use crate::state::StateData;
use std::sync::Arc;

pub(crate) async fn persist_state(
    store: Arc<JournalStore>,
    state: Arc<StateData>,
    retention_now: u64,
) -> Result<(), AppError> {
    tokio::task::spawn_blocking(move || {
        let journal = state.persistence_journal();
        let prepared = journal.prepare_for_persistence(retention_now)?;
        store.save_prepared(prepared)
    })
    .await
    .map_err(|_| AppError::internal("The application journal writer stopped unexpectedly."))?
}
