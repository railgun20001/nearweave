use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::error::AppResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UserSettings {
    pub clipboard_enabled: bool,
    pub lan_enabled: bool,
    pub lan_setup_decided: bool,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            clipboard_enabled: true,
            lan_enabled: false,
            lan_setup_decided: false,
        }
    }
}

pub fn load(path: &Path) -> UserSettings {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, settings: &UserSettings) -> AppResult<()> {
    fs::write(path, serde_json::to_vec_pretty(settings)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_preserves_disabled_clipboard_preference() {
        let temporary = tempfile::tempdir().expect("应创建测试目录");
        let path = temporary.path().join("settings.json");
        let expected = UserSettings {
            clipboard_enabled: false,
            lan_enabled: true,
            lan_setup_decided: true,
        };

        save(&path, &expected).expect("设置应写入");

        assert!(!load(&path).clipboard_enabled);
        assert!(load(&path).lan_enabled);
        assert!(load(&path).lan_setup_decided);
    }

    #[test]
    fn missing_settings_enable_clipboard_by_default() {
        let temporary = tempfile::tempdir().expect("应创建测试目录");
        let path = temporary.path().join("settings.json");

        assert!(load(&path).clipboard_enabled);
        assert!(!load(&path).lan_enabled);
        assert!(!load(&path).lan_setup_decided);
    }

    #[test]
    fn invalid_settings_fall_back_to_default_enabled() {
        let temporary = tempfile::tempdir().expect("应创建测试目录");
        let path = temporary.path().join("settings.json");
        fs::write(&path, b"not-json").expect("应写入损坏配置");

        assert!(load(&path).clipboard_enabled);
        assert!(!load(&path).lan_enabled);
        assert!(!load(&path).lan_setup_decided);
    }

    #[test]
    fn legacy_listening_preference_is_ignored() {
        let temporary = tempfile::tempdir().expect("应创建测试目录");
        let path = temporary.path().join("settings.json");
        fs::write(
            &path,
            br#"{"clipboardEnabled":false,"listeningEnabled":false}"#,
        )
        .expect("应写入旧版设置");

        assert!(!load(&path).clipboard_enabled);
        assert!(!load(&path).lan_enabled);
        assert!(!load(&path).lan_setup_decided);
    }
}
