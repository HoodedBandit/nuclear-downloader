use crate::app_error::AppError;
use crate::journal::PersistentJournal;
use crate::models::{
    OperationSnapshot, PendingAppUpdateRecovery, PersistenceHealth, PlaylistEntry, QueueItemRecord,
    RuntimeReadiness, UrlInspection, APP_SCHEMA_VERSION,
};
use std::collections::{HashMap, VecDeque};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Weak};

pub(super) const MAX_RETAINED_INSPECTION_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub(super) struct InspectionRetentionBudget {
    retained: HashMap<usize, (Weak<UrlInspection>, usize)>,
}

impl InspectionRetentionBudget {
    pub(super) fn retain(
        &mut self,
        inspection: UrlInspection,
    ) -> Result<Arc<UrlInspection>, AppError> {
        self.retained
            .retain(|_, (inspection, _)| inspection.strong_count() != 0);
        let retained_bytes = self
            .retained
            .values()
            .map(|(_, bytes)| *bytes)
            .sum::<usize>();
        let bytes = estimate_inspection_allocation(&inspection);
        if retained_bytes.saturating_add(bytes) > MAX_RETAINED_INSPECTION_BYTES {
            return Err(AppError::new(
                "inspection_retention_limit",
                "Completed inspection results reached the 16 MiB retention limit. Add or dismiss earlier results, then retry.",
            )
            .retryable(true));
        }
        let inspection = Arc::new(inspection);
        self.retained.insert(
            Arc::as_ptr(&inspection) as usize,
            (Arc::downgrade(&inspection), bytes),
        );
        Ok(inspection)
    }
}

#[derive(Clone)]
pub(super) struct SharedRecord<T>(Arc<T>);

impl<T> SharedRecord<T> {
    fn new(value: T) -> Self {
        Self(Arc::new(value))
    }
}

impl<T: Clone> SharedRecord<T> {
    pub(super) fn snapshot(&self) -> T {
        self.0.as_ref().clone()
    }
}

impl<T> Deref for SharedRecord<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl<T: Clone> DerefMut for SharedRecord<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl<T> From<T> for SharedRecord<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

#[derive(Clone)]
pub(super) struct StateData {
    pub(super) sequence: u64,
    pub(super) queue_order: Vec<String>,
    pub(super) queue: HashMap<String, SharedRecord<QueueItemRecord>>,
    pub(super) operation_order: Vec<String>,
    pub(super) operations: HashMap<String, SharedRecord<OperationSnapshot>>,
    pub(super) pending_downloads: VecDeque<String>,
    pub(super) runtime_readiness: RuntimeReadiness,
    pub(super) maintenance_active: bool,
    pub(super) draining: bool,
    pub(super) maintenance_owner: Option<String>,
    pub(super) persistence_health: PersistenceHealth,
    pub(super) persistence_dirty: bool,
    pub(super) pending_app_update: Option<PendingAppUpdateRecovery>,
}

impl StateData {
    pub(super) fn persistence_journal(&self) -> PersistentJournal {
        PersistentJournal {
            schema_version: APP_SCHEMA_VERSION,
            revision: self.sequence,
            queue: self
                .queue_order
                .iter()
                .filter_map(|id| self.queue.get(id).map(SharedRecord::snapshot))
                .collect(),
            operations: self
                .operation_order
                .iter()
                .filter_map(|id| {
                    self.operations.get(id).map(|operation| {
                        let mut operation = operation.snapshot();
                        operation.inspection_result = None;
                        operation
                    })
                })
                .collect(),
            pending_app_update: self.pending_app_update.clone(),
        }
    }
}

pub(crate) fn estimate_inspection_allocation(inspection: &UrlInspection) -> usize {
    let fixed =
        std::mem::size_of::<UrlInspection>().saturating_add(2 * std::mem::size_of::<usize>());
    match inspection {
        UrlInspection::Video { video } => [
            video.id.capacity(),
            video.title.capacity(),
            video.channel.as_ref().map_or(0, String::capacity),
            video.thumbnail.as_ref().map_or(0, String::capacity),
            video.url.capacity(),
            video
                .available_qualities
                .capacity()
                .saturating_mul(std::mem::size_of::<String>()),
            video
                .available_qualities
                .iter()
                .map(String::capacity)
                .fold(0usize, usize::saturating_add),
        ]
        .into_iter()
        .fold(fixed, usize::saturating_add),
        UrlInspection::Playlist { playlist } => {
            let entry_heap = playlist.entries.iter().fold(0usize, |total, entry| {
                [
                    entry.id.capacity(),
                    entry.title.as_ref().map_or(0, String::capacity),
                    entry.url.capacity(),
                    entry.thumbnail.as_ref().map_or(0, String::capacity),
                ]
                .into_iter()
                .fold(total, usize::saturating_add)
            });
            [
                playlist.title.capacity(),
                playlist.channel.as_ref().map_or(0, String::capacity),
                playlist
                    .entries
                    .capacity()
                    .saturating_mul(std::mem::size_of::<PlaylistEntry>()),
                entry_heap,
            ]
            .into_iter()
            .fold(fixed, usize::saturating_add)
        }
    }
}
