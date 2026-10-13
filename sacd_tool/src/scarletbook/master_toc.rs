use crate::scarletbook::consts;
use crate::scarletbook::types;
use anyhow::Result;
use byteorder::{BigEndian, ReadBytesExt};
use std::io::{Cursor, Read};

/// All bytes of the on-disc Master TOC are parsed into named fields. Some
/// (version, reserved spans, raw dates) are not consumed by the
/// extraction code paths but remain part of the public struct so callers
/// inspecting disc metadata have access to them — same rationale as the
/// C reference's `master_toc_t`, where every member is parsed even if
/// not used internally.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct MasterToc {
    // M_TOC_0_Header (16 bytes)
    pub id: [u8; 8],             // SACDMTOC; Master_TOC_Signature
    pub version: types::Version, // Spec_Version   1.20 / 0x0114
    pub reserved01: [u8; 6],
    // Album_Info (48 bytes)
    pub album_set_size: u16,        // Album_Set_Size,2bytes,  1..6553
    pub album_sequence_number: u16, // Album_Sequence_Number
    pub reserved02: [u8; 4],
    pub album_catalog_number: [u8; 16], // Album_Catalog_Number, 16 bytes, String; 0x00 when empty, else padded with spaces for shorter strings
    pub album_genre: [types::GenreTable; 4], // Album_Genre, 4x4 bytes;
    pub reserved03: [u8; 8],
    // Disc_Info (64 bytes)
    pub area_1_toc_1_start: u32, // LSN for AREA_TOC_1 of 2 channel - 2CH_TOC_1_Address, 4bytes, Uint32, values 0, 544
    pub area_1_toc_2_start: u32, // LSN for AREA_TOC_2 of 2 channel - 2CH_TOC_2_Address, 4bytes, Uint32
    pub area_2_toc_1_start: u32, // LSN for AREA_TOC_1 of M channel - MC_TOC_1_Address, 4bytes, Uint32
    pub area_2_toc_2_start: u32, // LSN for AREA_TOC_2 of M channel   - MC_TOC_2_Address, 4bytes, Uint32
    pub disc_flags: u8,          // Disc_Flags, 1 byte - Hybr, b7
    pub reserved04: [u8; 3],
    pub area_1_toc_size: u16, // Length in Sectors of AREA_TOC of  2ch - 2CH_TOC_Length, 2byte, Uint16, value 0, 5..
    pub area_2_toc_size: u16, // Length in Sectors of AREA_TOC of M channel - MC_TOC_Length, 2byte, Uint16, value 0, 37..
    pub disc_catalog_number: [u8; 16], // Disc_Catalog_Number, 16 bytes, String; 0x00 when empty, else padded with spaces for shorter strings
    pub disc_genre: [types::GenreTable; 4], // Disc_Genre, 4x4 bytes
    pub disc_date_year: u16,           // Disc_Date , 4 bytes
    pub disc_date_month: u8,
    pub disc_date_day: u8,
    pub reserved05: [u8; 4],
    // Text_Channels (40 bytes)
    pub text_area_count: u8, // N_Text_Channels, 1 byte, Uint8  , values =0..8
    pub reserved06: [u8; 7],
    pub locales: [types::LocaleTable; 8], // N_Text_Channels values= 0...8
}

impl MasterToc {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut cursor = Cursor::new(bytes);
        Self::read_from(&mut cursor)
    }

    pub fn read_from<R: Read>(reader: &mut R) -> Result<Self> {
        let mut id = [0u8; 8];
        reader.read_exact(&mut id)?;
        if &id != consts::MASTER_TOC_SIGNATURE {
            anyhow::bail!(
                "Master TOC signature mismatch: expected {:?}, got {:?} — not a Scarlet Book disc",
                std::str::from_utf8(consts::MASTER_TOC_SIGNATURE).unwrap_or("?"),
                String::from_utf8_lossy(&id),
            );
        }

        let version = types::Version::parse(reader)?;

        let mut reserved01 = [0u8; 6];
        reader.read_exact(&mut reserved01)?;

        let album_set_size = reader.read_u16::<BigEndian>()?;
        let album_sequence_number = reader.read_u16::<BigEndian>()?;

        let mut reserved02 = [0u8; 4];
        reader.read_exact(&mut reserved02)?;

        let mut album_catalog_number = [0u8; 16];
        reader.read_exact(&mut album_catalog_number)?;

        let album_genre = [
            types::GenreTable::parse(reader)?,
            types::GenreTable::parse(reader)?,
            types::GenreTable::parse(reader)?,
            types::GenreTable::parse(reader)?,
        ];

        let mut reserved03 = [0u8; 8];
        reader.read_exact(&mut reserved03)?;

        let area_1_toc_1_start = reader.read_u32::<BigEndian>()?;
        let area_1_toc_2_start = reader.read_u32::<BigEndian>()?;
        let area_2_toc_1_start = reader.read_u32::<BigEndian>()?;
        let area_2_toc_2_start = reader.read_u32::<BigEndian>()?;

        let disc_flags = reader.read_u8()?;

        let mut reserved04 = [0u8; 3];
        reader.read_exact(&mut reserved04)?;

        let area_1_toc_size = reader.read_u16::<BigEndian>()?;
        let area_2_toc_size = reader.read_u16::<BigEndian>()?;

        let mut disc_catalog_number = [0u8; 16];
        reader.read_exact(&mut disc_catalog_number)?;

        let disc_genre = [
            types::GenreTable::parse(reader)?,
            types::GenreTable::parse(reader)?,
            types::GenreTable::parse(reader)?,
            types::GenreTable::parse(reader)?,
        ];

        let disc_date_year = reader.read_u16::<BigEndian>()?;
        let disc_date_month = reader.read_u8()?;
        let disc_date_day = reader.read_u8()?;

        let mut reserved05 = [0u8; 4];
        reader.read_exact(&mut reserved05)?;

        // Text_Channels (40 bytes)
        let text_area_count = reader.read_u8()?;

        let mut reserved06 = [0u8; 7];
        reader.read_exact(&mut reserved06)?;

        let locales = [
            types::LocaleTable::parse(reader)?,
            types::LocaleTable::parse(reader)?,
            types::LocaleTable::parse(reader)?,
            types::LocaleTable::parse(reader)?,
            types::LocaleTable::parse(reader)?,
            types::LocaleTable::parse(reader)?,
            types::LocaleTable::parse(reader)?,
            types::LocaleTable::parse(reader)?,
        ];

        Ok(Self {
            id,
            version,
            reserved01,
            album_set_size,
            album_sequence_number,
            reserved02,
            album_catalog_number,
            album_genre,
            reserved03,
            area_1_toc_1_start,
            area_1_toc_2_start,
            area_2_toc_1_start,
            area_2_toc_2_start,
            disc_flags,
            reserved04,
            area_1_toc_size,
            area_2_toc_size,
            disc_catalog_number,
            disc_genre,
            disc_date_year,
            disc_date_month,
            disc_date_day,
            reserved05,
            text_area_count,
            reserved06,
            locales,
        })
    }

    /// Get disc catalog number as string (trimmed)
    pub fn disc_catalog(&self) -> String {
        String::from_utf8_lossy(&self.disc_catalog_number)
            .trim_end_matches('\0')
            .trim()
            .to_string()
    }

    /// Check if disc is hybrid (has both SACD and CD layers)
    pub fn is_hybrid(&self) -> bool {
        (self.disc_flags & 0x80) != 0
    }

    /// Has two-channel area
    pub fn has_two_channel(&self) -> bool {
        self.area_1_toc_1_start != 0
    }

    /// Has multi-channel area
    pub fn has_multi_channel(&self) -> bool {
        self.area_2_toc_1_start != 0
    }

    /// Get disc category (from first genre table entry with category=1)
    pub fn disc_category(&self) -> Option<&'static str> {
        self.disc_genre
            .iter()
            .find(|g| g.category == 1)
            .and_then(|g| g.category_name())
    }

    /// Get disc genre (from first genre table entry with category=1)
    pub fn disc_genre(&self) -> Option<&'static str> {
        self.disc_genre
            .iter()
            .find(|g| g.category == 1)
            .and_then(|g| g.genre_name())
    }
}

#[derive(Debug, Clone, Default)]
pub struct MasterText {
    pub album_title: Option<String>,
    pub album_artist: Option<String>,
    pub album_publisher: Option<String>,
    pub album_copyright: Option<String>,
    pub disc_title: Option<String>,
    pub disc_artist: Option<String>,
    pub disc_publisher: Option<String>,
    pub disc_copyright: Option<String>,
}

impl MasterText {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 2048 {
            anyhow::bail!("Master text data too short");
        }

        let mut cursor = Cursor::new(bytes);

        // Read and verify ID
        let mut id = [0u8; 8];
        cursor.read_exact(&mut id)?;
        if &id != consts::MASTER_TEXT_SIGNATURE {
            anyhow::bail!("Invalid master text signature");
        }

        // Skip reserved
        cursor.set_position(16);

        // Read all the position pointers
        let album_title_pos = cursor.read_u16::<BigEndian>()?;
        let album_artist_pos = cursor.read_u16::<BigEndian>()?;
        let album_publisher_pos = cursor.read_u16::<BigEndian>()?;
        let album_copyright_pos = cursor.read_u16::<BigEndian>()?;
        let _album_title_phonetic_pos = cursor.read_u16::<BigEndian>()?;
        let _album_artist_phonetic_pos = cursor.read_u16::<BigEndian>()?;
        let _album_publisher_phonetic_pos = cursor.read_u16::<BigEndian>()?;
        let _album_copyright_phonetic_pos = cursor.read_u16::<BigEndian>()?;
        let disc_title_pos = cursor.read_u16::<BigEndian>()?;
        let disc_artist_pos = cursor.read_u16::<BigEndian>()?;
        let disc_publisher_pos = cursor.read_u16::<BigEndian>()?;
        let disc_copyright_pos = cursor.read_u16::<BigEndian>()?;

        // Helper function to extract null-terminated string from position
        let extract_string = |pos: u16| -> Option<String> {
            if pos == 0 || pos as usize >= bytes.len() {
                return None;
            }
            let start = pos as usize;
            let end = bytes[start..]
                .iter()
                .position(|&b| b == 0)
                .map(|i| start + i)
                .unwrap_or(bytes.len());
            let s = types::decode_text(&bytes[start..end])
                .trim()
                .to_string();
            if s.is_empty() { None } else { Some(s) }
        };

        Ok(MasterText {
            album_title: extract_string(album_title_pos),
            album_artist: extract_string(album_artist_pos),
            album_publisher: extract_string(album_publisher_pos),
            album_copyright: extract_string(album_copyright_pos),
            disc_title: extract_string(disc_title_pos),
            disc_artist: extract_string(disc_artist_pos),
            disc_publisher: extract_string(disc_publisher_pos),
            disc_copyright: extract_string(disc_copyright_pos),
        })
    }
}
