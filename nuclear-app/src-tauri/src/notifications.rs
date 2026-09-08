use crate::models::{DownloadProgress, DownloaderRuntimeUpdateProgress, UpdateInstallProgress};
use futures_util::future::BoxFuture;
use std::sync::Arc;

pub(crate) type DownloadProgressSink = Arc<dyn Fn(&DownloadProgress) + Send + Sync>;
pub(crate) type RuntimeProgressSink = Arc<dyn Fn(DownloaderRuntimeUpdateProgress) + Send + Sync>;
pub(crate) type UpdateProgressSink = Arc<dyn Fn(UpdateInstallProgress) + Send + Sync>;
pub(crate) type DownloadCleanupWarningSink = Arc<dyn Fn(&str, &str) + Send + Sync>;

#[derive(Clone)]
pub(crate) struct DownloadNotifications {
    pub(crate) progress: Arc<dyn Fn(DownloadProgress) -> BoxFuture<'static, ()> + Send + Sync>,
    pub(crate) cleanup_warning: DownloadCleanupWarningSink,
}
