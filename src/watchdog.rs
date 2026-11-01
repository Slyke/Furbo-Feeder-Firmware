//! Root-generated settings for the existing factory recovery monitor.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub audio_cpu_percent: u8,
    pub rtsp_cpu_percent: u8,
    pub consecutive_readings: u16,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            audio_cpu_percent: 50,
            rtsp_cpu_percent: 50,
            consecutive_readings: 24,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=100).contains(&self.audio_cpu_percent)
                && (1..=100).contains(&self.rtsp_cpu_percent),
            "CPU thresholds must be 1..100"
        );
        ensure!(
            (2..=720).contains(&self.consecutive_readings),
            "watchdog readings must be 2..720"
        );
        Ok(())
    }
}
pub fn apply(settings: &Settings) -> Result<()> {
    settings.validate()?;
    // The factory comparison is counter > maximum, hence configured N uses N-1.
    let values=format!("high_pulseaudio_usge={}\nhigh_rtsp_svc_usge={}\nmax_over_pulseaudio={}\nmax_over_rtsp_svc={}\n",
        settings.audio_cpu_percent,settings.rtsp_cpu_percent,
        settings.consecutive_readings-1,settings.consecutive_readings-1);
    crate::store::atomic_write(
        Path::new("/run/furbo-local/watchdog.env"),
        values.as_bytes(),
    )
}
