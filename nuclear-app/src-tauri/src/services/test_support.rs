use super::Backend;
use crate::backend_lifecycle_tests::coordinator;
use crate::state::StateStore;
use std::path::PathBuf;

pub(crate) async fn started_backend(label: &str) -> (Backend, PathBuf) {
    let root =
        std::env::temp_dir().join(format!("nuclear-service-{label}-{}", uuid::Uuid::new_v4()));
    let state_store =
        StateStore::open_at(root.join("state.dpapi"), root.join("diagnostics")).unwrap();
    let download_manager = tokio::time::timeout(std::time::Duration::from_secs(2), coordinator())
        .await
        .expect("the fixture must complete the real startup barrier");
    (
        Backend {
            download_manager,
            state_store,
        },
        root,
    )
}
