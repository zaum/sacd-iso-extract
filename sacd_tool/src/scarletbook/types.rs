use crate::scarletbook::consts;
use anyhow::Result;
use byteorder::{BigEndian, ReadBytesExt};
use std::io::{self, Read};

/// Decode a Scarlet Book text field into a String.
///
/// The spec nominally stores UTF-8, but in practice many discs use a
/// single-byte code page (ISO-8859-1 / Windows-1252 — e.g. "János" as the
/// single byte 0xE1). Valid UTF-8 passes through unchanged; anything else
/// falls back to Windows-1252 (a superset of Latin-1 for printable text)
/// so accented characters survive intact instead of turning into U+FFFD
/// replacement boxes that later mojibake into "ï¿½" on the UI.
pub fn decode_text(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    // Windows-1252 mapping for 0x80..0x9F (undefined in ISO-8859-1).
    const CP1252_80_9F: [char; 32] = [
        '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}',
        '\u{017D}', '\u{008F}', '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}',
        '\u{2022}', '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}',
        '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
    ];
    bytes
        .iter()
        .map(|&b| {
            if b < 0x80 {
                b as char // ASCII
            } else if b < 0xA0 {
                CP1252_80_9F[(b - 0x80) as usize]
            } else {
                b as char // 0xA0..0xFF == Latin-1 == U+00A0..U+00FF
            }
        })
        .collect()
}

/// Frame format types for SACD audio
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameFormat {
    /// DST coded. Flexible format
    Dst = 0,
    /// Reserved
    Reserved = 1,
    /// Fixed format. 2-Channel Stereo, Plain DSD, 3 Frames in 14 Sectors
    Dsd3In14 = 2,
    /// Fixed format. 2-Channel Stereo, Plain DSD, 3 Frames in 16 Sectors
    Dsd3In16 = 3,
    /// Undocumented format 4 (appears on some discs, similar to DSD)
    Dsd4 = 4,
    /// Undocumented format 5 (appears on some discs, similar to DSD)
    Dsd5 = 5,
    /// Undocumented format 6 (appears on some discs, similar to DSD)
    Dsd6 = 6,
    /// Undocumented format 7 (appears on some discs, similar to DSD)
    Dsd7 = 7,
    /// Reserved for future use (8..15)
    Unknown(u8),
}

impl From<u8> for FrameFormat {
    fn from(val: u8) -> Self {
        match val {
            0 => FrameFormat::Dst,
            1 => FrameFormat::Reserved,
            2 => FrameFormat::Dsd3In14,
            3 => FrameFormat::Dsd3In16,
            4 => FrameFormat::Dsd4,
            5 => FrameFormat::Dsd5,
            6 => FrameFormat::Dsd6,
            7 => FrameFormat::Dsd7,
            n => FrameFormat::Unknown(n),
        }
    }
}

impl FrameFormat {
    /// Get the number of sectors per frame (for calculating track boundaries)
    /// Returns None if unknown or not applicable
    pub fn sectors_per_frame(&self) -> Option<u32> {
        match self {
            FrameFormat::Dst => Some(14), // DST uses similar frame structure as DSD
            FrameFormat::Dsd3In14 => Some(14),
            FrameFormat::Dsd3In16 => Some(16),
            // For undocumented formats, we'll try 14 as a reasonable default
            FrameFormat::Dsd4 | FrameFormat::Dsd5 | FrameFormat::Dsd6 | FrameFormat::Dsd7 => {
                Some(14)
            }
            _ => None,
        }
    }
}

/// Total_Area_Play_Time
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayTime {
    pub minutes: u8, // 0-255
    pub seconds: u8, // 0-59
    pub frames: u8,  // 0-74
}

impl PlayTime {
    pub fn parse<R: Read>(reader: &mut R) -> io::Result<Self> {
        Ok(PlayTime {
            minutes: reader.read_u8()?,
            seconds: reader.read_u8()?,
            frames: reader.read_u8()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenreTable {
    pub category: u8, // Genre_Table_Category, 1 byte, Uint8; 0=Not used, 1=General, 2=Japanese
    pub reserved: u8,
    pub genre: u16, // Genre_Table_Genre, 2 bytes, Uint16
}

impl GenreTable {
    pub fn parse<R: Read>(reader: &mut R) -> Result<Self> {
        Ok(Self {
            category: reader.read_u8()?,
            reserved: reader.read_u8()?,
            genre: reader.read_u16::<BigEndian>()?,
        })
    }

    /// Get category name
    pub fn category_name(&self) -> Option<&'static str> {
        match self.category {
            0 => Some("Not used"),
            1 => Some("General"),
            2 => Some("Japanese"),
            _ => None,
        }
    }

    /// Get genre name (only valid when category == 1)
    pub fn genre_name(&self) -> Option<&'static str> {
        if self.category == 1 && (self.genre as usize) < consts::GENRE_NAMES.len() {
            Some(consts::GENRE_NAMES[self.genre as usize])
        } else {
            None
        }
    }
}

/// Language and character set information
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocaleTable {
    /// ISO-639-1 language code (e.g., "en")
    pub language_code: [u8; 2],
    /// Character set code (1-7)
    pub character_set: u8,
    pub _reserved: u8,
}

impl LocaleTable {
    pub fn parse<R: Read>(reader: &mut R) -> Result<Self> {
        let mut language_code = [0u8; 2];
        reader.read_exact(&mut language_code)?;
        Ok(Self {
            language_code,
            character_set: reader.read_u8()?,
            _reserved: reader.read_u8()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u8,
    pub minor: u8,
}
impl Version {
    pub fn parse<R: Read>(reader: &mut R) -> Result<Self> {
        Ok(Self {
            major: reader.read_u8()?,
            minor: reader.read_u8()?,
        })
    }
}

/// Track text type identifiers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TrackTextType {
    Title = 0x01,
    Performer = 0x02,
    Songwriter = 0x03,
    Composer = 0x04,
    Arranger = 0x05,
    Message = 0x06,
    ExtraMessage = 0x07,
    Copyright = 0x08,
    TitlePhonetic = 0x81,
    PerformerPhonetic = 0x82,
    SongwriterPhonetic = 0x83,
    ComposerPhonetic = 0x84,
    ArrangerPhonetic = 0x85,
    MessagePhonetic = 0x86,
    ExtraMessagePhonetic = 0x87,
    CopyrightPhonetic = 0x88,
    Unknown(u8),
}

impl From<u8> for TrackTextType {
    fn from(val: u8) -> Self {
        match val {
            0x01 => TrackTextType::Title,
            0x02 => TrackTextType::Performer,
            0x03 => TrackTextType::Songwriter,
            0x04 => TrackTextType::Composer,
            0x05 => TrackTextType::Arranger,
            0x06 => TrackTextType::Message,
            0x07 => TrackTextType::ExtraMessage,
            0x08 => TrackTextType::Copyright,
            0x81 => TrackTextType::TitlePhonetic,
            0x82 => TrackTextType::PerformerPhonetic,
            0x83 => TrackTextType::SongwriterPhonetic,
            0x84 => TrackTextType::ComposerPhonetic,
            0x85 => TrackTextType::ArrangerPhonetic,
            0x86 => TrackTextType::MessagePhonetic,
            0x87 => TrackTextType::ExtraMessagePhonetic,
            0x88 => TrackTextType::CopyrightPhonetic,
            n => TrackTextType::Unknown(n),
        }
    }
}

/// Area_Tracklist_Time (from SACDTRL2)
/// Track time information for start time or duration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackTime {
    pub minutes: u8, // Minutes, 1 byte, Uint8, values 0..255
    pub seconds: u8, // Seconds, 1 byte, Uint8, values 0..59
    pub frames: u8,  // Frames, 1 byte, Uint8, values 0..74
    pub flags: u8,   // Track_Flags, 1 byte; b7=ILP, b4-b1=TMF4-TMF1, b6,b5,b0 reserved
}

impl TrackTime {
    pub fn parse<R: Read>(reader: &mut R) -> io::Result<Self> {
        Ok(TrackTime {
            minutes: reader.read_u8()?,
            seconds: reader.read_u8()?,
            frames: reader.read_u8()?,
            flags: reader.read_u8()?,
        })
    }
}

/// ISRC (International Standard Recording Code) from Area_ISRC_Genre (SACD_IGL)
/// Format: ISRC_Code[tno], 12 bytes, String
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Isrc {
    pub country_code: [u8; 2], // Country_Code, 2 bytes, String; ISO-3166-1 alpha-2 (e.g., "GB")
    pub owner_code: [u8; 3],   // Owner_Code, 3 bytes, String; alphanumeric (e.g., "AAA")
    pub recording_year: [u8; 2], // Year_of_Recording, 2 bytes, String; last 2 digits of year (e.g., "94")
    pub designation_code: [u8; 5], // Designation_Code, 5 bytes, String; numeric (e.g., "00468")
}

impl Isrc {
    pub fn parse<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut country_code = [0u8; 2];
        reader.read_exact(&mut country_code)?;
        let mut owner_code = [0u8; 3];
        reader.read_exact(&mut owner_code)?;
        let mut recording_year = [0u8; 2];
        reader.read_exact(&mut recording_year)?;
        let mut designation_code = [0u8; 5];
        reader.read_exact(&mut designation_code)?;

        Ok(Isrc {
            country_code,
            owner_code,
            recording_year,
            designation_code,
        })
    }

    /// Check if this ISRC has valid data (non-zero country code)
    pub fn is_valid(&self) -> bool {
        self.country_code[0] != 0
    }
}

impl std::fmt::Display for Isrc {
    /// Render as the standard 12-char ISRC string
    /// (`country` + `owner` + `year` + `designation`, e.g. "GBAAA9400468").
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.country_code))?;
        f.write_str(&String::from_utf8_lossy(&self.owner_code))?;
        f.write_str(&String::from_utf8_lossy(&self.recording_year))?;
        f.write_str(&String::from_utf8_lossy(&self.designation_code))
    }
}

/// Track metadata text
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackText {
    pub title: Option<String>,
    pub performer: Option<String>,
    pub songwriter: Option<String>,
    pub composer: Option<String>,
    pub arranger: Option<String>,
    pub message: Option<String>,
    pub extra_message: Option<String>,
    pub copyright: Option<String>,
    pub title_phonetic: Option<String>,
    pub performer_phonetic: Option<String>,
    pub songwriter_phonetic: Option<String>,
    pub composer_phonetic: Option<String>,
    pub arranger_phonetic: Option<String>,
    pub message_phonetic: Option<String>,
    pub extra_message_phonetic: Option<String>,
    pub copyright_phonetic: Option<String>,
}

impl TrackText {
    pub fn set_text(&mut self, text_type: TrackTextType, text: String) {
        if text.is_empty() {
            return;
        }

        let target = match text_type {
            TrackTextType::Title => &mut self.title,
            TrackTextType::Performer => &mut self.performer,
            TrackTextType::Songwriter => &mut self.songwriter,
            TrackTextType::Composer => &mut self.composer,
            TrackTextType::Arranger => &mut self.arranger,
            TrackTextType::Message => &mut self.message,
            TrackTextType::ExtraMessage => &mut self.extra_message,
            TrackTextType::Copyright => &mut self.copyright,
            TrackTextType::TitlePhonetic => &mut self.title_phonetic,
            TrackTextType::PerformerPhonetic => &mut self.performer_phonetic,
            TrackTextType::SongwriterPhonetic => &mut self.songwriter_phonetic,
            TrackTextType::ComposerPhonetic => &mut self.composer_phonetic,
            TrackTextType::ArrangerPhonetic => &mut self.arranger_phonetic,
            TrackTextType::MessagePhonetic => &mut self.message_phonetic,
            TrackTextType::ExtraMessagePhonetic => &mut self.extra_message_phonetic,
            TrackTextType::CopyrightPhonetic => &mut self.copyright_phonetic,
            TrackTextType::Unknown(_) => return,
        };

        *target = Some(text);
    }
}
