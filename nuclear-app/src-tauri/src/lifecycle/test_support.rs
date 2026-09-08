use super::{LifecycleCoordinator, LifecycleMode};
use crate::downloader::process::DownloadJob;

impl LifecycleCoordinator {
    pub(crate) async fn open_for_test(&self) {
        let _transition = self.inner.transition.clone().lock_owned().await;
        let mut state = self.inner.state.lock().await;
        assert_eq!(state.mode, LifecycleMode::Starting);
        state.mode = LifecycleMode::Accepting;
        drop(state);
        self.inner.mode_changed.notify_waiters();
    }

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

    pub(crate) async fn active_count(&self) -> usize {
        self.inner.state.lock().await.jobs.len()
    }
}
