use crate::{
    auth::{ApiKey, User},
    model::Schedule,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

#[derive(Clone, Deserialize, Serialize)]
pub struct Seen {
    pub id: String,
    pub timestamp: i64,
    pub result: serde_json::Value,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Database {
    pub users: BTreeMap<String, User>,
    pub api_keys: Vec<ApiKey>,
    pub schedules: Vec<Schedule>,
    pub seen: Vec<Seen>,
    pub last_feed: i64,
    #[serde(default)]
    pub pending_uploads: Vec<String>,
    #[serde(default)]
    pub outbox: Vec<serde_json::Value>,
    pub volume: u8,
    pub muted: bool,
    pub night: crate::model::NightMode,
}
impl Default for Database {
    fn default() -> Self {
        Self {
            users: BTreeMap::new(),
            api_keys: vec![],
            schedules: vec![],
            seen: vec![],
            last_feed: 0,
            pending_uploads: vec![],
            outbox: vec![],
            volume: 25,
            muted: false,
            night: crate::model::NightMode::Auto,
        }
    }
}
impl Database {
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("state.json");
        ensure!(
            fs::metadata(&path)?.len() < 1_048_576,
            "state file too large"
        );
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }
    pub fn save(&self, dir: &Path) -> Result<()> {
        atomic_write(&dir.join("state.json"), &serde_json::to_vec(self)?)
    }
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("new");
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    std::fs::File::open(path.parent().context("parent")?)?.sync_all()?;
    Ok(())
}
pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "directory must not be a symlink"
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
