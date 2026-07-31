use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{AppError, AppResult};

const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_SHA256";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredIdentity {
    private_key: Vec<u8>,
    public_key: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct DeviceIdentity {
    pub private_key: Vec<u8>,
    pub public_key: Vec<u8>,
}

impl DeviceIdentity {
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public_key)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedDevice {
    pub device_id: Uuid,
    pub name: String,
    pub public_key: Vec<u8>,
    pub fingerprint: String,
    pub created_at: u64,
    pub last_seen_at: u64,
}

pub fn load_or_create_identity(path: &Path) -> AppResult<DeviceIdentity> {
    if path.exists() {
        let protected = fs::read(path)?;
        let encoded = unprotect(&protected)?;
        let stored: StoredIdentity = serde_json::from_slice(&encoded)?;
        validate_identity(&stored)?;
        return Ok(DeviceIdentity {
            private_key: stored.private_key,
            public_key: stored.public_key,
        });
    }

    let params = NOISE_PATTERN
        .parse()
        .map_err(|error| AppError::Security(format!("Noise 参数无效：{error}")))?;
    let keypair = snow::Builder::new(params)
        .generate_keypair()
        .map_err(|error| AppError::Security(format!("无法生成设备身份密钥：{error}")))?;
    let stored = StoredIdentity {
        private_key: keypair.private,
        public_key: keypair.public,
    };
    validate_identity(&stored)?;
    let protected = protect(&serde_json::to_vec(&stored)?)?;
    atomic_write(path, &protected)?;
    Ok(DeviceIdentity {
        private_key: stored.private_key,
        public_key: stored.public_key,
    })
}

pub fn load_trusted_devices(path: &Path) -> Vec<TrustedDevice> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_trusted_devices(path: &Path, devices: &[TrustedDevice]) -> AppResult<()> {
    atomic_write(path, &serde_json::to_vec_pretty(devices)?)
}

pub fn fingerprint(public_key: &[u8]) -> String {
    let digest = Sha256::digest(public_key);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

pub fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn validate_identity(identity: &StoredIdentity) -> AppResult<()> {
    if identity.private_key.len() != 32 || identity.public_key.len() != 32 {
        return Err(AppError::Security(
            "设备身份密钥长度无效，请恢复原身份文件或重新安装后配对".into(),
        ));
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = temporary_path(path);
    fs::write(&temporary, bytes)?;
    if !path.exists() {
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        return Ok(());
    }

    let backup = backup_path(path);
    fs::rename(path, &backup)?;
    if let Err(error) = fs::rename(&temporary, path) {
        let restore_result = fs::rename(&backup, path);
        let _ = fs::remove_file(&temporary);
        return match restore_result {
            Ok(()) => Err(error.into()),
            Err(restore_error) => Err(AppError::Security(format!(
                "更新身份数据失败，旧数据保留在 {}，自动恢复也失败：{restore_error}",
                backup.display()
            ))),
        };
    }
    fs::remove_file(backup)?;
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|value| value.to_os_string())
        .unwrap_or_else(|| "identity".into());
    name.push(format!(".{}.tmp", Uuid::new_v4()));
    path.with_file_name(name)
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|value| value.to_os_string())
        .unwrap_or_else(|| "identity".into());
    name.push(format!(".{}.bak", Uuid::new_v4()));
    path.with_file_name(name)
}

#[cfg(target_os = "windows")]
fn protect(bytes: &[u8]) -> AppResult<Vec<u8>> {
    use std::slice;
    use windows::{
        Win32::{
            Foundation::{HLOCAL, LocalFree},
            Security::Cryptography::{
                CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
            },
        },
        core::PCWSTR,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len())
            .map_err(|_| AppError::Security("身份密钥数据过大".into()))?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|error| AppError::Security(format!("DPAPI 加密身份密钥失败：{error}")))?;
        let result = slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(result)
    }
}

#[cfg(target_os = "windows")]
fn unprotect(bytes: &[u8]) -> AppResult<Vec<u8>> {
    use std::slice;
    use windows::Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
        },
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len())
            .map_err(|_| AppError::Security("身份密钥数据过大".into()))?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|error| AppError::Security(format!("DPAPI 解密身份密钥失败：{error}")))?;
        let result = slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(result)
    }
}

#[cfg(not(target_os = "windows"))]
fn protect(bytes: &[u8]) -> AppResult<Vec<u8>> {
    Ok(bytes.to_vec())
}

#[cfg(not(target_os = "windows"))]
fn unprotect(bytes: &[u8]) -> AppResult<Vec<u8>> {
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_and_full_length() {
        let first = fingerprint(&[7; 32]);
        let second = fingerprint(&[7; 32]);
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn trust_store_round_trip_preserves_identity() {
        let temporary = tempfile::tempdir().expect("应创建测试目录");
        let path = temporary.path().join("trusted.json");
        let expected = vec![TrustedDevice {
            device_id: Uuid::nil(),
            name: "测试设备".into(),
            public_key: vec![3; 32],
            fingerprint: fingerprint(&[3; 32]),
            created_at: 1,
            last_seen_at: 2,
        }];

        save_trusted_devices(&path, &expected).expect("应保存信任记录");
        let actual = load_trusted_devices(&path);

        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].device_id, Uuid::nil());
        assert_eq!(actual[0].public_key, vec![3; 32]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn identity_is_dpapi_protected_and_reusable() {
        let temporary = tempfile::tempdir().expect("应创建测试目录");
        let path = temporary.path().join("identity.bin");
        let first = load_or_create_identity(&path).expect("应生成身份");
        let raw = fs::read(&path).expect("应读取身份文件");

        assert!(
            !raw.windows(first.private_key.len())
                .any(|value| value == first.private_key)
        );

        let second = load_or_create_identity(&path).expect("应读取身份");
        assert_eq!(first.private_key, second.private_key);
        assert_eq!(first.public_key, second.public_key);
    }
}
