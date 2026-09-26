//! Configuration Backup Manager
//!
//! Handles creation, restoration, and management of configuration backups.
//! Backups are stored as .tar.gz archives in a 'backups' subdirectory.

use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use flate2::Compression;
use flate2::bufread::GzDecoder;
use flate2::write::GzEncoder;
use tracing::{error, info};

use crate::config::profile::Config;

// Config files are normally small; cap the restored payload allocation.
const MAX_RESTORE_CONFIG_BYTES: u64 = 16 * 1024 * 1024;
// Tar archives may include zero padding after their end marker. Bound how much
// padding is consumed while still reading through the gzip integrity footer.
const MAX_RESTORE_TAR_PADDING_BYTES: usize = 1024 * 1024;
const RESTORE_READ_BUFFER_BYTES: usize = 8 * 1024;

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
    fn backup_dir(config_path: Option<&Path>) -> PathBuf {
        let mut path = config_path
            .map(|p| p.to_path_buf())
            .unwrap_or_else(Config::path);
        path.pop(); // Remove filename
        path.push(crate::common::constants::config::backup::SUBDIR);
        path
    }

    /// Create a new backup containing one canonical `config.json` entry.
    ///
    /// Existing archive paths are never overwritten, and incomplete output is
    /// removed if tar or gzip finalization fails.
    pub fn create_backup(is_manual: bool, config_path_override: Option<&Path>) -> Result<PathBuf> {
        let config_file_path = config_path_override
            .map(|p| p.to_path_buf())
            .unwrap_or_else(Config::path);

        let backup_dir = Self::backup_dir(config_path_override);
        fs::create_dir_all(&backup_dir).context("Failed to create backup directory")?;

        // Generate filename: [auto|manual]_backup_YYYYMMDD_HHMMSS.tar.gz
        let now = SystemTime::now();
        let datetime: chrono::DateTime<chrono::Local> = now.into();
        let timestamp_str = datetime.format("%Y%m%d_%H%M%S").to_string();

        let prefix = if is_manual {
            "manual_backup"
        } else {
            "auto_backup"
        };
        let filename = format!("{}_{}.tar.gz", prefix, timestamp_str);
        let backup_path = backup_dir.join(&filename);

        let mut config_file = fs::File::open(&config_file_path).with_context(|| {
            format!(
                "Failed to open config file for backup: {}",
                config_file_path.display()
            )
        })?;
        let tar_gz = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup_path)
            .context("Failed to create backup file")?;

        let archive_result = (|| -> Result<()> {
            let enc = GzEncoder::new(tar_gz, Compression::default());
            let mut tar = tar::Builder::new(enc);

            // The restore path intentionally accepts only this canonical entry.
            tar.append_file(crate::common::constants::config::FILENAME, &mut config_file)
                .context("Failed to add config file to archive")?;

            let enc = tar
                .into_inner()
                .context("Failed to finish backup archive")?;
            enc.finish()
                .context("Failed to finish backup compression")?;
            Ok(())
        })();

        if let Err(archive_error) = archive_result {
            if let Err(cleanup_error) = fs::remove_file(&backup_path) {
                error!(
                    path = ?backup_path,
                    error = %cleanup_error,
                    "Failed to remove incomplete backup"
                );
            }
            return Err(archive_error);
        }

        info!(path = ?backup_path, "Created backup");
        Ok(backup_path)
    }

    /// List regular `.tar.gz` backup candidates, sorted newest first.
    ///
    /// Symlinks and other non-regular file types are excluded. Only generated
    /// `auto_backup_` names participate in automatic pruning.
    pub fn list_backups(config_path_override: Option<&Path>) -> Result<Vec<BackupEntry>> {
        let backup_dir = Self::backup_dir(config_path_override);
        if !backup_dir.exists() {
            return Ok(Vec::new());
        }

        let mut backups = Vec::new();

        for entry in fs::read_dir(backup_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }

            let Ok(filename) = entry.file_name().into_string() else {
                continue;
            };
            if !filename.ends_with(".tar.gz") {
                continue;
            }
            // Only application-generated auto backups participate in pruning;
            // manually copied or renamed archives are retained like manual backups.
            let is_manual = !filename.starts_with("auto_backup_");

            let metadata = entry.metadata()?;
            let timestamp = metadata.modified().unwrap_or(SystemTime::now());
            backups.push(BackupEntry {
                filename,
                path: entry.path(),
                timestamp,
                is_manual,
            });
        }

        backups.sort_by_key(|backup| std::cmp::Reverse(backup.timestamp));

        Ok(backups)
    }

    /// Restore `config.json` from a listed, strictly validated backup archive.
    pub fn restore_backup(filename: &str, config_path_override: Option<&Path>) -> Result<()> {
        let backup_path = Self::backup_path_for_existing_backup(filename, config_path_override)?;
        let config_contents = Self::read_config_from_backup(&backup_path)?;

        let config_file_path = config_path_override
            .map(|p| p.to_path_buf())
            .unwrap_or_else(Config::path);

        crate::config::write_atomically(&config_file_path, &config_contents)
            .with_context(|| format!("Failed to restore config to {:?}", config_file_path))?;

        info!(filename, "Restored backup");
        Ok(())
    }

    fn backup_path_for_existing_backup(
        filename: &str,
        config_path_override: Option<&Path>,
    ) -> Result<PathBuf> {
        Self::validate_backup_filename(filename)?;

        let backups = Self::list_backups(config_path_override).context("Failed to list backups")?;
        backups
            .into_iter()
            .find(|backup| backup.filename == filename)
            .map(|backup| backup.path)
            .ok_or_else(|| anyhow::anyhow!("Backup file not found: {}", filename))
    }

    fn validate_backup_filename(filename: &str) -> Result<()> {
        if filename.is_empty() || filename.contains('/') || filename.contains('\\') {
            return Err(anyhow::anyhow!("Invalid backup filename: {}", filename));
        }

        let mut components = Path::new(filename).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(_)), None) => Ok(()),
            _ => Err(anyhow::anyhow!("Invalid backup filename: {}", filename)),
        }
    }

    fn read_config_from_backup(backup_path: &Path) -> Result<Vec<u8>> {
        let tar_gz = fs::File::open(backup_path).context("Failed to open backup file")?;
        let dec = GzDecoder::new(BufReader::new(tar_gz));
        let mut archive = tar::Archive::new(dec);

        let mut config_contents = None;
        // Raw iteration exposes PAX/GNU metadata entries instead of buffering their
        // payloads internally. Backups created by this application do not need
        // extensions, so reject them before their payloads are read.
        for entry in archive
            .entries()
            .context("Failed to read backup archive")?
            .raw(true)
        {
            let entry = entry.context("Failed to read backup archive entry")?;
            let entry_path = entry
                .path()
                .context("Failed to read backup archive entry path")?
                .into_owned();
            let entry_path_bytes = entry.path_bytes().into_owned();
            Self::validate_restore_entry_path(&entry_path, &entry_path_bytes)?;

            let entry_type = entry.header().entry_type();
            if !entry_type.is_file() {
                return Err(anyhow::anyhow!(
                    "Backup archive contains non-file entry: {}",
                    entry_path.display()
                ));
            }

            if config_contents.is_some() {
                return Err(anyhow::anyhow!(
                    "Backup archive contains duplicate {} entry",
                    crate::common::constants::config::FILENAME
                ));
            }

            // In raw mode this is the size of the current physical entry; metadata
            // extension entries have already been rejected as separate entries.
            let size = entry.size();
            if size > MAX_RESTORE_CONFIG_BYTES {
                return Err(anyhow::anyhow!(
                    "Backup config is too large to restore: {} bytes",
                    size
                ));
            }

            let capacity = usize::try_from(size).context("Backup config size is unsupported")?;
            let mut contents = Vec::with_capacity(capacity);
            entry
                .take(MAX_RESTORE_CONFIG_BYTES + 1)
                .read_to_end(&mut contents)
                .context("Failed to read backup config")?;
            if contents.len() != capacity {
                return Err(anyhow::anyhow!(
                    "Backup config size does not match archive metadata"
                ));
            }
            config_contents = Some(contents);
        }

        // Tar iteration stops at its first zero header block. Read the bounded
        // remaining padding to force gzip checksum and length validation.
        let mut decoder = archive.into_inner();
        let mut buffer = [0_u8; RESTORE_READ_BUFFER_BYTES];
        let mut trailing_bytes = 0;
        loop {
            // Permit one sentinel byte beyond the limit so exact-limit input
            // still performs the EOF read that validates the gzip footer.
            let remaining_with_sentinel = MAX_RESTORE_TAR_PADDING_BYTES - trailing_bytes + 1;
            let read_limit = buffer.len().min(remaining_with_sentinel);
            let bytes_read = decoder
                .read(&mut buffer[..read_limit])
                .context("Failed to verify backup gzip integrity")?;
            if bytes_read == 0 {
                break;
            }

            trailing_bytes += bytes_read;
            if trailing_bytes > MAX_RESTORE_TAR_PADDING_BYTES {
                return Err(anyhow::anyhow!(
                    "Backup archive contains excessive trailing padding"
                ));
            }
            if buffer[..bytes_read].iter().any(|byte| *byte != 0) {
                return Err(anyhow::anyhow!(
                    "Backup archive contains data after its end marker"
                ));
            }
        }

        let mut compressed_input = decoder.into_inner();
        if !compressed_input
            .fill_buf()
            .context("Failed to verify end of backup file")?
            .is_empty()
        {
            return Err(anyhow::anyhow!(
                "Backup file contains data after its gzip stream"
            ));
        }

        let config_contents = config_contents.ok_or_else(|| {
            anyhow::anyhow!(
                "Backup archive does not contain {}",
                crate::common::constants::config::FILENAME
            )
        })?;

        let restored_config: Config = serde_json::from_slice(&config_contents)
            .context("Backup archive contains an invalid configuration")?;
        if restored_config.profiles.is_empty() {
            return Err(anyhow::anyhow!(
                "Backup configuration does not contain any profiles"
            ));
        }

        Ok(config_contents)
    }

    fn validate_restore_entry_path(path: &Path, path_bytes: &[u8]) -> Result<()> {
        // Require the literal archive name; variants like "./config.json" stay invalid.
        if path_bytes != crate::common::constants::config::FILENAME.as_bytes() {
            return Err(anyhow::anyhow!(
                "Backup archive contains unexpected entry: {}",
                path.display()
            ));
        }

        if path.is_absolute() {
            return Err(anyhow::anyhow!(
                "Backup archive contains absolute entry: {}",
                path.display()
            ));
        }

        let mut components = path.components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(name)), None)
                if name == OsStr::new(crate::common::constants::config::FILENAME) =>
            {
                Ok(())
            }
            _ => Err(anyhow::anyhow!(
                "Backup archive contains unsafe entry: {}",
                path.display()
            )),
        }
    }

    /// Delete a regular `.tar.gz` file returned by [`Self::list_backups`].
    pub fn delete_backup(filename: &str, config_path_override: Option<&Path>) -> Result<()> {
        let backup_path = Self::backup_path_for_existing_backup(filename, config_path_override)?;
        fs::remove_file(&backup_path)
            .with_context(|| format!("Failed to delete backup file: {}", filename))?;
        info!(filename, "Deleted backup");
        Ok(())
    }

    /// Prune old backups based on retention count
    /// Only affects auto-backups (not manual ones)
    pub fn prune_backups(retention_count: u32, config_path_override: Option<&Path>) -> Result<()> {
        let backups = Self::list_backups(config_path_override)?;

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
    pub fn should_run_auto_backup(interval_days: u32, config_path_override: Option<&Path>) -> bool {
        if interval_days == 0 {
            return false;
        }

        let backups = match Self::list_backups(config_path_override) {
            Ok(b) => b,
            // Prefer attempting a backup when retention state cannot be read.
            Err(_) => return true,
        };

        let newest_auto = backups.iter().find(|b| !b.is_manual);

        match newest_auto {
            Some(backup) => {
                let now = SystemTime::now();
                match now.duration_since(backup.timestamp) {
                    Ok(duration) => {
                        let days_since = duration.as_secs() / 86400;
                        days_since >= interval_days as u64
                    }
                    // A future timestamp should not suppress backups indefinitely.
                    Err(_) => true,
                }
            }
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read, Seek, Write};
    use std::path::{Path, PathBuf};

    const TAR_BLOCK_BYTES: usize = 512;

    fn setup_config(contents: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let temp_dir = tempfile::tempdir().unwrap();
        let app_dir = temp_dir.path().join("eve-preview-manager");
        fs::create_dir_all(&app_dir).unwrap();

        let config_path = app_dir.join(crate::common::constants::config::FILENAME);
        fs::write(&config_path, contents).unwrap();

        (temp_dir, config_path)
    }

    fn write_backup_archive(
        config_path: &Path,
        filename: &str,
        entries: &[(&str, &[u8])],
    ) -> PathBuf {
        let backup_dir = BackupManager::backup_dir(Some(config_path));
        fs::create_dir_all(&backup_dir).unwrap();
        let backup_path = backup_dir.join(filename);

        let tar_gz = fs::File::create(&backup_path).unwrap();
        let enc = GzEncoder::new(tar_gz, Compression::default());
        let mut tar = tar::Builder::new(enc);

        for (path, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_size(contents.len() as u64);
            header.set_cksum();
            let mut contents = Cursor::new(*contents);
            tar.append(&header, &mut contents).unwrap();
        }

        let enc = tar.into_inner().unwrap();
        enc.finish().unwrap();
        backup_path
    }

    fn write_raw_path_backup_archive(
        config_path: &Path,
        filename: &str,
        entry_path: &[u8],
        contents: &[u8],
    ) -> PathBuf {
        let backup_dir = BackupManager::backup_dir(Some(config_path));
        fs::create_dir_all(&backup_dir).unwrap();
        let backup_path = backup_dir.join(filename);

        let tar_gz = fs::File::create(&backup_path).unwrap();
        let enc = GzEncoder::new(tar_gz, Compression::default());
        let mut tar = tar::Builder::new(enc);
        let mut header = tar::Header::new_gnu();

        header.set_entry_type(tar::EntryType::Regular);
        header.as_gnu_mut().unwrap().name[..entry_path.len()].copy_from_slice(entry_path);
        header.set_size(contents.len() as u64);
        header.set_cksum();

        let mut contents = Cursor::new(contents);
        tar.append(&header, &mut contents).unwrap();
        let enc = tar.into_inner().unwrap();
        enc.finish().unwrap();

        backup_path
    }

    fn write_link_backup_archive(config_path: &Path, filename: &str) -> PathBuf {
        let backup_dir = BackupManager::backup_dir(Some(config_path));
        fs::create_dir_all(&backup_dir).unwrap();
        let backup_path = backup_dir.join(filename);

        let tar_gz = fs::File::create(&backup_path).unwrap();
        let enc = GzEncoder::new(tar_gz, Compression::default());
        let mut tar = tar::Builder::new(enc);
        let mut header = tar::Header::new_gnu();

        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        tar.append_link(
            &mut header,
            crate::common::constants::config::FILENAME,
            "/tmp/evil-config.json",
        )
        .unwrap();
        let enc = tar.into_inner().unwrap();
        enc.finish().unwrap();

        backup_path
    }

    fn append_raw_tar_entry<W: Write, R: Read>(
        writer: &mut W,
        path: &str,
        entry_type: tar::EntryType,
        size: u64,
        mut contents: R,
    ) -> std::io::Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_path(path)?;
        header.set_entry_type(entry_type);
        header.set_size(size);
        header.set_mode(0o600);
        header.set_cksum();
        writer.write_all(header.as_bytes())?;

        let copied = std::io::copy(&mut contents.by_ref().take(size), writer)?;
        if copied != size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "raw tar entry contents are shorter than its header size",
            ));
        }

        write_tar_padding(writer, size)
    }

    fn write_tar_padding<W: Write>(writer: &mut W, size: u64) -> std::io::Result<()> {
        let remainder = size % TAR_BLOCK_BYTES as u64;
        if remainder == 0 {
            return Ok(());
        }

        let padding = TAR_BLOCK_BYTES as u64 - remainder;
        writer.write_all(&[0; TAR_BLOCK_BYTES][..padding as usize])
    }

    fn pax_record(key: &str, value: &str) -> Vec<u8> {
        let payload = format!("{}={}\n", key, value);
        let mut length_digits = 1;

        loop {
            let record_length = length_digits + 1 + payload.len();
            let next_length_digits = record_length.to_string().len();
            if next_length_digits == length_digits {
                return format!("{} {}", record_length, payload).into_bytes();
            }
            length_digits = next_length_digits;
        }
    }

    fn write_pax_size_override_archive(config_path: &Path, filename: &str) -> Result<PathBuf> {
        let backup_dir = BackupManager::backup_dir(Some(config_path));
        fs::create_dir_all(&backup_dir)?;
        let backup_path = backup_dir.join(filename);

        let tar_gz = fs::File::create(&backup_path)?;
        let mut enc = GzEncoder::new(tar_gz, Compression::default());
        let oversized_size = MAX_RESTORE_CONFIG_BYTES + 1;
        let pax_size = pax_record("size", &oversized_size.to_string());

        append_raw_tar_entry(
            &mut enc,
            "PaxHeaders/config.json",
            tar::EntryType::XHeader,
            pax_size.len() as u64,
            Cursor::new(pax_size),
        )?;

        // PAX supplies the effective size for this otherwise zero-sized entry.
        append_raw_tar_entry(
            &mut enc,
            crate::common::constants::config::FILENAME,
            tar::EntryType::Regular,
            0,
            std::io::empty(),
        )?;
        std::io::copy(&mut std::io::repeat(b'x').take(oversized_size), &mut enc)?;
        write_tar_padding(&mut enc, oversized_size)?;
        enc.write_all(&[0; TAR_BLOCK_BYTES * 2])?;
        enc.finish()?;

        Ok(backup_path)
    }

    fn write_gnu_long_name_archive(config_path: &Path, filename: &str) -> Result<PathBuf> {
        let backup_dir = BackupManager::backup_dir(Some(config_path));
        fs::create_dir_all(&backup_dir)?;
        let backup_path = backup_dir.join(filename);

        let tar_gz = fs::File::create(&backup_path)?;
        let mut enc = GzEncoder::new(tar_gz, Compression::default());
        let long_name = format!("{}\0", crate::common::constants::config::FILENAME);

        append_raw_tar_entry(
            &mut enc,
            "././@LongLink",
            tar::EntryType::GNULongName,
            long_name.len() as u64,
            Cursor::new(long_name),
        )?;
        append_raw_tar_entry(
            &mut enc,
            "ignored-name",
            tar::EntryType::Regular,
            2,
            Cursor::new(b"{}"),
        )?;
        enc.write_all(&[0; TAR_BLOCK_BYTES * 2])?;
        enc.finish()?;

        Ok(backup_path)
    }

    #[test]
    fn test_backup_logic() {
        // Setup temp environment
        let temp_dir = tempfile::tempdir().unwrap();
        let app_dir = temp_dir.path().join("eve-preview-manager");
        fs::create_dir_all(&app_dir).unwrap();

        let config_path = app_dir.join("config.json");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(b"{\"test\": true}").unwrap();

        // 1. Test Creation
        let backup_path = BackupManager::create_backup(false, Some(&config_path)).unwrap();
        assert!(backup_path.exists());
        assert!(backup_path.to_string_lossy().contains("auto_backup_"));
        assert!(!backup_path.to_string_lossy().contains("manual"));

        // Sleep to ensure unique timestamp
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Manual backup
        let manual_backup = BackupManager::create_backup(true, Some(&config_path)).unwrap();
        assert!(manual_backup.to_string_lossy().contains("manual_backup_"));

        // 2. Test Listing
        let list = BackupManager::list_backups(Some(&config_path)).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].timestamp >= list[1].timestamp); // Sorted newest first

        // 3. Test Restoration
        // Modify config first
        {
            let mut f = fs::File::create(&config_path).unwrap();
            f.write_all(b"{\"modified\": true}").unwrap();
        }

        // Delete the modified file to ensure restore recreates it
        fs::remove_file(&config_path).unwrap();

        BackupManager::restore_backup(&list[0].filename, Some(&config_path)).unwrap();
        let content = fs::read_to_string(&config_path).unwrap();
        assert_eq!(content, "{\"test\": true}");
        assert_eq!(fs::read_dir(&app_dir).unwrap().count(), 2);

        // 4. Test Pruning
        // Sleep between creations because backup filenames have one-second resolution.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        for _ in 0..5 {
            BackupManager::create_backup(false, Some(&config_path)).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        let list_before = BackupManager::list_backups(Some(&config_path)).unwrap();
        // Total: 2 initial (1 manual, 1 auto) + 5 new auto = 7 total. 6 auto.

        let auto_count = list_before.iter().filter(|b| !b.is_manual).count();
        assert_eq!(auto_count, 6);

        // Retention 3
        BackupManager::prune_backups(3, Some(&config_path)).unwrap();

        let list_after = BackupManager::list_backups(Some(&config_path)).unwrap();
        let auto_after = list_after.iter().filter(|b| !b.is_manual).count();
        assert_eq!(auto_after, 3);

        // Manual backup should still exist
        assert!(list_after.iter().any(|b| b.is_manual));

        // 5. Test Deletion
        let target = &list_after[0].filename;
        BackupManager::delete_backup(target, Some(&config_path)).unwrap();
        let list_final = BackupManager::list_backups(Some(&config_path)).unwrap();
        assert!(!list_final.iter().any(|b| b.filename == *target));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn restore_atomically_replaces_existing_config() {
        use std::os::unix::fs::PermissionsExt;

        let old_config = Config::default();
        let old_contents = serde_json::to_vec(&old_config).unwrap();
        let (_temp_dir, config_path) = setup_config(&old_contents);
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o640)).unwrap();
        let mut old_handle = fs::File::open(&config_path).unwrap();

        let mut restored_config = old_config;
        restored_config.profiles[0].profile_name = "restored".to_string();
        restored_config.global.selected_profile = "restored".to_string();
        let restored_contents = serde_json::to_vec(&restored_config).unwrap();
        let filename = "manual_backup_atomic_restore.tar.gz";
        write_backup_archive(
            &config_path,
            filename,
            &[(
                crate::common::constants::config::FILENAME,
                &restored_contents,
            )],
        );

        BackupManager::restore_backup(filename, Some(&config_path)).unwrap();

        old_handle.rewind().unwrap();
        let mut contents_from_old_handle = Vec::new();
        old_handle
            .read_to_end(&mut contents_from_old_handle)
            .unwrap();
        assert_eq!(contents_from_old_handle, old_contents);

        let loaded = Config::load_from(&config_path).unwrap();
        assert_eq!(loaded.global.selected_profile, "restored");
        assert_eq!(
            fs::metadata(&config_path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(
            fs::read_dir(config_path.parent().unwrap()).unwrap().count(),
            2
        );
    }

    #[test]
    fn restore_rejects_backup_filename_path_components() {
        let (_temp_dir, config_path) = setup_config(b"{\"original\": true}");

        let err = BackupManager::restore_backup("../manual_backup.tar.gz", Some(&config_path))
            .unwrap_err();

        assert!(err.to_string().contains("Invalid backup filename"));
        assert_eq!(
            fs::read_to_string(&config_path).unwrap(),
            "{\"original\": true}"
        );
    }

    #[test]
    fn delete_rejects_backup_filename_path_components() {
        let (_temp_dir, config_path) = setup_config(b"{\"original\": true}");
        fs::create_dir_all(BackupManager::backup_dir(Some(&config_path))).unwrap();

        let err = BackupManager::delete_backup("../config.json", Some(&config_path)).unwrap_err();

        assert!(err.to_string().contains("Invalid backup filename"));
        assert_eq!(
            fs::read_to_string(&config_path).unwrap(),
            "{\"original\": true}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn list_backups_ignores_symlinks() {
        let (_temp_dir, config_path) = setup_config(b"{\"original\": true}");
        let backup_dir = BackupManager::backup_dir(Some(&config_path));
        fs::create_dir_all(&backup_dir).unwrap();
        std::os::unix::fs::symlink(
            &config_path,
            backup_dir.join("manual_backup_symlink.tar.gz"),
        )
        .unwrap();

        let backups = BackupManager::list_backups(Some(&config_path)).unwrap();

        assert!(backups.is_empty());
    }

    #[test]
    fn create_backup_removes_incomplete_archive_after_write_failure() {
        let temp_dir = tempfile::tempdir().unwrap();
        let app_dir = temp_dir.path().join("eve-preview-manager");
        fs::create_dir_all(&app_dir).unwrap();
        let config_path = app_dir.join(crate::common::constants::config::FILENAME);
        fs::create_dir(&config_path).unwrap();

        BackupManager::create_backup(true, Some(&config_path))
            .expect_err("a directory cannot be archived as the config file");

        let backups = BackupManager::list_backups(Some(&config_path)).unwrap();
        assert!(backups.is_empty());
    }

    #[test]
    fn restore_rejects_parent_path_archive_entry() {
        let (temp_dir, config_path) = setup_config(b"{\"original\": true}");
        let filename = "manual_backup_parent.tar.gz";
        write_raw_path_backup_archive(
            &config_path,
            filename,
            b"../config.json",
            b"{\"escaped\": true}",
        );

        let err = BackupManager::restore_backup(filename, Some(&config_path)).unwrap_err();

        assert!(err.to_string().contains("unexpected entry"));
        assert_eq!(
            fs::read_to_string(&config_path).unwrap(),
            "{\"original\": true}"
        );
        assert!(!temp_dir.path().join("config.json").exists());
    }

    #[test]
    fn restore_rejects_archive_with_extra_entries_without_modifying_config() {
        let (_temp_dir, config_path) = setup_config(b"{\"original\": true}");
        let filename = "manual_backup_extra.tar.gz";
        write_backup_archive(
            &config_path,
            filename,
            &[
                (
                    crate::common::constants::config::FILENAME,
                    b"{\"restored\": true}",
                ),
                ("extra.json", b"{\"extra\": true}"),
            ],
        );

        let err = BackupManager::restore_backup(filename, Some(&config_path)).unwrap_err();

        assert!(err.to_string().contains("unexpected entry"));
        assert_eq!(
            fs::read_to_string(&config_path).unwrap(),
            "{\"original\": true}"
        );
        assert!(!config_path.parent().unwrap().join("extra.json").exists());
    }

    #[test]
    fn restore_rejects_archive_link_entry() {
        let (_temp_dir, config_path) = setup_config(b"{\"original\": true}");
        let filename = "manual_backup_link.tar.gz";
        write_link_backup_archive(&config_path, filename);

        let err = BackupManager::restore_backup(filename, Some(&config_path)).unwrap_err();

        assert!(err.to_string().contains("non-file entry"));
        assert_eq!(
            fs::read_to_string(&config_path).unwrap(),
            "{\"original\": true}"
        );
    }

    #[test]
    fn restore_rejects_pax_size_override_without_modifying_config() -> Result<()> {
        let (_temp_dir, config_path) = setup_config(b"{\"original\": true}");
        let filename = "manual_backup_pax_size.tar.gz";
        write_pax_size_override_archive(&config_path, filename)?;

        let err = BackupManager::restore_backup(filename, Some(&config_path))
            .expect_err("PAX metadata entries must not be accepted during restore");

        assert!(err.to_string().contains("unexpected entry"));
        assert_eq!(fs::read_to_string(&config_path)?, "{\"original\": true}");
        Ok(())
    }

    #[test]
    fn restore_rejects_gnu_long_name_extension_without_modifying_config() -> Result<()> {
        let (_temp_dir, config_path) = setup_config(b"{\"original\": true}");
        let filename = "manual_backup_long_name.tar.gz";
        write_gnu_long_name_archive(&config_path, filename)?;

        let err = BackupManager::restore_backup(filename, Some(&config_path))
            .expect_err("GNU metadata entries must not be accepted during restore");

        assert!(err.to_string().contains("unexpected entry"));
        assert_eq!(fs::read_to_string(&config_path)?, "{\"original\": true}");
        Ok(())
    }

    #[test]
    fn restore_rejects_invalid_config_without_modifying_current_config() -> Result<()> {
        let (_temp_dir, config_path) = setup_config(b"{\"original\": true}");
        let filename = "manual_backup_invalid_config.tar.gz";
        write_backup_archive(
            &config_path,
            filename,
            &[(crate::common::constants::config::FILENAME, b"not json")],
        );

        BackupManager::restore_backup(filename, Some(&config_path))
            .expect_err("invalid configuration JSON must not be restored");

        assert_eq!(fs::read_to_string(&config_path)?, "{\"original\": true}");
        Ok(())
    }

    #[test]
    fn restore_rejects_config_without_profiles_without_modifying_current_config() -> Result<()> {
        let (_temp_dir, config_path) = setup_config(b"{\"original\": true}");
        let filename = "manual_backup_no_profiles.tar.gz";
        write_backup_archive(
            &config_path,
            filename,
            &[(
                crate::common::constants::config::FILENAME,
                b"{\"profiles\": []}",
            )],
        );

        BackupManager::restore_backup(filename, Some(&config_path))
            .expect_err("a configuration without profiles must not be restored");

        assert_eq!(fs::read_to_string(&config_path)?, "{\"original\": true}");
        Ok(())
    }

    #[test]
    fn restore_rejects_invalid_gzip_checksum_without_modifying_config() -> Result<()> {
        let (_temp_dir, config_path) = setup_config(b"{\"original\": true}");
        let filename = "manual_backup_bad_checksum.tar.gz";
        let backup_path = write_backup_archive(
            &config_path,
            filename,
            &[(crate::common::constants::config::FILENAME, b"{}")],
        );
        let mut archive_bytes = fs::read(&backup_path)?;
        let checksum_byte = archive_bytes
            .len()
            .checked_sub(8)
            .context("test archive is missing its gzip footer")?;
        archive_bytes[checksum_byte] ^= 0xff;
        fs::write(&backup_path, archive_bytes)?;

        BackupManager::restore_backup(filename, Some(&config_path))
            .expect_err("a corrupt gzip checksum must not be accepted");

        assert_eq!(fs::read_to_string(&config_path)?, "{\"original\": true}");
        Ok(())
    }

    #[test]
    fn restore_rejects_appended_gzip_member_without_modifying_config() -> Result<()> {
        let (_temp_dir, config_path) = setup_config(b"{\"original\": true}");
        let filename = "manual_backup_appended_member.tar.gz";
        let valid_config = serde_json::to_vec(&Config::default())?;
        let backup_path = write_backup_archive(
            &config_path,
            filename,
            &[(crate::common::constants::config::FILENAME, &valid_config)],
        );

        let mut extra_encoder = GzEncoder::new(Vec::new(), Compression::default());
        extra_encoder.write_all(b"unexpected second gzip member")?;
        let extra_member = extra_encoder.finish()?;
        let mut backup_file = fs::OpenOptions::new().append(true).open(&backup_path)?;
        backup_file.write_all(&extra_member)?;

        BackupManager::restore_backup(filename, Some(&config_path))
            .expect_err("data after the expected gzip stream must not be accepted");

        assert_eq!(fs::read_to_string(&config_path)?, "{\"original\": true}");
        Ok(())
    }

    #[test]
    fn test_pruning_priority() {
        // Setup temp environment
        let temp_dir = tempfile::tempdir().unwrap();
        let app_dir = temp_dir.path().join("eve-preview-manager");
        fs::create_dir_all(&app_dir).unwrap();
        let config_path = app_dir.join("config.json");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(b"{}").unwrap();

        // 1. Create a Manual Backup (Oldest)
        let manual = BackupManager::create_backup(true, Some(&config_path)).unwrap();
        // Sleep to ensure timestamp diff
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // 2. Create 5 Auto Backups (Newer)
        for _ in 0..5 {
            BackupManager::create_backup(false, Some(&config_path)).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }

        // 3. Prune with retention 2
        // Should keep 2 newest autos, plus the manual one. Total 3.
        BackupManager::prune_backups(2, Some(&config_path)).unwrap();

        let list = BackupManager::list_backups(Some(&config_path)).unwrap();

        // Check counts
        let auto_count = list.iter().filter(|b| !b.is_manual).count();
        let manual_count = list.iter().filter(|b| b.is_manual).count();

        assert_eq!(auto_count, 2, "Should have 2 auto backups");
        assert_eq!(manual_count, 1, "Should preserve manual backup");

        // Verify the manual backup is the one we created
        let manual_filename = manual.file_name().unwrap().to_str().unwrap();
        assert!(
            list.iter().any(|b| b.filename == manual_filename),
            "Original manual backup should be preserved"
        );
    }
}
