use crate::scarletbook::consts::{FRAMES_PER_MINUTE, FRAMES_PER_SECOND};
use crate::scarletbook::types::FrameFormat;
use anyhow::Result;
use log::{debug, trace};

/// SACD audio frame header (first byte of each audio sector)
#[derive(Debug, Clone, Copy)]
pub struct AudioFrameHeader {
    /// DST encoded flag
    pub dst_encoded: bool,
    /// Number of frame starts (N_Frame_Starts)
    pub frame_info_count: u8,
    /// Number of packets (N_Packets)
    pub packet_info_count: u8,
}

impl AudioFrameHeader {
    /// Parse audio frame header from a byte
    pub fn from_byte(byte: u8) -> Self {
        Self {
            dst_encoded: (byte & 0x01) != 0,
            frame_info_count: (byte >> 2) & 0x07,
            packet_info_count: (byte >> 5) & 0x07,
        }
    }
}

/// SACD audio packet info
#[derive(Debug, Clone, Copy)]
pub struct AudioPacketInfo {
    /// Frame start flag
    pub frame_start: bool,
    /// Data type
    pub data_type: u8,
    /// Packet length in bytes
    pub packet_length: u16,
}

impl AudioPacketInfo {
    /// Parse audio packet info from 2 bytes
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            frame_start: (bytes[0] & 0x80) != 0,
            data_type: (bytes[0] >> 3) & 0x07,
            packet_length: ((bytes[0] as u16 & 0x07) << 8) | bytes[1] as u16,
        }
    }
}

/// SACD audio frame info (from sector headers)
#[derive(Debug, Clone, Copy)]
pub struct AudioFrameInfo {
    /// Timecode
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    /// Sector count (DST frames only) - number of sectors remaining for this frame
    pub sector_count: u8,
    /// Per-frame channel-hint bits, mirroring the C reference's
    /// `channel_bit_1/2/3` packed in byte 3 of the frame info. Used to
    /// cross-check the area TOC's channel count.
    pub channel_bits: u8,
}

impl AudioFrameInfo {
    /// Parse frame info from bytes
    pub fn from_bytes(bytes: &[u8], dst_encoded: bool) -> Self {
        let size = if dst_encoded { 4 } else { 3 };
        assert!(bytes.len() >= size, "Frame info too short");

        Self {
            minutes: bytes[0],
            seconds: bytes[1],
            frames: bytes[2],
            // byte-3 layout (LSB first): channel_bit_3 (bit 0),
            // channel_bit_2 (bit 1), sector_count (bits 2-6), channel_bit_1 (bit 7).
            sector_count: if dst_encoded {
                (bytes[3] >> 2) & 0x1F
            } else {
                0
            },
            channel_bits: if dst_encoded { bytes[3] & 0xE1 } else { 0 },
        }
    }

    /// Convert timecode to frame count (matching C code's TIME_FRAMECOUNT macro).
    pub fn to_frame_count(self) -> u32 {
        self.minutes as u32 * FRAMES_PER_MINUTE
            + self.seconds as u32 * FRAMES_PER_SECOND
            + self.frames as u32
    }

    /// Derive channel count from `channel_bit_2/3`, matching the C
    /// reference's `get_channel_count(audio_frame_info_t*)`.
    pub fn derived_channel_count(&self) -> u8 {
        let bit_3 = self.channel_bits & 0x01;
        let bit_2 = (self.channel_bits >> 1) & 0x01;
        match (bit_2, bit_3) {
            (1, 0) => 6,
            (0, 1) => 5,
            _ => 2,
        }
    }
}

/// Audio data types
const DATA_TYPE_AUDIO: u8 = 2;
const DATA_TYPE_SUPPLEMENTARY: u8 = 3;
const DATA_TYPE_PADDING: u8 = 7;

/// Audio sector parser. Walks SACD audio sectors and yields one
/// **raw** frame at a time:
///
/// * For DST-compressed areas, the yielded bytes are the raw DST frame
///   payload (the bytes that get fed into `DstDecoder::decode_frame`).
///   The caller is responsible for decoding — this lets the caller
///   batch frames and decode them in parallel.
/// * For uncompressed DSD areas, the yielded bytes are the audio data
///   directly, in chunks of at least `CHUNK_SIZE`.
///
/// `decoded_frames` and `filtered_frames` are kept as field names for
/// continuity, but for DST they count "frames yielded" / "frames
/// filtered out" — actual decoding happens elsewhere.
pub struct AudioSectorParser {
    /// Frame format (DST, DSD 3-in-14, DSD 3-in-16, etc.)
    frame_format: FrameFormat,
    /// Accumulated audio data for the current frame.
    frame_buffer: Vec<u8>,
    /// Total bytes yielded.
    total_bytes: u64,
    /// Frames yielded for downstream decoding (DST) or written directly (DSD).
    pub decoded_frames: u64,
    /// Frames dropped because they fell outside the timecode filter.
    pub filtered_frames: u64,
    /// Remaining sector count for current DST frame.
    dst_sector_count: i32,
    /// Frame has started.
    frame_started: bool,
    /// Optional timecode filtering (start frame count, end frame count).
    /// When set, only frames with timecodes in [start, end) range are emitted.
    timecode_filter: Option<(u32, u32)>,
    /// Current frame's timecode (as frame count) for filtering.
    current_frame_timecode: Option<u32>,
    /// Timecode of the last yielded DST frame (set on `parse_sector`/`flush`
    /// at the moment the frame is returned). Useful to debug filter range
    /// issues without exposing internal state.
    pub last_yielded_timecode: Option<u32>,
    /// Expected channel count from the area TOC, used to redundantly
    /// validate the per-frame `channel_bits` hint (see C reference's
    /// `get_channel_count` cross-check). Optional — set via
    /// [`AudioSectorParser::set_expected_channel_count`]; if unset, no
    /// validation is performed.
    expected_channel_count: Option<u8>,
}

impl AudioSectorParser {
    /// Create a new audio sector parser. The parser only demuxes sectors
    /// into raw frame bytes and never decodes; channel count and sample
    /// rate are properties of the downstream `DstDecoder`, not the parser.
    pub fn new(frame_format: FrameFormat) -> Result<Self> {
        Ok(Self {
            frame_format,
            frame_buffer: Vec::new(),
            total_bytes: 0,
            dst_sector_count: 0,
            frame_started: false,
            timecode_filter: None,
            current_frame_timecode: None,
            decoded_frames: 0,
            filtered_frames: 0,
            last_yielded_timecode: None,
            expected_channel_count: None,
        })
    }

    /// Tell the parser what the area TOC says the channel count should
    /// be. While set, every frame_info we parse is cross-checked against
    /// the per-frame `channel_bits` hint (matches the redundancy check
    /// the C reference performs via `get_channel_count`). Mismatches are
    /// logged at warn level but do not abort extraction.
    pub fn set_expected_channel_count(&mut self, n: u8) {
        self.expected_channel_count = Some(n);
    }

    /// Set timecode filter to only extract frames within a specific time range
    ///
    /// # Arguments
    /// * `start_minutes`, `start_seconds`, `start_frames` - Start timecode (inclusive)
    /// * `end_minutes`, `end_seconds`, `end_frames` - End timecode (exclusive)
    ///
    /// When set, only frames with timecodes >= start and < end will be extracted.
    /// This matches the C code's audio_frame_trimming behavior.
    pub fn set_timecode_filter(
        &mut self,
        start_minutes: u8,
        start_seconds: u8,
        start_frames: u8,
        end_minutes: u8,
        end_seconds: u8,
        end_frames: u8,
    ) {
        let start_frame_count = start_minutes as u32 * FRAMES_PER_MINUTE
            + start_seconds as u32 * FRAMES_PER_SECOND
            + start_frames as u32;
        let end_frame_count = end_minutes as u32 * FRAMES_PER_MINUTE
            + end_seconds as u32 * FRAMES_PER_SECOND
            + end_frames as u32;
        self.timecode_filter = Some((start_frame_count, end_frame_count));
        log::info!(
            "Timecode filter set: [{:02}:{:02}:{:02} - {:02}:{:02}:{:02}) = [frame {} - frame {})",
            start_minutes,
            start_seconds,
            start_frames,
            end_minutes,
            end_seconds,
            end_frames,
            start_frame_count,
            end_frame_count
        );
    }

    /// Parse an audio sector and extract any complete DST frames it
    /// yields. A sector can contain multiple frame-start packets — when
    /// frames are tiny (e.g., DSD-silence frames that fit in a single
    /// 11-byte packet), one sector can complete N>1 frames. The C
    /// reference handles this via a callback fired on each completion;
    /// we collect into a `Vec` and return them all at once.
    ///
    /// # Arguments
    /// * `sector_data` - Raw sector data (2048 bytes)
    pub fn parse_sector(&mut self, sector_data: &[u8]) -> Result<Vec<Vec<u8>>> {
        let mut yielded: Vec<Vec<u8>> = Vec::new();
        if sector_data.len() < 2048 {
            anyhow::bail!(
                "Audio sector too short: {} bytes (expected 2048)",
                sector_data.len()
            );
        }

        // Debug: log first sector
        static SECTOR_COUNT: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);
        let sector_num = SECTOR_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if sector_num < 3 {
            debug!(
                "[SECTOR #{}] First 16 bytes: {:02x?}",
                sector_num,
                &sector_data[..16]
            );
        }

        // Parse audio frame header (first byte)
        let header = AudioFrameHeader::from_byte(sector_data[0]);
        let mut offset = 1;

        log::debug!(
            "Audio sector header: dst_encoded={}, frame_info_count={}, packet_info_count={}, frame_format={:?}",
            header.dst_encoded,
            header.frame_info_count,
            header.packet_info_count,
            self.frame_format
        );

        // Parse packet info headers
        let mut packets = Vec::new();
        for i in 0..header.packet_info_count {
            if offset + 2 > sector_data.len() {
                anyhow::bail!("Invalid audio sector: packet info extends beyond sector");
            }
            let packet_info = AudioPacketInfo::from_bytes(&sector_data[offset..offset + 2]);

            // Debug: log first few packets with raw bytes
            if sector_num < 3 && i < 3 {
                debug!(
                    "[PKT_INFO sector={} pkt={}] raw_bytes=[{:02x}, {:02x}], frame_start={}, data_type={}, len={}",
                    sector_num,
                    i,
                    sector_data[offset],
                    sector_data[offset + 1],
                    packet_info.frame_start,
                    packet_info.data_type,
                    packet_info.packet_length
                );
            }

            packets.push(packet_info);
            offset += 2;
        }

        // Parse frame info headers
        let frame_info_size = if header.dst_encoded { 4 } else { 3 };
        let mut frame_infos = Vec::new();
        for i in 0..header.frame_info_count {
            if offset + frame_info_size > sector_data.len() {
                anyhow::bail!("Invalid audio sector: frame info extends beyond sector");
            }
            let frame_info = AudioFrameInfo::from_bytes(
                &sector_data[offset..offset + frame_info_size],
                header.dst_encoded,
            );
            log::debug!(
                "Frame info [{}]: timecode={}:{}:{}, sector_count={}",
                i,
                frame_info.minutes,
                frame_info.seconds,
                frame_info.frames,
                frame_info.sector_count
            );
            frame_infos.push(frame_info);
            offset += frame_info_size;
        }

        if offset > sector_data.len() {
            anyhow::bail!(
                "Invalid audio sector: calculated header size {} exceeds sector size {}",
                offset,
                sector_data.len()
            );
        }

        // Process each packet and extract audio data
        let mut frame_info_idx = 0;
        for packet in packets {
            if offset + packet.packet_length as usize > sector_data.len() {
                anyhow::bail!(
                    "Invalid audio sector: packet at offset {} with length {} extends beyond sector",
                    offset,
                    packet.packet_length
                );
            }

            // Mirror the C reference's switch over data_type — recognised
            // packet types are AUDIO (accumulate) and SUPPLEMENTARY/PADDING
            // (explicit no-op). Anything else is unknown and we silently
            // skip it, same as the reference's `default:` arm.
            match packet.data_type {
                DATA_TYPE_AUDIO => {}
                DATA_TYPE_SUPPLEMENTARY | DATA_TYPE_PADDING => {
                    offset += packet.packet_length as usize;
                    continue;
                }
                _ => {
                    offset += packet.packet_length as usize;
                    continue;
                }
            }

            {
                let packet_data = &sector_data[offset..offset + packet.packet_length as usize];

                static PKT_ALL_COUNT: std::sync::atomic::AtomicUsize =
                    std::sync::atomic::AtomicUsize::new(0);
                let pkt_all = PKT_ALL_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if pkt_all < 15 {
                    debug!(
                        "[AUDIO_PKT #{}] frame_start={}, len={}, frame_started={}",
                        pkt_all,
                        packet.frame_start,
                        packet_data.len(),
                        self.frame_started
                    );
                }

                // Debug: log first packet of first few frames
                if self.frame_started && self.frame_buffer.len() < 20 && packet_data.len() >= 8 {
                    debug!(
                        "[PACKET] Adding packet: offset={}, len={}, first_8_bytes={:02x?}, frame_buf_len_before={}",
                        offset,
                        packet_data.len(),
                        &packet_data[..8.min(packet_data.len())],
                        self.frame_buffer.len()
                    );
                }

                trace!(
                    "Audio packet: frame_start={}, length={}, first 8 bytes: {:02x?}",
                    packet.frame_start,
                    packet.packet_length,
                    &packet_data[..8.min(packet_data.len())]
                );

                // Handle DST-compressed audio
                if matches!(self.frame_format, FrameFormat::Dst) {
                    if packet.frame_start {
                        if frame_info_idx < frame_infos.len() {
                            let frame_info = &frame_infos[frame_info_idx];
                            frame_info_idx += 1;

                            // Cross-check per-frame channel hint against the
                            // area TOC, mirroring the C reference's
                            // `get_channel_count` use. Mismatch is logged
                            // (warn) but doesn't abort.
                            if let Some(expected) = self.expected_channel_count {
                                let derived = frame_info.derived_channel_count();
                                if derived != expected {
                                    log::warn!(
                                        "frame channel hint ({}) disagrees with area TOC \
                                         channel count ({}) at timecode {:02}:{:02}:{:02}",
                                        derived,
                                        expected,
                                        frame_info.minutes,
                                        frame_info.seconds,
                                        frame_info.frames,
                                    );
                                }
                            }

                            // If we have a previous frame that's complete, yield it.
                            if self.frame_started
                                && self.dst_sector_count == 0
                                && !self.frame_buffer.is_empty()
                            {
                                let should_yield = match self.timecode_filter {
                                    Some((s, e)) => self
                                        .current_frame_timecode
                                        .map(|tc| tc >= s && tc < e)
                                        .unwrap_or(true),
                                    None => true,
                                };

                                if should_yield {
                                    let yielded_tc = self.current_frame_timecode;
                                    let raw = std::mem::take(&mut self.frame_buffer);
                                    self.total_bytes += raw.len() as u64;
                                    self.decoded_frames += 1;
                                    self.last_yielded_timecode = yielded_tc;
                                    yielded.push(raw);
                                } else {
                                    self.filtered_frames += 1;
                                }
                            }

                            // Start new frame (whether or not previous was yielded).
                            // The bottom-of-loop sector_count decrement runs after
                            // this block, so the new frame's count starts at the
                            // full `frame_info.sector_count` and is decremented to
                            // account for *this* packet.
                            self.frame_buffer.clear();
                            self.frame_buffer.extend_from_slice(packet_data);
                            self.dst_sector_count = frame_info.sector_count as i32;
                            self.frame_started = true;
                            self.current_frame_timecode = Some(frame_info.to_frame_count());
                        }
                    } else {
                        // Continue accumulating current frame
                        if self.frame_started {
                            static PKT_COUNT: std::sync::atomic::AtomicUsize =
                                std::sync::atomic::AtomicUsize::new(0);
                            let pkt_num =
                                PKT_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if pkt_num < 20 {
                                debug!(
                                    "[ACCUM_PKT #{}] Adding {} bytes to frame, new total={}",
                                    pkt_num,
                                    packet_data.len(),
                                    self.frame_buffer.len() + packet_data.len()
                                );
                            }
                            self.frame_buffer.extend_from_slice(packet_data);
                        }
                    }

                    // Decrement sector_count after adding each audio packet (matches C code)
                    if self.frame_started && self.dst_sector_count > 0 {
                        self.dst_sector_count -= 1;
                        log::debug!(
                            "After adding packet: buffer_len={}, sector_count={}",
                            self.frame_buffer.len(),
                            self.dst_sector_count
                        );
                    }
                } else {
                    // Uncompressed DSD - just accumulate
                    self.frame_buffer.extend_from_slice(packet_data);
                    self.total_bytes += packet_data.len() as u64;
                }
            }

            offset += packet.packet_length as usize;
        }

        // For uncompressed DSD formats, return data in chunks to avoid building up too much in memory
        // For DST format, check if frame is complete and decode it
        if !matches!(self.frame_format, FrameFormat::Dst) {
            const CHUNK_SIZE: usize = 32768;
            if self.frame_buffer.len() >= CHUNK_SIZE {
                let chunk_data = std::mem::take(&mut self.frame_buffer);
                yielded.push(chunk_data);
            }
        } else if self.frame_started
            && self.dst_sector_count == 0
            && !self.frame_buffer.is_empty()
        {
            let should_yield = match self.timecode_filter {
                Some((s, e)) => self
                    .current_frame_timecode
                    .map(|tc| tc >= s && tc < e)
                    .unwrap_or(true),
                None => true,
            };

            if should_yield {
                let yielded_tc = self.current_frame_timecode;
                let raw = std::mem::take(&mut self.frame_buffer);
                self.frame_started = false;
                self.dst_sector_count = 0;
                self.total_bytes += raw.len() as u64;
                self.decoded_frames += 1;
                self.last_yielded_timecode = yielded_tc;
                yielded.push(raw);
            } else {
                self.filtered_frames += 1;
                self.frame_buffer.clear();
                self.frame_started = false;
                self.dst_sector_count = 0;
            }
        }
        Ok(yielded)
    }

    /// Yield the final partial frame buffered at end-of-stream, if any. For
    /// DST this returns the *raw* DST frame bytes; the caller still needs to
    /// run them through the decoder.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.frame_buffer.is_empty() {
            None
        } else if matches!(self.frame_format, FrameFormat::Dst) {
            let should_yield = match self.timecode_filter {
                Some((s, e)) => self
                    .current_frame_timecode
                    .map(|tc| tc >= s && tc < e)
                    .unwrap_or(true),
                None => true,
            };
            if !should_yield {
                self.frame_buffer.clear();
                return None;
            }
            let yielded_tc = self.current_frame_timecode;
            let raw = std::mem::take(&mut self.frame_buffer);
            self.total_bytes += raw.len() as u64;
            self.decoded_frames += 1;
            self.last_yielded_timecode = yielded_tc;
            Some(raw)
        } else {
            // Uncompressed DSD - return as-is
            let frame_data = std::mem::take(&mut self.frame_buffer);
            self.total_bytes += frame_data.len() as u64;
            Some(frame_data)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scarletbook::types::FrameFormat;
    use std::path::PathBuf;

    const SECTOR_SIZE: usize = 2048;

    fn fixture(name: &str) -> Vec<u8> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dst_frames")
            .join(name);
        std::fs::read(&p).unwrap_or_else(|e| panic!("missing fixture {}: {}", p.display(), e))
    }

    fn sector_header(frame_info_count: u8, packet_info_count: u8) -> u8 {
        // dst_encoded=1 (bit 0), frame_info_count (bits 2-4), packet_info_count (bits 5-7)
        1 | ((frame_info_count & 7) << 2) | ((packet_info_count & 7) << 5)
    }

    fn pkt_info(frame_start: bool, data_type: u8, len: u16) -> [u8; 2] {
        let b0 = ((frame_start as u8) << 7) | ((data_type & 7) << 3) | ((len >> 8) as u8 & 7);
        let b1 = (len & 0xFF) as u8;
        [b0, b1]
    }

    fn frm_info(m: u8, s: u8, f: u8, sc: u8) -> [u8; 4] {
        // sector_count occupies bits 2-6 of byte 3.
        [m, s, f, (sc & 0x1F) << 2]
    }

    /// Regression test for the off-by-one in `sector_count` when the first
    /// filter check accepts a frame mid-sector and the parser early-returns.
    ///
    /// Layout (5 sectors holding 3 fixture frames A, B, C):
    ///   sector 0: 1 pkt frame_start(A, sc=2), 2041B of A
    ///   sector 1: 2 pkts: rest of A (1057B) + frame_start(B, sc=2), 982B of B
    ///   sector 2: 2 pkts: rest of B (1801B) + frame_start(C, sc=3), 238B of C
    ///   sector 3: 1 pkt continuation of C (2045B)
    ///   sector 4: 1 pkt continuation of C (677B) — completes C
    ///
    /// Without the fix, the early return after decoding A leaves B's sector_count
    /// one too high. When C's frame_start arrives in sector 2, the
    /// `sector_count == 0` precondition fails, B's buffer is clobbered, and B is
    /// silently dropped. With the fix all three frames decode.
    #[test]
    fn early_return_preserves_sector_count_for_following_frame() {
        let frame_a = fixture("frame_000.dst");
        let frame_b = fixture("frame_001.dst");
        let frame_c = fixture("frame_002.dst");
        assert_eq!(frame_a.len(), 3098);
        assert_eq!(frame_b.len(), 2783);
        assert_eq!(frame_c.len(), 2956);

        let mut sectors: Vec<Vec<u8>> = Vec::new();

        // ---- Sector 0: frame_start A, sc=2, 2041B audio.
        let mut s = Vec::with_capacity(SECTOR_SIZE);
        s.push(sector_header(1, 1));
        s.extend_from_slice(&pkt_info(true, 2, 2041));
        s.extend_from_slice(&frm_info(0, 0, 0, 2));
        s.extend_from_slice(&frame_a[..2041]);
        assert_eq!(s.len(), SECTOR_SIZE);
        sectors.push(s);

        // ---- Sector 1: cont A (1057B) + frame_start B (sc=2), 982B of B.
        let mut s = Vec::with_capacity(SECTOR_SIZE);
        s.push(sector_header(1, 2));
        s.extend_from_slice(&pkt_info(false, 2, 1057));
        s.extend_from_slice(&pkt_info(true, 2, 982));
        s.extend_from_slice(&frm_info(0, 0, 1, 2));
        s.extend_from_slice(&frame_a[2041..]);
        s.extend_from_slice(&frame_b[..982]);
        assert_eq!(s.len(), SECTOR_SIZE);
        sectors.push(s);

        // ---- Sector 2: cont B (1801B) + frame_start C (sc=3), 238B of C.
        let mut s = Vec::with_capacity(SECTOR_SIZE);
        s.push(sector_header(1, 2));
        s.extend_from_slice(&pkt_info(false, 2, 1801));
        s.extend_from_slice(&pkt_info(true, 2, 238));
        s.extend_from_slice(&frm_info(0, 0, 2, 3));
        s.extend_from_slice(&frame_b[982..]);
        s.extend_from_slice(&frame_c[..238]);
        assert_eq!(s.len(), SECTOR_SIZE);
        sectors.push(s);

        // ---- Sector 3: 1 pkt continuation of C (2045B).
        let mut s = Vec::with_capacity(SECTOR_SIZE);
        s.push(sector_header(0, 1));
        s.extend_from_slice(&pkt_info(false, 2, 2045));
        s.extend_from_slice(&frame_c[238..238 + 2045]);
        assert_eq!(s.len(), SECTOR_SIZE);
        sectors.push(s);

        // ---- Sector 4: 1 pkt completing C (677B remaining).
        let used = 238 + 2045;
        let remaining = frame_c.len() - used;
        assert_eq!(remaining, 673);
        let mut s = Vec::with_capacity(SECTOR_SIZE);
        s.push(sector_header(0, 1));
        s.extend_from_slice(&pkt_info(false, 2, remaining as u16));
        s.extend_from_slice(&frame_c[used..]);
        // Pad the rest of the sector — real SACD sectors are exactly 2048 bytes.
        s.resize(SECTOR_SIZE, 0);
        assert_eq!(s.len(), SECTOR_SIZE);
        sectors.push(s);

        // Drive the parser. Use a wide filter so all three frames are accepted.
        let mut parser = AudioSectorParser::new(FrameFormat::Dst).expect("parser create");
        parser.set_timecode_filter(0, 0, 0, 0, 0, 100); // [0, 100)

        let mut produced = 0usize;
        for sector in &sectors {
            produced += parser.parse_sector(sector).expect("parse_sector").len();
        }
        if parser.flush().is_some() {
            produced += 1;
        }

        // The bug drops frame B (every other frame), so without the fix we'd
        // see only 1 (A) — possibly 2 if C still completes via end-of-sector.
        // With the fix, all three frames must decode.
        assert_eq!(
            parser.decoded_frames, 3,
            "expected 3 decoded frames; got {} (filtered={}). \
             The sector_count off-by-one bug drops frame B in the early-return path.",
            parser.decoded_frames, parser.filtered_frames
        );
        assert_eq!(produced, 3);
    }

    /// Regression test: a single sector containing multiple back-to-back
    /// frame_start packets must yield ALL of them. The Dylan
    /// `Slow Train Coming` ISO opens with stretches of 11-byte DSD-silence
    /// frames packed many-per-sector; an earlier `parse_sector` returning
    /// `Option<Vec<u8>>` would early-return after the first complete
    /// frame, silently dropping every subsequent packet in the same
    /// sector.
    #[test]
    fn parse_sector_yields_multiple_frames_in_one_sector() {
        // Three tiny frames packed into a single sector. Each frame
        // is 11 bytes (matches the Dylan silence-frame size) and
        // declares `sector_count = 1`, meaning the entire frame fits
        // in one packet within this sector.
        let frame_bytes: [u8; 11] = [0xff, 0x06, 0x00, 0x80, 0x38, 0x10, 0x10, 0x60, 0x01, 0x49, 0x80];

        let mut s = Vec::with_capacity(SECTOR_SIZE);
        s.push(sector_header(3, 3));
        // Three packet info entries.
        s.extend_from_slice(&pkt_info(true, 2, frame_bytes.len() as u16));
        s.extend_from_slice(&pkt_info(true, 2, frame_bytes.len() as u16));
        s.extend_from_slice(&pkt_info(true, 2, frame_bytes.len() as u16));
        // Three frame_info entries, each at sequential timecodes.
        s.extend_from_slice(&frm_info(0, 0, 0, 1));
        s.extend_from_slice(&frm_info(0, 0, 1, 1));
        s.extend_from_slice(&frm_info(0, 0, 2, 1));
        // Three packets of frame data.
        s.extend_from_slice(&frame_bytes);
        s.extend_from_slice(&frame_bytes);
        s.extend_from_slice(&frame_bytes);
        s.resize(SECTOR_SIZE, 0);

        let mut parser = AudioSectorParser::new(FrameFormat::Dst).expect("parser create");
        parser.set_timecode_filter(0, 0, 0, 0, 0, 100);

        let yielded_in_sector = parser.parse_sector(&s).expect("parse_sector");
        let trailing = parser.flush();

        // The first sector should drain frames 0 and 1 directly. Frame 2
        // is still buffered after the sector boundary (sector_count=0
        // but no following frame_start has triggered a yield yet) and
        // gets emitted by `flush`.
        let total_yielded = yielded_in_sector.len() + trailing.is_some() as usize;
        assert_eq!(
            total_yielded, 3,
            "expected 3 frames from one sector; got {} (parse_sector returned {} frames, flush={})",
            total_yielded,
            yielded_in_sector.len(),
            trailing.is_some()
        );
    }
}
