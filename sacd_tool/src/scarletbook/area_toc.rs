use crate::scarletbook::consts;
use crate::scarletbook::types;
use anyhow::Result;
use byteorder::{BigEndian, ReadBytesExt};
use log::warn;
use std::io::{Cursor, Read};

/// SACD Area Table of Contents
/// Scarlet Book: 'Area_TOC'
/// This struct represents the complete Area TOC structure (2048 bytes)
/// for either 2-channel or multi-channel audio areas on an SACD disc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaToc {
    // ===== A_TOC_0_Header (16 bytes) =====
    /// Area_TOC_Signature: "TWOCHTOC" or "MULCHTOC"
    pub id: [u8; 8],
    /// Spec_Version: 1.20 / 0x0114
    pub version: types::Version,
    /// Area_TOC_Length:  5..40 (total size of TOC); length of the Area_TOC in Sectors
    pub size: u16,

    // ===== Area_Data (112 bytes) =====
    /// Max_Byte_Rate: Max Average Byte Rate of Multiplexed Frames
    pub max_byte_rate: u32,
    /// FS_Code: 0x04 = (64 * 44.1 kHz) - others reserved
    pub sample_frequency: u8,
    /// Frame_Format: (bits 3-0 of Area_Flags byte)
    pub frame_format: types::FrameFormat,
    /// N_Channels
    pub channel_count: u8,
    /// Loudspeaker_Config: (bits 4-0 of Area_Config byte)
    pub loudspeaker_config: u8,
    /// Extra_Setting: (bits 7-5 of Area_Config byte)
    pub extra_setting: u8,
    /// Max_Available_Channels
    pub max_available_channels: u8,
    /// Area_Mute_Flags
    pub area_mute_flags: u8,
    /// Track_Attribute (bits 3-0 of Area_Copy_Management byte)
    pub track_attribute: u8,
    /// Total_Area_Play_Time
    pub total_playtime: types::PlayTime,
    /// Track_Offset
    pub track_offset: u8,
    /// N_Tracks (1-255)
    pub track_count: u8,
    /// Track_Area_Start_Address
    pub track_start: u32,
    /// Track_Area_End_Address
    pub track_end: u32,

    // ===== Text_Channels (40 bytes) =====
    /// N_Text_Channels
    pub text_area_count: u8,
    /// Language and character set for each text channel
    pub languages: Vec<types::LocaleTable>,

    // ===== List_Pointers (16 bytes) =====
    /// Track_Text_Ptr
    pub track_text_offset: u16,
    /// Index_List_Ptr
    pub index_list_offset: u16,
    /// Access_List_Ptr
    pub access_list_offset: u16,

    // ===== Area_Text (1904 bytes) =====
    /// Area_Description_Ptr
    pub area_description_offset: u16,
    /// Area_Copyright_Ptr
    pub copyright_offset: u16,
    /// Area_Description_Phonetic_Ptr
    pub area_description_phonetic_offset: u16,
    /// Area_Copyright_Phonetic_Ptr
    pub copyright_phonetic_offset: u16,
    /// Area_Description (parsed string)
    pub area_description: Option<String>,
    /// Area_Copyright (parsed string)
    pub copyright: Option<String>,
    /// Area_Description_Phonetic (parsed string)
    pub area_description_phonetic: Option<String>,
    /// Area_Copyright_Phonetic (parsed string)
    pub copyright_phonetic: Option<String>,
    /// Area_Text
    pub track_texts: Vec<types::TrackText>,

    // ===== Area_Tracklist_Time (from Track_List_2 / SACDTRL2) =====
    /// Track_Start[tno], Vec of area_tracklist_time_t, one per track
    pub track_times_start: Vec<types::TrackTime>,
    /// Track_Duration[tno], Vec of area_tracklist_time_t, one per track
    pub track_times_duration: Vec<types::TrackTime>,

    // ===== Area_ISRC_Genre (from ISRC_and_Genre_List / SACD_IGL) =====
    /// ISRC_Code[tno], Vec of isrc_t, one per track; International Standard Recording Code
    pub track_isrc: Vec<types::Isrc>,
    /// Track_Genre[tno], one per track (from SACD_IGL block, after ISRCs).
    pub track_genres: Vec<types::GenreTable>,

    // ===== Area_Tracklist_Offset (from Track_List_1 / SACDTRL1) =====
    /// Per-track start LSN. One entry per track.
    pub track_start_lsns: Vec<u32>,
    /// Per-track length in LSNs. One entry per track.
    pub track_length_lsns: Vec<u32>,
}

impl AreaToc {
    /// Parse from complete area data buffer
    ///
    /// # Arguments
    /// * `area_data` - Complete area TOC data (all sectors)
    ///
    /// # Format
    /// SACD uses big-endian byte order for multi-byte integers.
    /// Single-byte bitfields are extracted using the on-disc bit positions (big-endian layout).
    pub fn from_bytes(area_data: &[u8]) -> Result<Self> {
        let mut reader = Cursor::new(area_data);
        let mut id = [0u8; 8];
        reader.read_exact(&mut id)?;
        if &id != consts::AREA_TOC_SIGNATURE_STEREO && &id != consts::AREA_TOC_SIGNATURE_MCH {
            anyhow::bail!(
                "Area TOC signature mismatch: expected {:?} or {:?}, got {:?}",
                std::str::from_utf8(consts::AREA_TOC_SIGNATURE_STEREO).unwrap_or("?"),
                std::str::from_utf8(consts::AREA_TOC_SIGNATURE_MCH).unwrap_or("?"),
                String::from_utf8_lossy(&id),
            );
        }

        let version = types::Version::parse(&mut reader)?;
        let size = reader.read_u16::<BigEndian>()?;

        let mut reserved01 = [0u8; 4];
        reader.read_exact(&mut reserved01)?;

        let max_byte_rate = reader.read_u32::<BigEndian>()?;
        let sample_frequency = reader.read_u8()?;

        // Area_Flags byte: On-disc format is big-endian layout
        // Bits 7-4: reserved02
        // Bits 3-0: frame_format
        let area_flags_byte = reader.read_u8()?;
        let frame_format = types::FrameFormat::from(area_flags_byte & 0x0F);

        let mut reserved03 = [0u8; 10];
        reader.read_exact(&mut reserved03)?;

        let channel_count = reader.read_u8()?;

        // Area_Config byte: On-disc format is big-endian layout
        // Bits 7-5: extra_setting
        // Bits 4-0: loudspeaker_config
        let area_config_byte = reader.read_u8()?;
        let extra_setting = (area_config_byte >> 5) & 0x07;
        let loudspeaker_config = area_config_byte & 0x1F;

        let max_available_channels = reader.read_u8()?;
        let area_mute_flags = reader.read_u8()?;

        let mut reserved04 = [0u8; 12];
        reader.read_exact(&mut reserved04)?;

        // Area_Copy_Management byte: On-disc format is big-endian layout
        // Bits 7-4: reserved05
        // Bits 3-0: track_attribute
        let copy_mgmt_byte = reader.read_u8()?;
        let track_attribute = copy_mgmt_byte & 0x0F;

        let mut reserved06 = [0u8; 15];
        reader.read_exact(&mut reserved06)?;

        let total_playtime = types::PlayTime::parse(&mut reader)?;

        let _reserved07 = reader.read_u8()?;
        let track_offset = reader.read_u8()?;
        let track_count = reader.read_u8()?;

        let mut reserved08 = [0u8; 2];
        reader.read_exact(&mut reserved08)?;

        let track_start = reader.read_u32::<BigEndian>()?;
        let track_end = reader.read_u32::<BigEndian>()?;

        let text_area_count = reader.read_u8()?;

        let mut reserved09 = [0u8; 7];
        reader.read_exact(&mut reserved09)?;

        let mut languages = Vec::with_capacity(consts::MAX_LANGUAGE_COUNT);
        for _ in 0..consts::MAX_LANGUAGE_COUNT {
            languages.push(types::LocaleTable::parse(&mut reader)?);
        }

        let mut reserved091 = [0u8; 8];
        reader.read_exact(&mut reserved091)?;

        let track_text_offset = reader.read_u16::<BigEndian>()?;
        let index_list_offset = reader.read_u16::<BigEndian>()?;
        let access_list_offset = reader.read_u16::<BigEndian>()?;

        let mut reserved10 = [0u8; 10];
        reader.read_exact(&mut reserved10)?;

        let area_description_offset = reader.read_u16::<BigEndian>()?;
        let copyright_offset = reader.read_u16::<BigEndian>()?;
        let area_description_phonetic_offset = reader.read_u16::<BigEndian>()?;
        let copyright_phonetic_offset = reader.read_u16::<BigEndian>()?;

        let mut data = vec![0u8; 1896];
        reader.read_exact(&mut data)?;

        // Parse track text from complete area data
        let track_texts =
            Self::parse_track_text(area_data, track_text_offset, track_count, &languages)?;

        // Parse track times and ISRC from complete area data
        let (track_times_start, track_times_duration) =
            Self::parse_track_times(area_data, size, track_count)?;
        let (track_isrc, track_genres) =
            Self::parse_track_isrc_and_genres(area_data, size, track_count)?;
        let (track_start_lsns, track_length_lsns) =
            Self::parse_track_offsets(area_data, size, track_count)?;

        // Parse area description and copyright strings
        let area_description = if area_description_offset > 0
            && (area_description_offset as usize) < area_data.len()
        {
            Self::read_text_at_offset(area_data, area_description_offset as usize)
        } else {
            None
        };

        let copyright = if copyright_offset > 0 && (copyright_offset as usize) < area_data.len() {
            Self::read_text_at_offset(area_data, copyright_offset as usize)
        } else {
            None
        };

        let area_description_phonetic = if area_description_phonetic_offset > 0
            && (area_description_phonetic_offset as usize) < area_data.len()
        {
            Self::read_text_at_offset(area_data, area_description_phonetic_offset as usize)
        } else {
            None
        };

        let copyright_phonetic = if copyright_phonetic_offset > 0
            && (copyright_phonetic_offset as usize) < area_data.len()
        {
            Self::read_text_at_offset(area_data, copyright_phonetic_offset as usize)
        } else {
            None
        };

        Ok(AreaToc {
            id,
            version,
            size,
            max_byte_rate,
            sample_frequency,
            frame_format,
            channel_count,
            loudspeaker_config,
            extra_setting,
            max_available_channels,
            area_mute_flags,
            track_attribute,
            total_playtime,
            track_offset,
            track_count,
            track_start,
            track_end,
            text_area_count,
            languages,
            track_text_offset,
            index_list_offset,
            access_list_offset,
            area_description_offset,
            copyright_offset,
            area_description_phonetic_offset,
            copyright_phonetic_offset,
            area_description,
            copyright,
            area_description_phonetic,
            copyright_phonetic,
            track_texts,
            track_times_start,
            track_times_duration,
            track_isrc,
            track_genres,
            track_start_lsns,
            track_length_lsns,
        })
    }

    /// Get the area ID as a string (e.g., "TWOCHTOC" or "MULCHTOC")
    pub fn id_string(&self) -> Result<String, std::str::Utf8Error> {
        std::str::from_utf8(&self.id).map(|s| s.to_string())
    }

    /// Parse track text from the raw area data
    ///
    /// This parses the "SACDTTxt" section which contains metadata for each track.
    ///
    /// # Arguments
    /// Read null-terminated text at given offset in area data
    ///
    /// # Arguments
    /// * `area_data` - Complete area data buffer
    /// * `offset` - Byte offset to the text
    ///
    /// # Returns
    /// Decoded text (UTF-8, Windows-1252 fallback), or None if empty
    fn read_text_at_offset(area_data: &[u8], offset: usize) -> Option<String> {
        if offset >= area_data.len() {
            return None;
        }

        // Find null terminator
        let end = area_data[offset..]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(area_data.len() - offset);

        // Decode UTF-8, falling back to Windows-1252 for legacy discs
        // (a Latin-1 title used to be dropped entirely here by
        // `String::from_utf8(...).ok()`).
        Some(types::decode_text(&area_data[offset..offset + end]))
    }

    /// Parse track text
    ///
    /// # Arguments
    /// * `area_data` - Complete area data (multiple sectors, starting from Area TOC)
    /// * `track_text_offset` - Offset in sectors to the track text
    /// * `track_count` - Number of tracks
    /// * `languages` - Language/charset information
    ///
    /// # Returns
    /// Vector of TrackText, one for each track in the area
    fn parse_track_text(
        area_data: &[u8],
        track_text_offset: u16,
        track_count: u8,
        languages: &[types::LocaleTable],
    ) -> Result<Vec<types::TrackText>> {
        if track_text_offset == 0 {
            // No track text present
            return Ok(vec![types::TrackText::default(); track_count as usize]);
        }

        // Track text starts at track_text_offset sectors from the beginning
        let track_text_start = (track_text_offset as usize) * consts::SACD_LSN_SIZE;

        if track_text_start + 8 > area_data.len() {
            anyhow::bail!("Track text offset beyond area data bounds");
        }

        // Verify signature
        let signature = &area_data[track_text_start..track_text_start + 8];
        if signature != consts::AREA_TRACK_TEXT_SIGNATURE {
            anyhow::bail!("Invalid track text signature: expected 'SACDTTxt'");
        }

        let mut track_texts = vec![types::TrackText::default(); track_count as usize];

        // Get character set from first locale
        let _character_set = if !languages.is_empty() {
            languages[0].character_set
        } else {
            2 // Default to ISO-8859-1
        };

        // Parse track text positions
        let positions_start = track_text_start + 8;

        for (track_idx, track_text_slot) in track_texts.iter_mut().enumerate() {
            let pos_offset = positions_start + (track_idx * 2);

            if pos_offset + 2 > area_data.len() {
                continue;
            }

            // Read the track text position (big-endian u16)
            let track_text_pos =
                u16::from_be_bytes([area_data[pos_offset], area_data[pos_offset + 1]]) as usize;

            if track_text_pos == 0 {
                continue; // No text for this track
            }

            let text_start = track_text_start + track_text_pos;

            if text_start >= area_data.len() {
                continue;
            }

            // Read N_Items (number of text items for this track)
            let n_items = area_data[text_start] as usize;

            // Skip 4 bytes total (N_Items + 3 reserved bytes)
            let mut ptr = text_start + 4;

            for _ in 0..n_items {
                if ptr >= area_data.len() {
                    break;
                }

                // Read Text_Type (1 byte)
                let text_type = types::TrackTextType::from(area_data[ptr]);
                ptr += 1;

                // Read Padding1 (1 byte, should be 0x20)
                if ptr >= area_data.len() {
                    break;
                }
                let padding1 = area_data[ptr];
                ptr += 1;

                if padding1 != 0x20 {
                    warn!("Warning: Padding1 is not 0x20 (got 0x{:02x})", padding1);
                }

                // Read null-terminated string
                if ptr >= area_data.len() {
                    break;
                }

                let string_start = ptr;
                while ptr < area_data.len() && area_data[ptr] != 0 {
                    ptr += 1;
                }

                if ptr >= area_data.len() {
                    break;
                }

                let string_bytes = &area_data[string_start..ptr];

                // Decode UTF-8, falling back to Windows-1252/Latin-1 for
                // discs that store single-byte text (accents like "á"
                // survive intact instead of becoming U+FFFD).
                let text = types::decode_text(string_bytes);

                // Store in the appropriate field
                track_text_slot.set_text(text_type, text);

                // Skip null terminator
                ptr += 1;

                // Skip Padding2 (0-3 bytes of 0x00)
                while ptr < area_data.len() && area_data[ptr] == 0 {
                    ptr += 1;
                }
            }
        }

        Ok(track_texts)
    }

    /// Parse Area_Tracklist_Time (Track_List_2) from the raw area data
    ///
    /// Searches for "SACDTRL2" (Track_List_2_Signature) sector containing:
    /// - Track_Start[255]: area_tracklist_time_t array for start times
    /// - Track_Duration[255]: area_tracklist_time_t array for durations
    ///
    /// # Arguments
    /// * `area_data` - Complete area data (multiple sectors, starting from Area TOC)
    /// * `size` - Area_TOC_Length in sectors
    /// * `track_count` - N_Tracks (1..255)
    ///
    /// # Returns
    /// Tuple of (Track_Start[], Track_Duration[]) vectors
    fn parse_track_times(
        area_data: &[u8],
        size: u16,
        track_count: u8,
    ) -> Result<(Vec<types::TrackTime>, Vec<types::TrackTime>)> {
        // Search for SACDTRL2 signature
        let mut offset = consts::SACD_LSN_SIZE; // Skip first sector (Area TOC header)
        let end_offset = (size as usize) * consts::SACD_LSN_SIZE;

        while offset + 8 <= end_offset {
            if &area_data[offset..offset + 8] == consts::AREA_TRACK_LIST_2_SIGNATURE {
                // Found track time list
                let mut reader = Cursor::new(&area_data[offset..]);

                // Skip signature
                reader.set_position(8);

                let mut start_times = Vec::with_capacity(track_count as usize);
                let mut durations = Vec::with_capacity(track_count as usize);

                // SACDTRL2 structure has 255 fixed entries for start times
                // Read all 255 start times, but only keep track_count entries
                for i in 0..255 {
                    let time = types::TrackTime::parse(&mut reader)?;
                    if i < track_count {
                        start_times.push(time);
                    }
                }

                // Then read all 255 durations, but only keep track_count entries
                for i in 0..255 {
                    let time = types::TrackTime::parse(&mut reader)?;
                    if i < track_count {
                        durations.push(time);
                    }
                }

                return Ok((start_times, durations));
            }
            offset += consts::SACD_LSN_SIZE;
        }

        // If not found, return empty vectors
        Ok((vec![], vec![]))
    }

    /// Parse Area_ISRC_Genre (ISRC_and_Genre_List) from the raw area data
    ///
    /// Searches for "SACD_IGL" (ISRC_and_Genre_List_Signature) sector containing:
    /// - ISRC_Code[255]: isrc_t array (12 bytes each)
    /// - Track_Genre[255]: genre_table_t array (4 bytes each)
    ///
    /// # Arguments
    /// * `area_data` - Complete area data (multiple sectors, starting from Area TOC)
    /// * `size` - Area_TOC_Length in sectors
    /// * `track_count` - N_Tracks (1..255)
    ///
    /// # Returns
    /// Vector of ISRC_Code[] (one per track)
    fn parse_track_isrc_and_genres(
        area_data: &[u8],
        size: u16,
        track_count: u8,
    ) -> Result<(Vec<types::Isrc>, Vec<types::GenreTable>)> {
        // Search for SACD_IGL signature
        let mut offset = consts::SACD_LSN_SIZE; // Skip first sector (Area TOC header)
        let end_offset = (size as usize) * consts::SACD_LSN_SIZE;

        while offset + 8 <= end_offset {
            if &area_data[offset..offset + 8] == consts::AREA_ISRC_GENRE_SIGNATURE {
                let mut reader = Cursor::new(&area_data[offset..]);
                reader.set_position(8);

                let mut isrc_codes = Vec::with_capacity(track_count as usize);
                for i in 0..255 {
                    let isrc = types::Isrc::parse(&mut reader)?;
                    if i < track_count {
                        isrc_codes.push(isrc);
                    }
                }

                let mut genres = Vec::with_capacity(track_count as usize);
                for i in 0..255 {
                    let g = types::GenreTable::parse(&mut reader)?;
                    if i < track_count {
                        genres.push(g);
                    }
                }

                return Ok((isrc_codes, genres));
            }
            offset += consts::SACD_LSN_SIZE;
        }
        Ok((vec![], vec![]))
    }

    /// Parse Track_List_1 (`SACDTRL1`) which holds explicit per-track LSN
    /// ranges. Layout: 8-byte signature + 255 BE u32 `track_start_lsn` +
    /// 255 BE u32 `track_length_lsn` (total 2048 bytes, one sector).
    fn parse_track_offsets(
        area_data: &[u8],
        size: u16,
        track_count: u8,
    ) -> Result<(Vec<u32>, Vec<u32>)> {
        let mut offset = consts::SACD_LSN_SIZE;
        let end_offset = (size as usize) * consts::SACD_LSN_SIZE;
        while offset + 8 <= end_offset {
            if &area_data[offset..offset + 8] == consts::AREA_TRACK_LIST_1_SIGNATURE {
                let mut reader = Cursor::new(&area_data[offset..]);
                reader.set_position(8);
                let mut starts = Vec::with_capacity(track_count as usize);
                for _ in 0..255 {
                    starts.push(reader.read_u32::<BigEndian>()?);
                }
                starts.truncate(track_count as usize);
                let mut lengths = Vec::with_capacity(track_count as usize);
                for _ in 0..255 {
                    lengths.push(reader.read_u32::<BigEndian>()?);
                }
                lengths.truncate(track_count as usize);
                return Ok((starts, lengths));
            }
            offset += consts::SACD_LSN_SIZE;
        }
        warn!("SACDTRL1 not found in area TOC; falling back to estimated track ranges");
        Ok((vec![], vec![]))
    }
}
