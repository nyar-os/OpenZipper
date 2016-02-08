use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};

/// Default 100_000 entries, 8 GiB total, 4 GiB per entry, 1000:1 ratio and 3 nested levels.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveLimits {
    pub max_entries: usize,
    pub max_total_bytes: u64,
    pub max_entry_bytes: u64,
    pub max_compression_ratio: u32,
    pub max_nested_depth: u32,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_total_bytes: 8 * 1024 * 1024 * 1024,
            max_entry_bytes: 4 * 1024 * 1024 * 1024,
            max_compression_ratio: 1000,
            max_nested_depth: 3,
        }
    }
}

impl ArchiveLimits {
    pub fn validate(&self) -> Result<()> {
        if self.max_entries == 0 {
            return Err(Error::coded(ErrorCode::InvalidOption, "`maxEntries` must be greater than zero"));
        }
        if self.max_total_bytes == 0 || self.max_entry_bytes == 0 {
            return Err(Error::coded(ErrorCode::InvalidOption, "size limits must be greater than zero"));
        }
        if self.max_compression_ratio == 0 {
            return Err(Error::coded(ErrorCode::InvalidOption, "`maxCompressionRatio` must be greater than zero"));
        }
        Ok(())
    }

    pub fn check_entry_count(&self, count: usize) -> Result<()> {
        if count > self.max_entries {
            return Err(Error::coded(
                ErrorCode::ArchiveBomb,
                format!("archive declares {count} entries, above the limit of {}", self.max_entries),
            ));
        }
        Ok(())
    }

    pub fn check_declared_entry(&self, name: &str, size: u64, compressed: Option<u64>, total: u64) -> Result<()> {
        if size > self.max_entry_bytes {
            return Err(Error::coded(
                ErrorCode::ArchiveBomb,
                format!("entry `{name}` declares {size} bytes, above the per-entry limit of {}", self.max_entry_bytes),
            ));
        }
        if total > self.max_total_bytes {
            return Err(Error::coded(
                ErrorCode::ArchiveBomb,
                format!("archive declares at least {total} bytes, above the total limit of {}", self.max_total_bytes),
            ));
        }
        if let Some(compressed) = compressed
            && compressed > 0
            && size > 1_048_576
            && size / compressed > u64::from(self.max_compression_ratio)
        {
            return Err(Error::coded(
                ErrorCode::ArchiveBomb,
                format!(
                    "entry `{name}` has a compression ratio of {}:1, above the limit of {}:1",
                    size / compressed,
                    self.max_compression_ratio
                ),
            ));
        }
        Ok(())
    }

    pub fn check_written_bytes(&self, name: &str, entry_bytes: u64, total_bytes: u64) -> Result<()> {
        if entry_bytes > self.max_entry_bytes {
            return Err(Error::coded(
                ErrorCode::ArchiveBomb,
                format!("entry `{name}` expanded past the per-entry limit of {} bytes", self.max_entry_bytes),
            ));
        }
        if total_bytes > self.max_total_bytes {
            return Err(Error::coded(
                ErrorCode::ArchiveBomb,
                format!("archive expanded past the total limit of {} bytes", self.max_total_bytes),
            ));
        }
        Ok(())
    }

    /// Compressed tar streams do not expose per-entry compressed sizes, so the
    /// guard compares cumulative output with the on-disk archive size.
    pub fn check_global_ratio(&self, total_bytes: u64, archive_bytes: u64) -> Result<()> {
        if archive_bytes == 0 {
            return Ok(());
        }
        if total_bytes / archive_bytes > u64::from(self.max_compression_ratio) {
            return Err(Error::coded(
                ErrorCode::ArchiveBomb,
                format!("archive expanded past the global compression ratio of {}:1", self.max_compression_ratio),
            ));
        }
        Ok(())
    }

    pub fn check_nested_depth(&self, depth: u32) -> Result<()> {
        if depth > self.max_nested_depth {
            return Err(Error::coded(
                ErrorCode::NestedDepth,
                format!("nested archive depth {depth} exceeds the limit of {}", self.max_nested_depth),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_count_limit_is_enforced() {
        let limits = ArchiveLimits { max_entries: 2, ..ArchiveLimits::default() };
        assert!(limits.check_entry_count(2).is_ok());
        assert!(limits.check_entry_count(3).is_err());
    }

    #[test]
    fn compression_ratio_is_enforced() {
        let limits = ArchiveLimits { max_compression_ratio: 10, ..ArchiveLimits::default() };
        assert!(limits.check_declared_entry("small.txt", 100, Some(10), 100).is_ok());
        let error = limits.check_declared_entry("bomb.bin", 5_000_000, Some(1000), 5_000_000).unwrap_err();
        assert_eq!(error.code(), "archive_bomb");
    }
}
