// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! On-disk state: the CLI's stable identity, settings, and a device cache.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const APP_DIR_NAME: &str = "landrop-cli";

/// Set by `--config-dir` so the process can run against an alternate profile.
static CONFIG_DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Point this process at an alternate configuration directory.
pub fn set_config_dir_override(path: PathBuf) {
    let _ = CONFIG_DIR_OVERRIDE.set(path);
}

/// `%APPDATA%\landrop-cli` on Windows, `~/.config/landrop-cli` on Linux.
///
/// Overridable via `--config-dir` or the `LANDROP_CLI_CONFIG_DIR` environment
/// variable, which makes the CLI portable (identity on a USB stick) and lets
/// tests run without touching the real profile.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = CONFIG_DIR_OVERRIDE.get() {
        return Ok(dir.clone());
    }
    if let Ok(dir) = std::env::var("LANDROP_CLI_CONFIG_DIR")
        && !dir.trim().is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::config_dir().context("cannot determine the user config directory")?;
    Ok(base.join(APP_DIR_NAME))
}

pub fn identity_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("identity.json"))
}

pub fn settings_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("settings.json"))
}

/// Write a file, restricting permissions to the owner on Unix.
fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    fs::write(path, contents).with_context(|| format!("cannot write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredIdentity {
    /// base64 of the raw 32-byte secp256k1 secret scalar.
    pub sk: String,
    /// base64 of the 33-byte compressed public key, kept for readability.
    pub pk: String,
}

/// Load the CLI's identity, creating and persisting one on first use.
///
/// The identity must be stable: peers key their trust list on the public key, so a
/// fresh key each run would make every peer treat the CLI as a brand-new device and
/// prompt again.
pub fn load_or_create_identity() -> Result<crate::crypto::Identity> {
    let path = identity_path()?;
    if let Ok(raw) = fs::read_to_string(&path)
        && let Ok(stored) = serde_json::from_str::<StoredIdentity>(&raw)
        && let Ok(identity) = crate::crypto::Identity::from_sk_base64(&stored.sk)
    {
        return Ok(identity);
    }
    let identity = crate::crypto::Identity::generate();
    let stored = StoredIdentity {
        sk: identity.sk_base64(),
        pk: identity.pk_base64(),
    };
    let json = serde_json::to_vec_pretty(&stored)?;
    write_private(&path, &json)?;
    Ok(identity)
}

// ---------------------------------------------------------------------------
// Platform facts
// ---------------------------------------------------------------------------

/// The device type strings the protocol uses.
pub fn device_type() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Best-effort hostname, which is what a device advertises when nobody has
/// chosen a name for it.
pub fn hostname() -> String {
    for key in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(value) = std::env::var(key)
            && !value.trim().is_empty()
        {
            return value.trim().to_string();
        }
    }
    for path in ["/etc/hostname", "/proc/sys/kernel/hostname"] {
        if let Ok(value) = fs::read_to_string(path) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    "landrop-cli".to_string()
}

/// Where a device that is *not* our own should put received files.
pub fn default_download_dir() -> Result<PathBuf> {
    let base = dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
        .context("cannot determine a download directory")?;
    Ok(base.join("LANDrop"))
}

// ---------------------------------------------------------------------------
// Settings and device cache
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KnownDevice {
    pub name: String,
    #[serde(rename = "type", default)]
    pub device_type: String,
    pub public_key: String,
    #[serde(default)]
    pub last_address: String,
    #[serde(default)]
    pub last_port: u16,
    #[serde(default)]
    pub last_seen: i64,
}

/// A sender this CLI will accept from without asking.
///
/// Keyed on the sender's long-term secp256k1 public key, which is what stays
/// the same across runs and what a person is really deciding about.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedDevice {
    #[serde(default)]
    pub name: String,
    pub public_key: String,
    #[serde(default)]
    pub added_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    /// Advertised device name. Defaults to the hostname.
    #[serde(default)]
    pub device_name: Option<String>,
    /// Where received files are written. Defaults to `<downloads>/LANDrop`.
    #[serde(default)]
    pub download_dir: Option<PathBuf>,
    /// TCP port for receive mode. 0 means "pick an ephemeral port".
    #[serde(default)]
    pub listening_port: u16,
    /// Devices seen at least once, so `--to <name>` can work without discovery.
    #[serde(default)]
    pub known_devices: Vec<KnownDevice>,
    /// Senders whose transfers are accepted automatically in receive mode.
    #[serde(default)]
    pub trusted_devices: Vec<TrustedDevice>,
}

impl Settings {
    pub fn load() -> Result<Self> {
        let path = settings_path()?;
        match fs::read_to_string(&path) {
            Ok(raw) => Ok(serde_json::from_str(&raw).unwrap_or_default()),
            Err(_) => Ok(Self::default()),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = settings_path()?;
        let json = serde_json::to_vec_pretty(self)?;
        write_private(&path, &json)
    }

    pub fn device_name(&self) -> String {
        self.device_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(hostname)
    }

    pub fn download_dir(&self) -> PathBuf {
        self.download_dir
            .clone()
            .unwrap_or_else(|| default_download_dir().unwrap_or_else(|_| PathBuf::from("LANDrop")))
    }

    /// Record or refresh a device seen during discovery.
    pub fn remember(&mut self, device: &crate::discovery::DiscoveredDevice) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Some(existing) = self
            .known_devices
            .iter_mut()
            .find(|d| d.public_key == device.public_key)
        {
            existing.name = device.name.clone();
            existing.device_type = device.device_type.clone();
            if !device.address.is_empty() {
                existing.last_address = device.address.clone();
            }
            if device.port != 0 {
                existing.last_port = device.port;
            }
            existing.last_seen = now;
        } else {
            self.known_devices.push(KnownDevice {
                name: device.name.clone(),
                device_type: device.device_type.clone(),
                public_key: device.public_key.clone(),
                last_address: device.address.clone(),
                last_port: device.port,
                last_seen: now,
            });
        }
    }

    /// Resolve a user-supplied target: a public key, a `name`, or `host:port`.
    pub fn lookup(&self, query: &str) -> Option<KnownDevice> {
        let query_lower = query.to_ascii_lowercase();
        self.known_devices
            .iter()
            .find(|d| d.public_key == query || d.name.to_ascii_lowercase() == query_lower)
            .cloned()
    }

    // -- trust list ---------------------------------------------------------

    /// Is this sender allowed to transfer without a prompt?
    pub fn is_trusted(&self, public_key: &str) -> bool {
        self.trusted_devices
            .iter()
            .any(|d| d.public_key == public_key)
    }

    pub fn find_trusted(&self, query: &str) -> Option<&TrustedDevice> {
        let query_lower = query.to_ascii_lowercase();
        self.trusted_devices
            .iter()
            .find(|d| d.public_key == query || d.name.to_ascii_lowercase() == query_lower)
    }

    /// Trust a sender. Returns `false` if it was already trusted.
    pub fn add_trusted(&mut self, name: &str, public_key: &str) -> bool {
        if self.is_trusted(public_key) {
            // Refresh a previously blank name rather than duplicating the entry.
            if let Some(existing) = self
                .trusted_devices
                .iter_mut()
                .find(|d| d.public_key == public_key)
                && existing.name.trim().is_empty()
                && !name.trim().is_empty()
            {
                existing.name = name.to_string();
            }
            return false;
        }
        let added_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.trusted_devices.push(TrustedDevice {
            name: name.to_string(),
            public_key: public_key.to_string(),
            added_at,
        });
        true
    }

    /// Remove by name or key. Returns how many entries were removed.
    pub fn remove_trusted(&mut self, query: &str) -> usize {
        let before = self.trusted_devices.len();
        let query_lower = query.to_ascii_lowercase();
        self.trusted_devices
            .retain(|d| d.public_key != query && d.name.to_ascii_lowercase() != query_lower);
        before - self.trusted_devices.len()
    }

    /// Rename entries matched by name or key. Returns how many were renamed.
    ///
    /// Renaming does not change the key, so the trust relationship is untouched —
    /// only the label used to recognise the device in listings and in `--remove`.
    pub fn rename_trusted(&mut self, query: &str, new_name: &str) -> usize {
        let query_lower = query.to_ascii_lowercase();
        let mut renamed = 0;
        for device in &mut self.trusted_devices {
            if device.public_key == query || device.name.to_ascii_lowercase() == query_lower {
                device.name = new_name.to_string();
                renamed += 1;
            }
        }
        renamed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::DiscoveredDevice;

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = Settings {
            device_name: Some("test-box".to_string()),
            ..Settings::default()
        };
        settings.remember(&DiscoveredDevice {
            name: "phone".to_string(),
            device_type: "android".to_string(),
            address: "192.168.1.5".to_string(),
            port: 1234,
            public_key: "KEY1".to_string(),
            discoverable: true,
        });
        fs::write(&path, serde_json::to_vec_pretty(&settings).unwrap()).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        let parsed: Settings = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.device_name(), "test-box");
        assert_eq!(parsed.known_devices.len(), 1);
        assert_eq!(parsed.lookup("phone").unwrap().public_key, "KEY1");
        assert_eq!(parsed.lookup("KEY1").unwrap().last_port, 1234);
        assert!(parsed.lookup("nonexistent").is_none());
    }

    #[test]
    fn remember_updates_in_place() {
        let mut settings = Settings::default();
        let first = DiscoveredDevice {
            name: "old".to_string(),
            device_type: "linux".to_string(),
            address: "10.0.0.1".to_string(),
            port: 1,
            public_key: "K".to_string(),
            discoverable: true,
        };
        settings.remember(&first);
        settings.remember(&DiscoveredDevice {
            name: "new".to_string(),
            ..first
        });
        assert_eq!(
            settings.known_devices.len(),
            1,
            "must not duplicate a device"
        );
        assert_eq!(settings.known_devices[0].name, "new");
    }

    #[test]
    fn device_type_is_known() {
        let t = device_type();
        assert!(["windows", "linux", "macos"].contains(&t));
    }

    #[test]
    fn trust_list_add_lookup_and_remove() {
        let mut settings = Settings::default();
        assert!(!settings.is_trusted("KEY1"));

        assert!(settings.add_trusted("phone", "KEY1"), "first add is new");
        assert!(
            !settings.add_trusted("phone", "KEY1"),
            "second add is a no-op"
        );
        assert_eq!(settings.trusted_devices.len(), 1);
        assert!(settings.is_trusted("KEY1"));

        assert!(settings.add_trusted("laptop", "KEY2"));
        assert_eq!(settings.trusted_devices.len(), 2);

        // Re-adding with an empty name must not duplicate the entry.
        assert!(!settings.add_trusted("", "KEY2"));
        assert_eq!(settings.trusted_devices.len(), 2);

        // A key first trusted anonymously gets its name filled in later.
        assert!(settings.add_trusted("", "KEY3"));
        assert!(!settings.add_trusted("tablet", "KEY3"));
        assert_eq!(settings.trusted_devices.len(), 3);
        assert_eq!(settings.find_trusted("KEY3").unwrap().name, "tablet");

        assert_eq!(settings.find_trusted("phone").unwrap().public_key, "KEY1");
        assert_eq!(settings.find_trusted("laptop").unwrap().public_key, "KEY2");

        // Removal works by name or by key and reports what it actually removed.
        assert_eq!(settings.remove_trusted("phone"), 1);
        assert_eq!(settings.remove_trusted("phone"), 0);
        assert!(!settings.is_trusted("KEY1"));
        assert!(settings.is_trusted("KEY2"));
        assert_eq!(settings.remove_trusted("KEY2"), 1);
        assert_eq!(settings.remove_trusted("KEY3"), 1);
        assert!(settings.trusted_devices.is_empty());
    }

    #[test]
    fn removing_by_name_deletes_every_matching_entry() {
        let mut settings = Settings::default();
        settings.add_trusted("phone", "KEY_A");
        settings.add_trusted("phone", "KEY_B");
        assert_eq!(settings.remove_trusted("phone"), 2);
        assert!(settings.trusted_devices.is_empty());
    }

    #[test]
    fn trust_list_survives_a_save_load_round_trip() {
        let mut settings = Settings::default();
        settings.add_trusted("laptop", "KEYX");
        let json = serde_json::to_vec_pretty(&settings).unwrap();
        let parsed: Settings = serde_json::from_slice(&json).unwrap();
        assert!(parsed.is_trusted("KEYX"));
        assert_eq!(parsed.trusted_devices[0].name, "laptop");
    }

    #[test]
    fn unknown_settings_fields_still_load() {
        // Forward compatibility: a newer version's extra keys must not break us.
        let raw = r#"{"device_name":"x","trusted_devices":[
            {"name":"a","public_key":"K","added_at":1,"future_field":true}]}"#;
        let parsed: Settings = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.device_name(), "x");
        assert!(parsed.is_trusted("K"));
    }

    #[test]
    fn rename_trusted_changes_the_label_but_not_the_key() {
        let mut settings = Settings::default();
        settings.add_trusted("old-name", "KEY1");
        settings.add_trusted("other", "KEY2");

        assert_eq!(settings.rename_trusted("old-name", "new-name"), 1);
        assert_eq!(settings.find_trusted("KEY1").unwrap().name, "new-name");
        assert!(
            settings.is_trusted("KEY1"),
            "renaming must not touch the trust relationship"
        );
        assert_eq!(
            settings.trusted_devices.len(),
            2,
            "nothing added or removed"
        );

        // Keys are matched exactly, names case-insensitively.
        assert_eq!(settings.rename_trusted("key2", "Third"), 0);
        assert_eq!(settings.rename_trusted("OTHER", "third"), 1);
        assert_eq!(settings.find_trusted("KEY2").unwrap().name, "third");

        assert_eq!(settings.rename_trusted("nobody", "x"), 0);
    }
}
