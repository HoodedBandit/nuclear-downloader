mod command_args;
mod engine;
mod errors;
pub(crate) mod inspection;
mod naming;
pub(crate) mod process;
mod progress;
pub(crate) mod publication;
mod validation;

pub use engine::start_download;
pub(crate) use engine::DownloadOutcome;
pub use validation::{
    validate_download_request, validate_fetch_request, validate_output_directory,
};
