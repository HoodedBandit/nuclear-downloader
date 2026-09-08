pub(crate) mod commands;
pub(crate) mod downloads;
pub(crate) mod inspection;
mod maintenance;
pub(crate) mod operations;
pub(crate) mod queue;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod updates;

use crate::lifecycle::DownloadManager;
use crate::state::StateStore;

// Workflow services compose these independent owners. Neither state nor the
// lifecycle coordinator reaches into the other to complete an operation.
#[derive(Clone)]
pub(crate) struct Backend {
    pub(crate) download_manager: DownloadManager,
    pub(crate) state_store: StateStore,
}
