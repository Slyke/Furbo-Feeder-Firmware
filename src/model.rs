use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer,
    Operator,
    Admin,
}
impl Role {
    pub fn can_control(self) -> bool {
        self != Self::Viewer
    }
    pub fn is_admin(self) -> bool {
        self == Self::Admin
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Configure {
        settings: serde_json::Value,
    },
    SetSchedules {
        schedules: Vec<Schedule>,
    },
    DeleteMedia {
        media_id: String,
    },
    ExternalEvent {
        kind: String,
        details: serde_json::Value,
    },
    WifiStage {
        ssid: String,
        passphrase: String,
    },
    WifiConfirm {
        transaction_id: String,
    },
    WifiRollback {},
    Toss {},
    Led {
        color: Color,
    },
    Tunnel {
        on: bool,
    },
    Night {
        mode: NightMode,
    },
    Volume {
        percent: u8,
    },
    Mute {
        muted: bool,
    },
    Image {
        setting: ImageSetting,
        value: i16,
    },
    Capture {
        #[serde(default)]
        upload: bool,
    },
    Record {
        seconds: u8,
        #[serde(default)]
        upload: bool,
    },
    Video {
        seconds: u8,
        #[serde(default)]
        upload: bool,
    },
    Play {
        media_id: String,
    },
    Upload {
        media_id: String,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    Off,
    Red,
    Yellow,
    Green,
    Cyan,
    Blue,
    Purple,
    Rgb,
}
impl Color {
    pub fn pic(&self) -> &'static str {
        match self {
            Self::Off => "TF+L0",
            Self::Red => "TF+L2",
            Self::Yellow => "TF+L3",
            Self::Green => "TF+L4",
            Self::Cyan => "TF+L7",
            Self::Blue => "TF+L9",
            Self::Purple => "TF+L5",
            Self::Rgb => "TF+L1",
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NightMode {
    Off,
    On,
    Auto,
}
impl NightMode {
    pub fn native_number(&self) -> u32 {
        // cec_image uses a different enum from the unused Oryx image-service API.
        match self {
            Self::Off => 1,
            Self::On => 2,
            Self::Auto => 0,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSetting {
    Brightness,
    Contrast,
    Saturation,
    Sharpness,
    Hue,
    DayNight,
    AntiFlicker,
    WhiteBalance,
}
impl ImageSetting {
    pub fn option(&self) -> &'static str {
        match self {
            Self::Brightness => "--brightness",
            Self::Contrast => "--contrast",
            Self::Saturation => "--saturation",
            Self::Sharpness => "--sharpness",
            Self::Hue => "--hue",
            Self::DayNight => "--day-night-mode",
            Self::AntiFlicker => "--anti-flicker",
            Self::WhiteBalance => "--awb-mode",
        }
    }
    pub fn valid(&self, v: i16) -> bool {
        match self {
            Self::DayNight | Self::AntiFlicker => (0..=1).contains(&v),
            Self::WhiteBalance => (0..=9).contains(&v),
            _ => (-2..=2).contains(&v),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub id: String,
    pub issued_at: i64,
    pub action: Action,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub id: String,
    pub time: String,
    /// Monday=0 through Sunday=6.
    pub days: Vec<u8>,
    pub enabled: bool,
    pub voice_id: Option<String>,
}
impl Schedule {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(valid_id(&self.id), "invalid schedule id");
        anyhow::ensure!(
            self.time.len() == 5 && self.time.as_bytes()[2] == b':',
            "expected HH:MM"
        );
        let h: u8 = self.time[..2].parse()?;
        let m: u8 = self.time[3..].parse()?;
        anyhow::ensure!(
            h < 24 && m < 60 && !self.days.is_empty() && self.days.iter().all(|d| *d < 7),
            "invalid schedule"
        );
        if let Some(id) = &self.voice_id {
            anyhow::ensure!(valid_id(id), "invalid voice id");
        }
        Ok(())
    }
}
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 80
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
pub fn clock_valid() -> bool {
    now() >= 1_704_067_200
}

impl Action {
    pub fn description(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Some(fields) = value.as_object_mut() {
            // Never retain Wi-Fi passwords/SSIDs, external payloads, or schedule details in activity.
            let kind = fields.get("type").cloned().unwrap_or_default();
            if matches!(
                self,
                Self::WifiStage { .. }
                    | Self::ExternalEvent { .. }
                    | Self::SetSchedules { .. }
                    | Self::Configure { .. }
            ) {
                fields.clear();
                fields.insert("type".into(), kind);
            }
        }
        value
    }
}
