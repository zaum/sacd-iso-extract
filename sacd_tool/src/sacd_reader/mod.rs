use anyhow::Result;

mod iso_reader;

pub use iso_reader::IsoReader;

/// Trait for reading SACD data from various sources (ISO file, etc.)
pub trait SacdReader {
    /// Read sectors from the disc.
    ///
    /// # Arguments
    /// * `start_lsn` - Starting logical sector number
    /// * `sector_count` - Number of sectors to read
    ///
    /// # Returns
    /// A vector containing the raw sector data (sector_count * 2048 bytes)
    fn read_data(&mut self, start_lsn: u32, sector_count: u32) -> Result<Vec<u8>>;

    /// Get the total number of sectors
    fn get_total_sectors(&mut self) -> Result<u32>;
}
