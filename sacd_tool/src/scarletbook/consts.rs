pub const START_OF_MASTER_TOC: u32 = 510;
pub const MASTER_TOC_LEN: u32 = 10;
pub const MAX_LANGUAGE_COUNT: usize = 8;
pub const SACD_LSN_SIZE: usize = 2048;

/// SACD timecode rate — 75 frames per second, inherited from Red Book CD.
pub const FRAMES_PER_SECOND: u32 = 75;
/// `m * SECONDS_PER_MINUTE * FRAMES_PER_SECOND + s * FRAMES_PER_SECOND + f`
/// converts an `(m:s:f)` SACD timecode to an absolute frame count.
pub const SECONDS_PER_MINUTE: u32 = 60;
/// Frames per minute, derived (`60 * 75`).
pub const FRAMES_PER_MINUTE: u32 = SECONDS_PER_MINUTE * FRAMES_PER_SECOND;

/// DSD64: 64 × 44.1 kHz. The base SACD rate.
pub const DSD64_SAMPLE_RATE: u32 = 2_822_400;
/// DSD128: 128 × 44.1 kHz.
pub const DSD128_SAMPLE_RATE: u32 = 5_644_800;
/// DSD256: 256 × 44.1 kHz.
pub const DSD256_SAMPLE_RATE: u32 = 11_289_600;

// Eight-byte ASCII signatures that mark each on-disc structure. Kept as
// named constants here (rather than inlined as byte literals at parser
// sites, the way the C reference does) so all magic strings live in one
// place.
pub const MASTER_TOC_SIGNATURE: &[u8; 8] = b"SACDMTOC";
pub const MASTER_TEXT_SIGNATURE: &[u8; 8] = b"SACDText";
pub const AREA_TOC_SIGNATURE_STEREO: &[u8; 8] = b"TWOCHTOC";
pub const AREA_TOC_SIGNATURE_MCH: &[u8; 8] = b"MULCHTOC";
pub const AREA_TRACK_TEXT_SIGNATURE: &[u8; 8] = b"SACDTTxt";
pub const AREA_TRACK_LIST_1_SIGNATURE: &[u8; 8] = b"SACDTRL1";
pub const AREA_TRACK_LIST_2_SIGNATURE: &[u8; 8] = b"SACDTRL2";
pub const AREA_ISRC_GENRE_SIGNATURE: &[u8; 8] = b"SACD_IGL";
pub const GENRE_NAMES: [&str; 29] = [
    "Not used",
    "Not defined",
    "Adult Contemporary",
    "Alternative Rock",
    "Children's Music",
    "Classical",
    "Contemporary Christian",
    "Country",
    "Dance",
    "Easy Listening",
    "Erotic",
    "Folk",
    "Gospel",
    "Hip Hop",
    "Jazz",
    "Latin",
    "Musical",
    "New Age",
    "Opera",
    "Operetta",
    "Pop Music",
    "RAP",
    "Reggae",
    "Rock Music",
    "Rhythm & Blues",
    "Sound Effects",
    "Sound Track",
    "Spoken Word",
    "World Music",
];
