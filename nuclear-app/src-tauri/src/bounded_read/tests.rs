use super::{read_bounded, read_bounded_async, BoundedReadError};
use std::io::{Cursor, Read};

#[test]
fn exact_limits_are_accepted_and_unbounded_sources_stop_after_one_probe_byte() {
    let mut exact = Cursor::new(b"bounded".to_vec());
    assert_eq!(read_bounded(&mut exact, 7).unwrap(), b"bounded");
    assert_eq!(
        read_bounded(&mut Cursor::new(Vec::<u8>::new()), 0).unwrap(),
        b""
    );

    let mut endless = std::io::repeat(b'x');
    assert!(matches!(
        read_bounded(&mut endless, 4096),
        Err(BoundedReadError::LimitExceeded)
    ));
    let mut larger = Cursor::new(vec![b'x'; 8192]);
    assert!(matches!(
        read_bounded(&mut larger, 4096),
        Err(BoundedReadError::LimitExceeded)
    ));
    assert_eq!(larger.position(), 4097);
}

#[tokio::test]
async fn asynchronous_reader_enforces_the_same_exact_limit_and_probe_bound() {
    let mut exact = Cursor::new(b"bounded".to_vec());
    assert_eq!(read_bounded_async(&mut exact, 7).await.unwrap(), b"bounded");
    let mut larger = Cursor::new(vec![b'x'; 8192]);
    assert!(matches!(
        read_bounded_async(&mut larger, 4096).await,
        Err(BoundedReadError::LimitExceeded)
    ));
    assert_eq!(larger.position(), 4097);
}

struct FailingReader;

impl Read for FailingReader {
    fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "fixture",
        ))
    }
}

#[test]
fn io_failure_is_distinct_from_overflow_and_invalid_limits_do_not_read() {
    assert!(matches!(
        read_bounded(&mut FailingReader, 8),
        Err(BoundedReadError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied
    ));
    let mut input = Cursor::new(vec![b'x']);
    assert!(matches!(
        read_bounded(&mut input, u64::MAX),
        Err(BoundedReadError::InvalidLimit)
    ));
    assert_eq!(input.position(), 0);
}
