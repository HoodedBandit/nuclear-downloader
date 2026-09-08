use super::LifecycleCoordinator;
use crate::downloader::process::DownloadJob;
use tokio::sync::OwnedSemaphorePermit;

impl LifecycleCoordinator {
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
}
