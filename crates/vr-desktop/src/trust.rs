//! Trust settings use public keys, never a device name or a connection code.
use std::{fs, io, io::Write, path::Path};

use sha2::{Digest, Sha256};
use vr_crypto::{DevicePublicKey, DeviceSigningKey};

const PREFIX: &str = "VR-PAIR1:";
const MAX_DEVICES: usize = 32;

#[derive(Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(super) struct Trust {
    pub required: bool,
    devices: Vec<String>,
}

impl Trust {
    pub fn devices(&self) -> &[String] {
        &self.devices
    }

    pub fn allowed(&self) -> Option<Vec<DevicePublicKey>> {
        self.required.then(|| {
            self.devices
                .iter()
                .take(MAX_DEVICES)
                .filter_map(|key| DevicePublicKey::parse(key).ok())
                .collect()
        })
    }

    pub fn add(&mut self, pairing: &str) -> Result<(), &'static str> {
        let key = pairing
            .trim()
            .strip_prefix(PREFIX)
            .ok_or("请输入 VR-PAIR1: 开头的配对信息。")?;
        DevicePublicKey::parse(key).map_err(|_| "配对信息无效，请重新复制。")?;
        if self.devices.iter().any(|existing| existing == key) {
            return Err("这台设备已经在名单中。");
        }
        if self.devices.len() >= MAX_DEVICES {
            return Err("可信设备名单已满，请先移除不再使用的设备。");
        }
        self.devices.push(key.into());
        Ok(())
    }

    pub fn remove(&mut self, key: &str) {
        self.devices.retain(|entry| entry != key);
    }
}

pub(super) fn pairing_code(key: &DeviceSigningKey) -> String {
    format!("{PREFIX}{}", key.public_key())
}

pub(super) fn fingerprint(public_key: &str) -> String {
    Sha256::digest(public_key.as_bytes())
        .chunks(4)
        .map(|chunk| {
            chunk
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Atomic creation: two desktop instances must not get different identities.
/// On Unix the temporary file is owner-only from the moment it is created.
/// On Windows it inherits the ACL of the current user's state directory.
pub(super) fn load_or_create(dir: &Path) -> anyhow::Result<DeviceSigningKey> {
    let path = dir.join("viewer-identity.key");
    let read = || -> anyhow::Result<_> {
        let meta = fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            meta.is_file() && meta.len() == 32,
            "invalid viewer identity file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            anyhow::ensure!(
                meta.permissions().mode() & 0o077 == 0,
                "viewer identity is not private"
            );
        }
        Ok(DeviceSigningKey::from_secret_bytes(&fs::read(&path)?)?)
    };
    match fs::symlink_metadata(&path) {
        Ok(_) => return read(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    fs::create_dir_all(dir)?;
    let key = DeviceSigningKey::generate();
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(&key.secret_bytes())?;
    file.as_file().sync_all()?;
    match file.persist_noclobber(&path) {
        Ok(_) => Ok(key),
        Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => read(),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_keys_are_stable_and_bad_keys_are_not_silently_rotated() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create(dir.path()).unwrap();
        assert_eq!(
            first.public_key(),
            load_or_create(dir.path()).unwrap().public_key()
        );
        let path = dir.path().join("viewer-identity.key");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::write(path, b"invalid").unwrap();
        assert!(load_or_create(dir.path()).is_err());
    }

    #[test]
    fn restrictions_remain_enabled_when_the_last_device_is_removed() {
        let key = DeviceSigningKey::generate();
        let mut trust = Trust::default();
        assert!(trust.allowed().is_none());
        assert!(trust.add("123456789012").is_err());
        assert!(trust.add("VR-PAIR1:garbage").is_err());
        assert!(trust.add(&pairing_code(&key)).is_ok());
        assert!(trust.add(&pairing_code(&key)).is_err());
        trust.required = true;
        let saved = serde_json::to_string(&trust).unwrap();
        let mut trust = serde_json::from_str::<Trust>(&saved).unwrap();
        assert_eq!(trust.allowed().unwrap().len(), 1);
        trust.remove(&key.public_key());
        assert!(trust.allowed().is_some_and(|keys| keys.is_empty()));
    }
}
