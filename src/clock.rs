//! UTC dates and session timestamps from `SystemTime`, with no extra crate.

use std::time::{SystemTime, UNIX_EPOCH};

/// Today's UTC date as `YYYY-MM-DD`.
pub fn date(time: SystemTime) -> String {
    let (year, month, day, _, _, _) = civil(time);
    format!("{year:04}-{month:02}-{day:02}")
}

/// A sortable UTC timestamp as `YYYYMMDDTHHMMSSZ`.
pub fn timestamp(time: SystemTime) -> String {
    let (year, month, day, hour, minute, second) = civil(time);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

fn civil(time: SystemTime) -> (i64, u32, u32, u32, u32, u32) {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    (
        year,
        month,
        day,
        (rest / 3_600) as u32,
        ((rest % 3_600) / 60) as u32,
        (rest % 60) as u32,
    )
}

/// Howard Hinnant's civil-from-days algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        year + if month <= 2 { 1 } else { 0 },
        month as u32,
        day as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn formats_known_dates() {
        assert_eq!(date(UNIX_EPOCH), "1970-01-01");
        assert_eq!(
            date(UNIX_EPOCH + Duration::from_secs(946_684_800)),
            "2000-01-01"
        );
        assert_eq!(
            date(UNIX_EPOCH + Duration::from_secs(1_709_210_096)),
            "2024-02-29"
        );
    }

    #[test]
    fn formats_sortable_timestamps() {
        assert_eq!(timestamp(UNIX_EPOCH), "19700101T000000Z");
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(1_709_210_096)),
            "20240229T123456Z"
        );
    }
}
