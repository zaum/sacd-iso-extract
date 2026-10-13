use anyhow::{Context, Result};
use log::debug;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::scarletbook::consts::SACD_LSN_SIZE;

use crate::sacd_reader::SacdReader;

/// SACD ISO file reader
///
/// Reads SACD ISO images and provides sector-level access to the disc data.
/// Each sector (LSN - Logical Sector Number) is 2048 bytes.
pub struct IsoReader {
    file: File,
    total_sectors: Option<u32>,
}

impl SacdReader for IsoReader {
    fn read_data(&mut self, start_lsn: u32, sector_count: u32) -> Result<Vec<u8>> {
        self.read_blocks(start_lsn, sector_count)
    }

    fn get_total_sectors(&mut self) -> Result<u32> {
        self.get_total_sectors()
    }
}

impl IsoReader {
    /// Open an SACD ISO file
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let file = File::open(path.as_ref())
            .with_context(|| format!("Failed to open ISO file: {}", path.as_ref().display()))?;

        Ok(Self {
            file,
            total_sectors: None,
        })
    }

    /// Get the total number of sectors in the ISO
    pub fn get_total_sectors(&mut self) -> Result<u32> {
        if let Some(sectors) = self.total_sectors {
            return Ok(sectors);
        }

        // Get file size and calculate number of sectors
        let file_size = self
            .file
            .metadata()
            .context("Failed to get ISO file metadata")?
            .len();

        let sectors = (file_size / SACD_LSN_SIZE as u64) as u32;
        self.total_sectors = Some(sectors);

        Ok(sectors)
    }

    /// Read a block of sectors from the ISO
    ///
    /// # Arguments
    /// * `start_lsn` - Starting logical sector number
    /// * `sector_count` - Number of sectors to read
    ///
    /// # Returns
    /// A vector containing the raw sector data (sector_count * 2048 bytes)
    pub fn read_blocks(&mut self, start_lsn: u32, sector_count: u32) -> Result<Vec<u8>> {
        let offset = (start_lsn as u64) * (SACD_LSN_SIZE as u64);
        let bytes_to_read = (sector_count as usize) * SACD_LSN_SIZE;

        // Seek to the start position
        self.file
            .seek(SeekFrom::Start(offset))
            .with_context(|| format!("Failed to seek to sector {} in ISO", start_lsn))?;

        // Read the data
        let mut buffer = vec![0u8; bytes_to_read];
        self.file.read_exact(&mut buffer).with_context(|| {
            format!(
                "Failed to read {} sectors from ISO at sector {}",
                sector_count, start_lsn
            )
        })?;

        // Log first few sectors in the 584-600 range
        if (584..590).contains(&start_lsn) {
            debug!(
                "[ISO_READ] LSN {}: First 32 bytes: {:02x?}",
                start_lsn,
                &buffer[..32.min(buffer.len())]
            );
        }

        Ok(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iso_reader_basic() {
        // This test requires an actual ISO file to work
        // For now, just verify the module compiles
        assert_eq!(SACD_LSN_SIZE, 2048);
    }
}
