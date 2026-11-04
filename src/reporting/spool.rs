//! On-disk spool of outcomes not yet accepted by the endpoint.
//!
//! One file per outcome (`<finished_at µs>-<idempotency key>.pb`, the serialized
//! `ReportOutcomeRequest`), so they are sent in order and survive restarts. Files
//! the endpoint will never accept are moved to `rejected/`.

use std::path::{Path, PathBuf};

use tracing::warn;

pub struct Spool {
    dir: PathBuf,
    max_entries: usize,
}

impl Spool {
    pub fn new(dir: &str, max_entries: usize) -> Result<Self, String> {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(dir.join("rejected"))
            .map_err(|e| format!("Failed to create spool directory {}: {}", dir.display(), e))?;
        Ok(Self { dir, max_entries })
    }

    /// Store one outcome; beyond `max_entries`, drop the oldest ones.
    pub fn push(&self, finished_at_micros: i64, idempotency_key: &str, bytes: &[u8]) -> Result<(), String> {
        let key: String = idempotency_key
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        let name = format!("{:020}-{}.pb", finished_at_micros.max(0), key);
        let tmp = self.dir.join(format!(".{name}.tmp"));
        std::fs::write(&tmp, bytes).map_err(|e| format!("Failed to write {}: {}", tmp.display(), e))?;
        std::fs::rename(&tmp, self.dir.join(&name))
            .map_err(|e| format!("Failed to move {} into the spool: {}", tmp.display(), e))?;

        let entries = self.entries()?;
        if entries.len() > self.max_entries {
            let excess = entries.len() - self.max_entries;
            warn!(
                excess,
                max_entries = self.max_entries,
                "Spool full: dropping the oldest unsent outcome(s)"
            );
            for path in entries.iter().take(excess) {
                self.remove(path);
            }
        }
        Ok(())
    }

    /// Spooled outcomes, oldest first.
    pub fn entries(&self) -> Result<Vec<PathBuf>, String> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .map_err(|e| format!("Failed to read spool {}: {}", self.dir.display(), e))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "pb"))
            .collect();
        entries.sort();
        Ok(entries)
    }

    pub fn remove(&self, path: &Path) {
        if let Err(details) = std::fs::remove_file(path) {
            warn!(%details, path = %path.display(), "Failed to remove spooled outcome");
        }
    }

    /// Keep an outcome the endpoint will never accept aside, for inspection.
    pub fn reject(&self, path: &Path) {
        let Some(name) = path.file_name() else { return };
        if let Err(details) = std::fs::rename(path, self.dir.join("rejected").join(name)) {
            warn!(%details, path = %path.display(), "Failed to move rejected outcome");
            self.remove(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("regops-spool-test-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.to_string_lossy().to_string()
    }

    #[test]
    fn keeps_order_and_bound() {
        let dir = temp_dir("bound");
        let spool = Spool::new(&dir, 2).unwrap();
        spool.push(3, "c", b"3").unwrap();
        spool.push(1, "a/../x", b"1").unwrap();
        spool.push(2, "b", b"2").unwrap();

        let entries = spool.entries().unwrap();
        assert_eq!(entries.len(), 2, "oldest dropped beyond the bound");
        assert_eq!(std::fs::read(&entries[0]).unwrap(), b"2");
        assert_eq!(std::fs::read(&entries[1]).unwrap(), b"3");

        spool.reject(&entries[0]);
        assert_eq!(spool.entries().unwrap().len(), 1);
        assert!(Path::new(&dir).join("rejected").read_dir().unwrap().next().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
