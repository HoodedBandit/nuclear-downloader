use regex::Regex;
use std::sync::LazyLock;

pub(super) static DOWNLOAD_PROGRESS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[download\]\s+([\d.]+)%\s+of").unwrap());
pub(super) static DOWNLOAD_SPEED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"at\s+([\d.]+\w+/s)").unwrap());
pub(super) static DOWNLOAD_ETA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ETA\s+(\S+)").unwrap());
pub(super) static DOWNLOAD_MERGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\[Merger\]|\[VideoConvertor\]|\[VideoRemuxer\]|\[ExtractAudio\]|post-?process|converting|remuxing").unwrap()
});

fn parse_ffmpeg_time_value(value: &str) -> Option<f64> {
    let value = value.trim();

    if let Ok(microseconds) = value.parse::<f64>() {
        return (microseconds >= 0.0).then_some(microseconds / 1_000_000.0);
    }

    let parts: Vec<&str> = value.split(':').collect();
    if parts.len() != 3 {
        return None;
    }

    let hours = parts[0].parse::<f64>().ok()?;
    let minutes = parts[1].parse::<f64>().ok()?;
    let seconds = parts[2].parse::<f64>().ok()?;

    Some((hours * 3600.0) + (minutes * 60.0) + seconds)
}

pub(super) fn parse_ffmpeg_progress_percent(line: &str, duration_seconds: f64) -> Option<f64> {
    if duration_seconds <= 0.0 || !duration_seconds.is_finite() {
        return None;
    }

    let value = line
        .strip_prefix("out_time_us=")
        .or_else(|| line.strip_prefix("out_time_ms="))
        .or_else(|| line.strip_prefix("out_time="))?;
    let seconds = parse_ffmpeg_time_value(value)?;

    Some(((seconds / duration_seconds) * 100.0).clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::parse_ffmpeg_progress_percent;

    #[test]
    fn parses_ffmpeg_progress_from_microseconds_and_timestamps() {
        assert_eq!(
            parse_ffmpeg_progress_percent("out_time_us=5000000", 20.0),
            Some(25.0)
        );
        assert_eq!(
            parse_ffmpeg_progress_percent("out_time_ms=10000000", 20.0),
            Some(50.0)
        );
        assert_eq!(
            parse_ffmpeg_progress_percent("out_time=00:00:15.000000", 20.0),
            Some(75.0)
        );
        assert_eq!(
            parse_ffmpeg_progress_percent("out_time=00:00:30.000000", 20.0),
            Some(100.0)
        );
    }

    #[test]
    fn rejects_invalid_ffmpeg_progress_inputs() {
        assert_eq!(
            parse_ffmpeg_progress_percent("progress=continue", 20.0),
            None
        );
        assert_eq!(parse_ffmpeg_progress_percent("out_time_us=N/A", 20.0), None);
        assert_eq!(parse_ffmpeg_progress_percent("out_time_us=1000", 0.0), None);
    }
}
