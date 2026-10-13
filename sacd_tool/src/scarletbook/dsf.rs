use anyhow::{Context, Result};
use byteorder::{LittleEndian, WriteBytesExt};
use std::fs::File;
use std::io::{BufWriter, Seek, Write};
use std::path::Path;

/// DSF file format constants
const DSD_CHUNK_ID: &[u8; 4] = b"DSD ";
const FMT_CHUNK_ID: &[u8; 4] = b"fmt ";
const DATA_CHUNK_ID: &[u8; 4] = b"data";
const DSD_CHUNK_SIZE: u64 = 28;
const FMT_CHUNK_SIZE: u64 = 52;

/// DSF block size per channel — 4096 bytes is fixed by the DSF spec.
pub const DSF_BLOCK_SIZE_PER_CHANNEL: usize = 4096;

/// Per-channel partial-block tails saved when nopad mode skips zero-padding
/// the last DSF block of a track. These bytes are already in DSF on-disk
/// form (LSB-first) and are pre-loaded into the next consecutive track's
/// per-channel buffers via `DsfWriter::pre_load_carry`.
pub struct NopadCarry {
    /// One Vec per channel; `len() < DSF_BLOCK_SIZE_PER_CHANNEL`. All Vecs
    /// have the same length.
    pub channel_tails: Vec<Vec<u8>>,
}

/// DSF file writer for SACD audio extraction. Note that `sample_rate` and
/// the up-front `total_samples_per_channel` parameters are written into
/// the fmt chunk during `create()` but not retained on the struct: the
/// final `sample_count` is patched in `finalize()` from the actual decoded
/// byte total (the C reference does the same via `handle->sample_count /
/// channel_count * 8`).
pub struct DsfWriter {
    writer: BufWriter<File>,
    channel_count: u32,
    data_chunk_offset: u64,
    bytes_written: u64,
    /// Total decoded sample bytes received via `write_samples` BEFORE block
    /// padding. `sample_count` in the fmt chunk is derived from this so that
    /// it reflects the actual decoded length, not the padded one.
    decoded_bytes_total: u64,
    /// Per-channel buffers for interleaving (4096 bytes per channel)
    channel_buffers: Vec<Vec<u8>>,
    channel_buffer_pos: usize,
    /// Optional ID3 footer to write after the audio data on finalize().
    id3_footer: Option<Vec<u8>>,
    /// When true, finalize() returns the partial last block as a carry
    /// instead of zero-padding and writing it. Mirrors C `dsf_nopad`.
    nopad: bool,
}

impl DsfWriter {
    /// Create a new DSF file writer
    ///
    /// # Arguments
    /// * `path` - Output file path
    /// * `channel_count` - Number of audio channels (2 for stereo, 5 for 5.0, 6 for 5.1, etc.)
    /// * `sample_rate` - Sample rate in Hz (typically 2822400 for DSD64)
    /// * `total_samples_per_channel` - Total number of samples per channel (for calculating file size)
    /// * `extra_setting` - Extra settings from area TOC (used to determine speaker configuration)
    pub fn create(
        path: &Path,
        channel_count: u32,
        sample_rate: u32,
        total_samples_per_channel: u64,
        extra_setting: u8,
    ) -> Result<Self> {
        let file = File::create(path)
            .with_context(|| format!("Failed to create DSF file: {}", path.display()))?;
        let mut writer = BufWriter::new(file);

        // Write DSD chunk (file header)
        writer.write_all(DSD_CHUNK_ID)?;
        writer.write_u64::<LittleEndian>(DSD_CHUNK_SIZE)?; // Chunk size (always 28)
        writer.write_u64::<LittleEndian>(0)?; // Total file size (placeholder, will update later)
        writer.write_u64::<LittleEndian>(0)?; // Metadata pointer (0 = no metadata)

        // Write fmt chunk (format info)
        writer.write_all(FMT_CHUNK_ID)?;
        writer.write_u64::<LittleEndian>(FMT_CHUNK_SIZE)?; // Chunk size (always 52)
        writer.write_u32::<LittleEndian>(1)?; // Format version (always 1)
        writer.write_u32::<LittleEndian>(0)?; // Format ID (0 = DSD raw)

        // Channel type based on channel count and extra_setting
        // This matches the logic from the reference implementation
        let channel_type = if channel_count == 2 && extra_setting == 0 {
            2 // Stereo
        } else if channel_count == 5 && extra_setting == 3 {
            6 // 5 channels
        } else if channel_count == 6 && extra_setting == 4 {
            7 // 5.1 channels
        } else {
            // Fallback based on channel count
            match channel_count {
                1 => 1, // Mono
                2 => 2, // Stereo
                3 => 3, // 3 channels
                4 => 4, // Quad
                5 => 6, // 5 channels
                6 => 7, // 5.1 channels
                _ => 2, // Default to stereo
            }
        };
        writer.write_u32::<LittleEndian>(channel_type)?; // Channel type
        writer.write_u32::<LittleEndian>(channel_count)?; // Number of channels
        writer.write_u32::<LittleEndian>(sample_rate)?; // Sampling frequency
        writer.write_u32::<LittleEndian>(1)?; // Bits per sample (always 1 for DSD)
        writer.write_u64::<LittleEndian>(total_samples_per_channel)?; // Sample count
        writer.write_u32::<LittleEndian>(DSF_BLOCK_SIZE_PER_CHANNEL as u32)?;
        writer.write_u32::<LittleEndian>(0)?; // Reserved (must be 0)

        // Write data chunk header
        let data_chunk_offset = writer.stream_position()?;
        writer.write_all(DATA_CHUNK_ID)?;
        writer.write_u64::<LittleEndian>(0)?; // Data chunk size (placeholder, will update later)

        // Initialize per-channel buffers, one block-sized chunk each.
        let channel_buffers =
            vec![Vec::with_capacity(DSF_BLOCK_SIZE_PER_CHANNEL); channel_count as usize];

        Ok(Self {
            writer,
            channel_count,
            data_chunk_offset,
            bytes_written: 0,
            decoded_bytes_total: 0,
            channel_buffers,
            channel_buffer_pos: 0,
            id3_footer: None,
            nopad: false,
        })
    }

    /// Attach an ID3 footer to be written after audio on finalize. Setting
    /// the metadata pointer in the DSD chunk happens automatically.
    pub fn set_id3_footer(&mut self, footer: Vec<u8>) {
        self.id3_footer = Some(footer);
    }

    /// Enable nopad mode: when set, `finalize()` returns the partial last
    /// block as a `NopadCarry` instead of zero-padding it. Used together
    /// with `pre_load_carry` on the next consecutive track to avoid the
    /// inter-track zero-pad gap that some DSF players cannot handle.
    pub fn set_nopad(&mut self, on: bool) {
        self.nopad = on;
    }

    /// Pre-load the per-channel buffers with a carry produced by the
    /// previous consecutive track's nopad finalize(). The bytes are already
    /// in DSF on-disk form (LSB-first) and are placed at the head of each
    /// channel's buffer; the round-robin write position is advanced so the
    /// next byte from `write_samples` lands on channel 0.
    pub fn pre_load_carry(&mut self, carry: NopadCarry) {
        assert_eq!(
            carry.channel_tails.len(),
            self.channel_count as usize,
            "carry channel count mismatch"
        );
        let per_channel = carry.channel_tails.first().map(|v| v.len()).unwrap_or(0);
        for (ch, tail) in carry.channel_tails.into_iter().enumerate() {
            debug_assert_eq!(tail.len(), per_channel, "carry tails must be equal length");
            self.channel_buffers[ch].extend_from_slice(&tail);
        }
        self.channel_buffer_pos = per_channel * self.channel_count as usize;
    }

    /// Write DSD audio samples
    ///
    /// SACD audio data is byte-interleaved with MSB-first bit order: byte0=ch0, byte1=ch1...
    /// DSF format requires block-interleaved with LSB-first bit order: [ch0 4096 bytes][ch1 4096 bytes]...
    ///
    /// This method deinterleaves by channel, reverses bits, and buffers to 4096-byte blocks.
    ///
    /// # Arguments
    /// * `data` - Byte-interleaved audio data (MSB-first)
    pub fn write_samples(&mut self, data: &[u8]) -> Result<()> {
        // Deinterleave byte-by-byte in round-robin fashion and reverse bits
        // Input: byte0=ch0(MSB), byte1=ch1(MSB), byte2=ch0(MSB), byte3=ch1(MSB)...
        // Output: buffer[ch0][0..4096](LSB), buffer[ch1][0..4096](LSB), etc.

        self.decoded_bytes_total += data.len() as u64;

        for &byte in data {
            // Determine which channel this byte belongs to (round-robin)
            let channel_idx = self.channel_buffer_pos % self.channel_count as usize;

            // Reverse bits (MSB -> LSB) and add to that channel's buffer
            let reversed_byte = reverse_bits(byte);
            self.channel_buffers[channel_idx].push(reversed_byte);

            self.channel_buffer_pos += 1;

            // Check if ALL channels' buffers are full (4096 bytes each)
            if self
                .channel_buffers
                .iter()
                .all(|buf| buf.len() >= DSF_BLOCK_SIZE_PER_CHANNEL)
            {
                // Write all channel blocks in order: ch0, ch1, ch2, ...
                for channel in 0..self.channel_count as usize {
                    let block: Vec<u8> = self.channel_buffers[channel]
                        .drain(..DSF_BLOCK_SIZE_PER_CHANNEL)
                        .collect();
                    self.writer.write_all(&block)?;
                    self.bytes_written += DSF_BLOCK_SIZE_PER_CHANNEL as u64;
                }
            }
        }

        Ok(())
    }

    /// Flush one 4096-byte block from all channel buffers (used by finalize()).
    fn flush_channel_block(&mut self) -> Result<()> {
        // With the new round-robin approach, we flush individual channels as they fill
        // This function is kept for the finalize() method
        for channel in 0..self.channel_count as usize {
            if self.channel_buffers[channel].len() >= DSF_BLOCK_SIZE_PER_CHANNEL {
                let block: Vec<u8> = self.channel_buffers[channel]
                    .drain(..DSF_BLOCK_SIZE_PER_CHANNEL)
                    .collect();
                self.writer.write_all(&block)?;
                self.bytes_written += DSF_BLOCK_SIZE_PER_CHANNEL as u64;
            }
        }
        Ok(())
    }

    /// Finish writing the DSF file and update headers with final sizes.
    ///
    /// Returns `Some(NopadCarry)` when nopad mode is enabled and the track
    /// ended with a non-empty partial block; the caller is responsible for
    /// passing that carry to the next consecutive track via
    /// `pre_load_carry`. Returns `None` otherwise (default zero-pad path).
    pub fn finalize(mut self) -> Result<Option<NopadCarry>> {
        let has_partial = self.channel_buffers.iter().any(|buf| !buf.is_empty());

        let carry = if self.nopad && has_partial {
            // Steal the partial tails per channel for the caller. Do not
            // pad/flush; bytes_written and the data chunk size stay at
            // their pre-tail values so this file's audio length is shorter
            // than `ceil(N/4096)*4096` per channel.
            let channel_tails: Vec<Vec<u8>> = self
                .channel_buffers
                .iter_mut()
                .map(std::mem::take)
                .collect();
            Some(NopadCarry { channel_tails })
        } else {
            if has_partial {
                for channel_buf in &mut self.channel_buffers {
                    if channel_buf.len() < DSF_BLOCK_SIZE_PER_CHANNEL {
                        channel_buf.resize(DSF_BLOCK_SIZE_PER_CHANNEL, 0);
                    }
                }
                self.flush_channel_block()?;
            }
            None
        };

        let audio_end_pos = self.writer.stream_position()?;

        // Optionally append ID3 footer.
        let footer_size = if let Some(ref footer) = self.id3_footer {
            self.writer.write_all(footer)?;
            footer.len() as u64
        } else {
            0
        };
        let total_file_size = audio_end_pos + footer_size;

        // Update DSD chunk header (offset 12 = total_file_size, offset 20 = metadata_offset).
        self.writer.seek(std::io::SeekFrom::Start(12))?;
        self.writer.write_u64::<LittleEndian>(total_file_size)?;
        let metadata_offset = if footer_size > 0 { audio_end_pos } else { 0 };
        self.writer.write_u64::<LittleEndian>(metadata_offset)?;

        // Patch fmt chunk's sample_count (offset 28 + 12 + 4*5 = 60) so that it
        // reflects the actual decoded sample count, not the padded one.
        // In nopad mode the partial tail is held over to the next track so
        // it must not be reflected in this file's sample_count; derive
        // sample_count from `bytes_written` (matches C `handle->sample_count
        // / channel_count * 8` when nopad skips the final block flush).
        // In default pad mode the existing decoded-bytes accounting matches
        // the C reference bit-for-bit on block-aligned tracks.
        let channels = self.channel_count.max(1) as u64;
        let sample_count = if carry.is_some() {
            self.bytes_written / channels * 8
        } else {
            self.decoded_bytes_total / channels * 8
        };
        // Layout: DSD(28) + fmt header (id 4 + size 8) + version 4 + format_id 4
        //         + channel_type 4 + channel_count 4 + sample_freq 4
        //         + bits_per_sample 4 = 28 + 12 + 4 + 4 + 4 + 4 + 4 + 4 = 64.
        self.writer.seek(std::io::SeekFrom::Start(64))?;
        self.writer.write_u64::<LittleEndian>(sample_count)?;

        // Update data chunk size = 12 (header) + audio bytes.
        let data_chunk_size = 12 + self.bytes_written;
        self.writer
            .seek(std::io::SeekFrom::Start(self.data_chunk_offset + 4))?;
        self.writer.write_u64::<LittleEndian>(data_chunk_size)?;

        self.writer.flush()?;
        Ok(carry)
    }
}

/// Reverse bits in a byte (convert MSB-first to LSB-first).
///
/// SACD audio data uses MSB-first bit order, but DSF format requires LSB-first.
#[inline]
pub fn reverse_bits(byte: u8) -> u8 {
    let mut result = byte;
    result = (result & 0xF0) >> 4 | (result & 0x0F) << 4;
    result = (result & 0xCC) >> 2 | (result & 0x33) << 2;
    result = (result & 0xAA) >> 1 | (result & 0x55) << 1;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reverse_bits() {
        assert_eq!(reverse_bits(0b00000000), 0b00000000);
        assert_eq!(reverse_bits(0b11111111), 0b11111111);
        assert_eq!(reverse_bits(0b10000000), 0b00000001);
        assert_eq!(reverse_bits(0b00000001), 0b10000000);
        assert_eq!(reverse_bits(0b10110010), 0b01001101);
        assert_eq!(reverse_bits(0b11001010), 0b01010011);
    }
}
