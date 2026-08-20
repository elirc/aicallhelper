//! Crash logging (§11).
//!
//! POLICY: nothing sensitive is ever written to crash.log — no API keys, no
//! resume or job-description text, no transcripts, no answers. The log carries
//! only a timestamp, the panic's source location, and the panic message
//! (developer-authored text; no code path in this app formats user data into a
//! panic message).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Install a panic hook that appends one timestamped line per panic to
/// `crash_log`.
///
/// Containment note: answer pipelines run inside tokio tasks, so under
/// unwinding a panic kills that one task, the hook logs it, and the process
/// (and every other session) survives. This is why the release profile keeps
/// unwinding semantics rather than `panic = "abort"` (§11).
pub fn install_panic_hook(crash_log: PathBuf) {
    // Chain rather than replace: the default hook's stderr print is what a dev
    // terminal shows, and losing it would make dev crashes silent.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let line = format!(
            "[{}] panic at {location}: {}\n",
            format_timestamp(secs),
            payload_message(info.payload())
        );
        // Best effort by design: a crash log that cannot be written must not
        // turn one crash into a second one.
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&crash_log) {
            let _ = file.write_all(line.as_bytes());
        }
        previous(info);
    }));
}

fn payload_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.as_str()
    } else {
        "<non-string panic payload>"
    }
}

/// Unix seconds -> "YYYY-MM-DDTHH:MM:SSZ", no chrono dependency. UTC on
/// purpose: crash timestamps get correlated across machines and DST walls.
pub fn format_timestamp(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs = unix_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Days-since-epoch -> Gregorian date (Howard Hinnant's civil_from_days).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_formats_as_the_epoch() {
        assert_eq!(format_timestamp(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn last_second_of_a_day_stays_in_that_day() {
        assert_eq!(format_timestamp(86_399), "1970-01-01T23:59:59Z");
    }

    #[test]
    fn known_timestamp_round_trips() {
        // The billennium: well-known fixed point for the whole algorithm.
        assert_eq!(format_timestamp(1_000_000_000), "2001-09-09T01:46:40Z");
    }

    #[test]
    fn leap_day_is_computed_correctly() {
        // 2000 is the leap-year edge case (divisible by 400): a naive
        // century rule would skip Feb 29 here.
        assert_eq!(format_timestamp(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn pre_epoch_times_do_not_wrap() {
        // A clock set before 1970 must not produce garbage via unsigned wrap.
        assert_eq!(format_timestamp(-1), "1969-12-31T23:59:59Z");
    }
}
