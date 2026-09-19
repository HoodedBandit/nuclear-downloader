use crate::app_error::AppError;
use crate::journal::JournalStore;
use crate::models::{AppSnapshot, OperationState};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiTheme {
    #[default]
    Light,
    Dark,
    System,
}

fn preference_path() -> Result<PathBuf, AppError> {
    Ok(JournalStore::default_path()?.with_file_name("appearance.json"))
}

pub fn read_theme() -> Result<UiTheme, AppError> {
    read_theme_at(&preference_path()?)
}

fn read_theme_at(path: &Path) -> Result<UiTheme, AppError> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(UiTheme::Light),
        Err(_) => {
            return Err(AppError::internal(
                "Could not read your appearance preference.",
            ))
        }
    };
    let mut bytes = Vec::new();
    file.take(1025)
        .read_to_end(&mut bytes)
        .map_err(|_| AppError::internal("Could not read your appearance preference."))?;
    if bytes.len() > 1024 {
        return Err(AppError::new(
            "invalid_appearance",
            "The saved appearance preference is invalid. Choose a theme to reset it.",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        AppError::new(
            "invalid_appearance",
            "The saved appearance preference is invalid. Choose a theme to reset it.",
        )
    })
}

pub fn write_theme(theme: UiTheme) -> Result<(), AppError> {
    write_theme_at(&preference_path()?, theme)
}

fn write_theme_at(path: &Path, theme: UiTheme) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::internal("The appearance folder is unavailable."))?;
    fs::create_dir_all(parent)
        .map_err(|_| AppError::internal("Could not create the appearance folder."))?;
    let temporary = parent.join(format!(".appearance-{}.tmp", uuid::Uuid::new_v4()));
    let save = || -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(&theme)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    };
    if save().is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(AppError::internal(
            "Could not save your appearance preference. The previous theme is still selected.",
        ));
    }
    Ok(())
}

pub fn published_file(snapshot: &AppSnapshot, item_id: &str) -> Result<PathBuf, AppError> {
    let item = snapshot
        .queue
        .iter()
        .find(|item| item.id == item_id)
        .ok_or_else(|| AppError::new("item_missing", "This download is no longer in the queue."))?;
    let operation = snapshot
        .operations
        .iter()
        .find(|operation| {
            Some(&operation.id) == item.latest_operation_id.as_ref()
                && operation.queue_item_id.as_deref() == Some(item_id)
                && operation.state == OperationState::Completed
        })
        .ok_or_else(|| {
            AppError::new(
                "output_unavailable",
                "This download has no completed output to show.",
            )
        })?;
    let output = operation.published_output.as_ref().ok_or_else(|| {
        AppError::new(
            "output_unavailable",
            "This download has no recorded output file.",
        )
    })?;
    let path = PathBuf::from(&output.path);
    if !path.is_absolute() || !path.is_file() {
        return Err(AppError::new(
            "output_missing",
            "The downloaded file was moved or removed.",
        ));
    }
    Ok(path)
}

pub fn reveal_file(path: &Path) -> Result<(), AppError> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // The destination comes from an authoritative completed operation, never a renderer path.
        std::process::Command::new("explorer.exe")
            .arg(format!("/select,{}", path.display()))
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|_| AppError::internal("Could not show the downloaded file in Explorer."))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(AppError::internal(
            "Showing downloaded files is available on Windows.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn appearance_round_trips_across_restarts_and_replaces_existing_value() {
        let root =
            std::env::temp_dir().join(format!("nuclear-appearance-{}", uuid::Uuid::new_v4()));
        let path = root.join("appearance.json");
        assert_eq!(read_theme_at(&path).unwrap(), UiTheme::Light);
        for theme in [UiTheme::Dark, UiTheme::System, UiTheme::Light] {
            write_theme_at(&path, theme).unwrap();
            assert_eq!(read_theme_at(&path).unwrap(), theme);
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn malformed_or_oversized_preferences_are_reported_without_overwriting() {
        let root =
            std::env::temp_dir().join(format!("nuclear-appearance-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("appearance.json");
        for bytes in [b"\"purple\"".to_vec(), vec![b' '; 1025]] {
            fs::write(&path, &bytes).unwrap();
            assert!(read_theme_at(&path).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        fs::remove_dir_all(root).unwrap();
    }
}
