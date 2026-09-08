//! Handle operations shared by filesystem owners. Callers choose access, sharing,
//! path validation and cleanup policy; this module never opens or removes a path.

use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use windows_sys::Win32::Storage::FileSystem::{
    FileDispositionInfo, GetFileInformationByHandle, SetFileInformationByHandle,
    BY_HANDLE_FILE_INFORMATION, FILE_DISPOSITION_INFO,
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    volume: u32,
    index: u64,
}

pub(crate) fn identity(file: &File) -> io::Result<FileIdentity> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the borrowed File keeps its handle valid, and the output has the
    // SDK-defined layout and remains alive for the complete call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FileIdentity {
        volume: information.dwVolumeSerialNumber,
        index: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    })
}

/// Requests deletion of the exact opened object when its last handle closes.
/// The caller must have opened it with DELETE access and proved its ownership.
/// Windows rejects directory deletion if the directory is no longer empty.
pub(crate) fn mark_for_deletion(file: &File) -> io::Result<()> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: File owns a live handle, and the SDK-defined disposition structure
    // stays valid for this synchronous call. No path is looked up again.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&raw const disposition).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
