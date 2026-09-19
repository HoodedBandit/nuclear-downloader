// A local UI preview must never load or alter the installed app's queue,
// preferences, diagnostics, or downloaded tool versions.
#[cfg(feature = "local-preview")]
pub const USER_DATA_FOLDER: &str = "Nuclear Downloader Preview";
#[cfg(not(feature = "local-preview"))]
pub const USER_DATA_FOLDER: &str = "Nuclear Downloader";

#[cfg(feature = "local-preview")]
pub const TOOL_DATA_FOLDER: &str = "NuclearDownloaderPreview";
#[cfg(not(feature = "local-preview"))]
pub const TOOL_DATA_FOLDER: &str = "NuclearDownloader";
