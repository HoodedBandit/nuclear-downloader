use std::fmt;
use std::io::Read;
use tokio::io::{AsyncRead, AsyncReadExt};

#[derive(Debug)]
pub(crate) enum BoundedReadError {
    LimitExceeded,
    Io(std::io::Error),
    InvalidLimit,
}

impl fmt::Display for BoundedReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LimitExceeded => formatter.write_str("The file exceeds its read limit."),
            Self::Io(error) => error.fmt(formatter),
            Self::InvalidLimit => formatter.write_str("The file read limit is invalid."),
        }
    }
}

// Read one extra byte to distinguish an exact-limit file from an oversized one.
// Callers keep responsibility for opening, ownership checks and error policy.
pub(crate) fn read_bounded<R: Read>(
    reader: &mut R,
    limit: u64,
) -> Result<Vec<u8>, BoundedReadError> {
    let probe_limit = limit.checked_add(1).ok_or(BoundedReadError::InvalidLimit)?;
    let mut bytes = Vec::new();
    reader
        .take(probe_limit)
        .read_to_end(&mut bytes)
        .map_err(BoundedReadError::Io)?;
    if bytes.len() as u64 > limit {
        return Err(BoundedReadError::LimitExceeded);
    }
    Ok(bytes)
}

pub(crate) async fn read_bounded_async<R: AsyncRead + Unpin>(
    reader: &mut R,
    limit: u64,
) -> Result<Vec<u8>, BoundedReadError> {
    let probe_limit = limit.checked_add(1).ok_or(BoundedReadError::InvalidLimit)?;
    let mut bytes = Vec::new();
    reader
        .take(probe_limit)
        .read_to_end(&mut bytes)
        .await
        .map_err(BoundedReadError::Io)?;
    if bytes.len() as u64 > limit {
        return Err(BoundedReadError::LimitExceeded);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
