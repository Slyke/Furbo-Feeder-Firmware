use anyhow::{ensure, Context, Result};
use chrono::{DateTime, FixedOffset, Utc};
use std::collections::BTreeMap;
pub const VERSION: &str = include_str!("../tzdb/VERSION");
pub fn load(name: &str) -> Result<tz::TimeZone> {
    let index: BTreeMap<&str, [usize; 2]> =
        serde_json::from_str(include_str!("../tzdb/index.json"))?;
    let &[offset, length] = index.get(name).context("unknown IANA timezone")?;
    let bytes = include_bytes!("../tzdb/zones.bin");
    ensure!(offset + length <= bytes.len(), "invalid timezone bundle");
    Ok(tz::TimeZone::from_tz_data(&bytes[offset..offset + length])?)
}
pub fn at(zone: &tz::TimeZone, time: DateTime<Utc>) -> Result<DateTime<FixedOffset>> {
    let offset = zone.find_local_time_type(time.timestamp())?.ut_offset();
    Ok(time.with_timezone(&FixedOffset::east_opt(offset).context("invalid UTC offset")?))
}
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};
    #[test]
    fn current_vancouver_rules() {
        let zone = load("America/Vancouver").unwrap();
        let t = Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0).unwrap();
        let local = at(&zone, t).unwrap();
        assert_eq!(local.offset().local_minus_utc(), -7 * 3600);
        assert_eq!(local.hour(), 5);
    }
    #[test]
    fn historical_fold_runs_once_per_local_date() {
        let z = load("America/New_York").unwrap();
        let a = at(&z, Utc.with_ymd_and_hms(2024, 11, 3, 5, 30, 0).unwrap()).unwrap();
        let b = at(&z, Utc.with_ymd_and_hms(2024, 11, 3, 6, 30, 0).unwrap()).unwrap();
        assert_eq!(
            a.format("%Y%m%d_%H%M").to_string(),
            b.format("%Y%m%d_%H%M").to_string()
        );
        assert_ne!(a.offset(), b.offset());
    }
}
