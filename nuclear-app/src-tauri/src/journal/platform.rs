use super::MAX_ENCRYPTED_JOURNAL_BYTES;
use crate::app_error::AppError;
use crate::bounded_read::{read_bounded, BoundedReadError};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::Path;

pub(super) fn read_encrypted_journal<R: Read>(reader: &mut R) -> Result<Vec<u8>, AppError> {
    match read_bounded(reader, MAX_ENCRYPTED_JOURNAL_BYTES) {
        Ok(bytes) => Ok(bytes),
        Err(BoundedReadError::LimitExceeded) => Err(journal_too_large()),
        Err(BoundedReadError::Io(_)) => Err(AppError::internal(
            "Could not read the application journal.",
        )),
        Err(BoundedReadError::InvalidLimit) => Err(AppError::internal(
            "The application journal read limit was invalid.",
        )),
    }
}

pub(super) fn journal_too_large() -> AppError {
    AppError::new(
        "journal_too_large",
        "The saved application state is too large to load safely. The existing journal was preserved.",
    )
}

pub(super) fn acquire_journal_lock(journal_path: &Path) -> Result<fs::File, AppError> {
    let parent = journal_path
        .parent()
        .ok_or_else(|| AppError::internal("Journal path did not have a parent folder."))?;
    fs::create_dir_all(parent).map_err(|error| {
        AppError::internal("Could not create the application data folder.")
            .with_detail(error.kind().to_string())
    })?;
    let lock_path = journal_path.with_extension("lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    options.open(lock_path).map_err(|error| {
        AppError::new(
            "already_running",
            "Another Nuclear Downloader instance is already using this application data.",
        )
        .retryable(true)
        .with_detail(error.kind().to_string())
    })
}

#[cfg(windows)]
pub(super) fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "Kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    let existing = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let new = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            existing.as_ptr(),
            new.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
pub(super) fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
pub(super) fn protect_for_current_user(plaintext: &[u8]) -> Result<Vec<u8>, AppError> {
    crypt_protect(plaintext, false)
}

#[cfg(windows)]
pub(super) fn unprotect_for_current_user(ciphertext: &[u8]) -> Result<Vec<u8>, AppError> {
    crypt_protect(ciphertext, true)
}

#[cfg(windows)]
fn crypt_protect(input: &[u8], decrypt: bool) -> Result<Vec<u8>, AppError> {
    #[repr(C)]
    struct DataBlob {
        size: u32,
        data: *mut u8,
    }

    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;
    #[link(name = "Crypt32")]
    extern "system" {
        fn CryptProtectData(
            input: *const DataBlob,
            description: *const u16,
            entropy: *const DataBlob,
            reserved: *mut core::ffi::c_void,
            prompt: *const core::ffi::c_void,
            flags: u32,
            output: *mut DataBlob,
        ) -> i32;
        fn CryptUnprotectData(
            input: *const DataBlob,
            description: *mut *mut u16,
            entropy: *const DataBlob,
            reserved: *mut core::ffi::c_void,
            prompt: *const core::ffi::c_void,
            flags: u32,
            output: *mut DataBlob,
        ) -> i32;
    }
    #[link(name = "Kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }

    let size = u32::try_from(input.len())
        .map_err(|_| AppError::internal("Journal data exceeded the supported size."))?;
    let input_blob = DataBlob {
        size,
        data: input.as_ptr() as *mut u8,
    };
    let mut output_blob = DataBlob {
        size: 0,
        data: std::ptr::null_mut(),
    };
    let ok = unsafe {
        if decrypt {
            CryptUnprotectData(
                &input_blob,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output_blob,
            )
        } else {
            CryptProtectData(
                &input_blob,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output_blob,
            )
        }
    };
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        if decrypt && error.raw_os_error() == Some(13) {
            return Err(AppError::new(
                "journal_corrupt",
                "The application journal could not be decrypted because it is corrupt.",
            ));
        }
        return Err(AppError::new(
            "journal_crypto_failed",
            if decrypt {
                "Windows could not decrypt the application journal."
            } else {
                "Windows could not protect the application journal."
            },
        )
        .with_detail(error.kind().to_string()));
    }

    let result = unsafe {
        let bytes =
            std::slice::from_raw_parts(output_blob.data, output_blob.size as usize).to_vec();
        let _ = LocalFree(output_blob.data.cast());
        bytes
    };
    Ok(result)
}

#[cfg(not(windows))]
pub(super) fn protect_for_current_user(_plaintext: &[u8]) -> Result<Vec<u8>, AppError> {
    Err(AppError::new(
        "unsupported_platform",
        "Encrypted queue persistence is supported only on Windows.",
    ))
}

#[cfg(not(windows))]
pub(super) fn unprotect_for_current_user(_ciphertext: &[u8]) -> Result<Vec<u8>, AppError> {
    Err(AppError::new(
        "unsupported_platform",
        "Encrypted queue persistence is supported only on Windows.",
    ))
}
