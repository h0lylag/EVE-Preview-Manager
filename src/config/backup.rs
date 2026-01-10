//! Configuration Backup Manager
//!
//! Handles creation, restoration, and management of configuration backups.
//! Backups are stored as .tar.gz archives in a 'backups' subdirectory.

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use tracing::{error, info};

use crate::config::profile::Config;

/// Represents a backup file
#[derive(Debug, Clone)]
pub struct BackupEntry {
    pub filename: String,
    pub path: PathBuf,
    pub timestamp: SystemTime,
    pub is_manual: bool,
}

pub struct BackupManager;

impl BackupManager {
    /// Get the path to the backup directory
    fn backup_dir() -> PathBuf {
        let mut path = Config::path();
        path.pop(); // Remove filename
        path.push(crate::common::constants::config::backup::SUBDIR);
        path
    }

    /// Create a new backup of the configuration directory
    pub fn create_backup(is_manual: bool) -> Result<PathBuf> {
        let config_file_path = Config::path();

        // Ensure backup directory exists
        let backup_dir = Self::backup_dir();
        if !backup_dir.exists() {
            fs::create_dir_all(&backup_dir).context("Failed to create backup directory")?;
        }

        // Generate filename: config_backup_[manual_]YYYYMMDD_HHMMSS.tar.gz
        let now = SystemTime::now();
        let datetime: chrono::DateTime<chrono::Local> = now.into();
        let timestamp_str = datetime.format("%Y%m%d_%H%M%S_%3f").to_string();

        let prefix = if is_manual { "manual_" } else { "" };
        let filename = format!("config_backup_{}{}.tar.gz", prefix, timestamp_str);
        let backup_path = backup_dir.join(&filename);

        // Create tar.gz archive
        let tar_gz = fs::File::create(&backup_path).context("Failed to create backup file")?;
        let enc = GzEncoder::new(tar_gz, Compression::default());
        let mut tar = tar::Builder::new(enc);

        // Add config.json to archive
        // We only backup the config file for now, but could extend to entire dir if needed
        // (excluding the backups dir itself to avoid recursion)
        match fs::File::open(&config_file_path) {
            Ok(mut file) => {
                tar.append_file(crate::common::constants::config::FILENAME, &mut file)
                    .context("Failed to add config file to archive")?;
            }
            Err(e) => {
                // It's possible the config file doesn't exist yet (fresh install)
                // In that case, we can try to save the current in-memory config first?
                // But this function is usually called when app is running.
                return Err(anyhow::anyhow!(
                    "Failed to open config file for backup: {}",
                    e
                ));
            }
        }

        tar.finish().context("Failed to finish backup archive")?;

        info!("Created backup: {:?}", backup_path);
        Ok(backup_path)
    }

    /// List all available backups, sorted by date (newest first)
    pub fn list_backups() -> Result<Vec<BackupEntry>> {
        let backup_dir = Self::backup_dir();
        if !backup_dir.exists() {
            return Ok(Vec::new());
        }

        let mut backups = Vec::new();

        for entry in fs::read_dir(backup_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("gz") {
                let metadata = fs::metadata(&path)?;
                let timestamp = metadata.modified().unwrap_or(SystemTime::now());
                let filename = entry.file_name().to_string_lossy().to_string();

                let is_manual = filename.contains("_manual_");

                backups.push(BackupEntry {
                    filename,
                    path,
                    timestamp,
                    is_manual,
                });
            }
        }

        // Sort by timestamp descending (newest first)
        backups.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        Ok(backups)
    }

    /// Restore configuration from a specific backup
    pub fn restore_backup(filename: &str) -> Result<()> {
        let backup_path = Self::backup_dir().join(filename);
        if !backup_path.exists() {
            return Err(anyhow::anyhow!("Backup file not found: {}", filename));
        }

        let tar_gz = fs::File::open(&backup_path).context("Failed to open backup file")?;
        let dec = GzDecoder::new(tar_gz);
        let mut archive = tar::Archive::new(dec);

        let config_dir = Config::path()
            .parent()
            .context("Failed to get config directory")?
            .to_path_buf();

        // Unpack into config dir
        archive
            .unpack(&config_dir)
            .context("Failed to unpack backup")?;

        info!("Restored backup: {}", filename);
        Ok(())
    }

    /// Delete a specific backup file
    pub fn delete_backup(filename: &str) -> Result<()> {
        let backup_path = Self::backup_dir().join(filename);
        if backup_path.exists() {
            fs::remove_file(&backup_path)
                .context(format!("Failed to delete backup file: {}", filename))?;
            info!("Deleted backup: {}", filename);
        }
        Ok(())
    }

    /// Prune old backups based on retention count
    /// Only affects auto-backups (not manual ones)
    pub fn prune_backups(retention_count: u32) -> Result<()> {
        let backups = Self::list_backups()?;

        // Filter for only auto backups
        let auto_backups: Vec<&BackupEntry> = backups.iter().filter(|b| !b.is_manual).collect();

        if auto_backups.len() > retention_count as usize {
            let to_remove = &auto_backups[retention_count as usize..];
            for backup in to_remove {
                if let Err(e) = fs::remove_file(&backup.path) {
                    error!("Failed to prune backup {:?}: {}", backup.path, e);
                } else {
                    info!("Pruned old backup: {:?}", backup.filename);
                }
            }
        }
        Ok(())
    }

    /// Check if an automatic backup should run
    pub fn should_run_auto_backup(interval_days: u32) -> bool {
        if interval_days == 0 {
            return false;
        }

        let backups = match Self::list_backups() {
            Ok(b) => b,
            Err(_) => return true, // If we can't list, assume we need one? Or fail safe.
        };

        // Find newest auto-backup
        let newest_auto = backups.iter().find(|b| !b.is_manual);

        match newest_auto {
            Some(backup) => {
                let now = SystemTime::now();
                match now.duration_since(backup.timestamp) {
                    Ok(duration) => {
                        let days_since = duration.as_secs() / 86400;
                        days_since >= interval_days as u64
                    }
                    Err(_) => true, // Time moved backwards? Run backup.
                }
            }
            None => true, // No auto backups exist
        }
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_backup_logic() {
        // Setup temp environment
        let temp_dir = tempfile::tempdir().unwrap();
        let app_dir = temp_dir.path().join("eve-preview-manager");
        fs::create_dir_all(&app_dir).unwrap();

        let config_path = app_dir.join("config.json");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(b"{\"test\": true}").unwrap();

        // Mock Config path by setting env var (Config::path() checks this)
        unsafe {
            std::env::set_var("EVE_PREVIEW_MANAGER_CONFIG_DIR", app_dir.to_str().unwrap());
        }

        // 1. Test Creation
        let backup_path = BackupManager::create_backup(false).unwrap();
        assert!(backup_path.exists());
        assert!(backup_path.to_string_lossy().contains("config_backup_"));
        assert!(!backup_path.to_string_lossy().contains("manual"));

        // Manual backup
        let manual_backup = BackupManager::create_backup(true).unwrap();
        assert!(manual_backup.to_string_lossy().contains("manual_"));

        // 2. Test Listing
        let list = BackupManager::list_backups().unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].timestamp >= list[1].timestamp); // Sorted newest first

        // 3. Test Restoration
        // Modify config first
        {
            let mut f = fs::File::create(&config_path).unwrap();
            f.write_all(b"{\"modified\": true}").unwrap();
        }

        BackupManager::restore_backup(&list[0].filename).unwrap();
        let content = fs::read_to_string(&config_path).unwrap();
        assert_eq!(content, "{\"test\": true}");

        // 4. Test Pruning
        // Create a few more dummy backups
        // Note: files created too fast might have same timestamp, but prune depends on list order
        // To ensure they are treated as "old", we can just rely on the count since we just made them.
        for _ in 0..5 {
            BackupManager::create_backup(false).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let list_before = BackupManager::list_backups().unwrap();
        // Total: 2 initial (1 manual, 1 auto) + 5 new auto = 7 total. 6 auto.

        let auto_count = list_before.iter().filter(|b| !b.is_manual).count();
        assert_eq!(auto_count, 6);

        // Retention 3
        BackupManager::prune_backups(3).unwrap();

        let list_after = BackupManager::list_backups().unwrap();
        let auto_after = list_after.iter().filter(|b| !b.is_manual).count();
        assert_eq!(auto_after, 3);

        // Manual backup should still exist
        assert!(list_after.iter().any(|b| b.is_manual));

        // 5. Test Deletion
        let target = &list_after[0].filename;
        BackupManager::delete_backup(target).unwrap();
        let list_final = BackupManager::list_backups().unwrap();
        assert!(!list_final.iter().any(|b| b.filename == *target));

        unsafe {
            std::env::remove_var("EVE_PREVIEW_MANAGER_CONFIG_DIR");
        }
    }
}
