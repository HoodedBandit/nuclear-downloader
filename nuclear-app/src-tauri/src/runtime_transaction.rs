mod filesystem;
mod journal;
mod lock;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) use journal::{clear, load, protected_update_ids, quarantine, store};
#[cfg(test)]
use journal::{JOURNAL_LIMIT, QUARANTINE_PREFIX};
pub(crate) use lock::RuntimeMutationLock;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RuntimeTransactionCheckpoint {
    CandidateVerified,
    OldMoved,
    NewPublished,
    CurrentPointerCommitted,
    BackupCleaned,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RuntimeTransaction {
    schema_version: u32,
    pub(crate) update_id: String,
    pub(crate) runtime_version: String,
    pub(crate) had_existing: bool,
    pub(crate) checkpoint: RuntimeTransactionCheckpoint,
}

impl RuntimeTransaction {
    pub(crate) fn new(
        update_id: String,
        runtime_version: String,
        had_existing: bool,
    ) -> Result<Self, String> {
        validate_update_id(&update_id)?;
        validate_runtime_version(&runtime_version)?;
        Ok(Self {
            schema_version: 1,
            update_id,
            runtime_version,
            had_existing,
            checkpoint: RuntimeTransactionCheckpoint::CandidateVerified,
        })
    }

    pub(crate) fn set_checkpoint(&mut self, checkpoint: RuntimeTransactionCheckpoint) {
        self.checkpoint = checkpoint;
    }

    pub(crate) fn paths(&self, root: &Path) -> RuntimeTransactionPaths {
        let work_root = root.join(".updates").join(&self.update_id);
        RuntimeTransactionPaths {
            candidate: work_root.join("extracted"),
            work_root,
            final_dir: root.join(&self.runtime_version),
            backup: root.join(format!(
                ".backup-{}-{}",
                self.runtime_version, self.update_id
            )),
        }
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "Unsupported runtime transaction schema version {}.",
                self.schema_version
            ));
        }
        validate_update_id(&self.update_id)?;
        validate_runtime_version(&self.runtime_version)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeTransactionPaths {
    pub(crate) work_root: PathBuf,
    pub(crate) candidate: PathBuf,
    pub(crate) final_dir: PathBuf,
    pub(crate) backup: PathBuf,
}

fn validate_update_id(update_id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(update_id)
        .map(|_| ())
        .map_err(|_| "Runtime transaction update ID is invalid.".to_string())
}

fn validate_runtime_version(version: &str) -> Result<(), String> {
    let mut parts = version.split('.');
    let valid = (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }) && parts.next().is_none();
    if valid && version.trim() == version {
        Ok(())
    } else {
        Err("Runtime transaction version is invalid.".into())
    }
}

#[cfg(test)]
mod tests;
