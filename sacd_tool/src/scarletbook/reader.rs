use anyhow::{Context, Result};
use chrono;
use log::warn;
use std::fs::File;
use std::io::Write;

use crate::sacd_reader::SacdReader;
use crate::scarletbook::{
    area_toc::AreaToc,
    consts,
    master_toc::{MasterText, MasterToc},
    types,
};

pub struct ScarletBookReader<R: SacdReader> {
    reader: R,
    master_toc: MasterToc,
    master_text: Option<MasterText>,
    stereo_toc: Option<AreaToc>,
    mch_toc: Option<AreaToc>,
    total_sectors: Option<u32>,
}

pub fn new<R: SacdReader>(mut reader: R) -> Result<ScarletBookReader<R>> {
    let master_toc = read_master_toc(&mut reader)?;

    let master_text = read_master_text(&mut reader);

    let stereo_toc = read_stereo_toc(&master_toc, &mut reader);
    let mch_toc = read_mch_toc(&master_toc, &mut reader);

    let sbreader = ScarletBookReader {
        reader,
        master_toc,
        master_text,
        stereo_toc,
        mch_toc,
        total_sectors: None,
    };

    Ok(sbreader)
}

impl<R: SacdReader> ScarletBookReader<R> {
    pub fn get_master_toc(&self) -> MasterToc {
        self.master_toc.clone()
    }

    pub fn get_stereo_toc(&self) -> Option<AreaToc> {
        self.stereo_toc.clone()
    }

    pub fn get_mch_toc(&self) -> Option<AreaToc> {
        self.mch_toc.clone()
    }

    pub fn get_master_text(&self) -> Option<&MasterText> {
        self.master_text.as_ref()
    }

    pub fn get_reader_mut(&mut self) -> &mut R {
        &mut self.reader
    }

    /// Consume the ScarletBookReader and return the inner reader
    pub fn into_reader(self) -> R {
        self.reader
    }

    /// Print disc and track information to stdout
    pub fn print_disc_info(&mut self) {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        let _ = self.write_disc_info(&mut handle);
    }

    /// Write disc and track information to any Write implementation
    fn write_disc_info<W: Write>(&mut self, writer: &mut W) -> Result<()> {
        self.write_version_info(writer)?;
        self.write_disc_info_section(writer)?;
        self.write_album_info_section(writer)?;
        self.write_area_count(writer)?;

        let mut area_idx = 0;
        if let Some(ref stereo_toc) = self.stereo_toc {
            self.write_area_toc(writer, stereo_toc, area_idx)?;
            area_idx += 1;
        }

        if let Some(ref mch_toc) = self.mch_toc {
            self.write_area_toc(writer, mch_toc, area_idx)?;
        }

        self.write_disc_size(writer)?;
        Ok(())
    }

    /// Write version and current date
    fn write_version_info<W: Write>(&self, writer: &mut W) -> Result<()> {
        let now = chrono::Local::now();
        writeln!(
            writer,
            "sacd-rs version {} ({})",
            env!("CARGO_PKG_VERSION"),
            now.format("%Y-%m-%d")
        )?;
        writeln!(writer)?;
        Ok(())
    }

    /// Write disc and track information to a file
    pub fn write_disc_info_to_file<P: AsRef<std::path::Path>>(&mut self, path: P) -> Result<()> {
        let mut file = File::create(path)?;
        self.write_disc_info(&mut file)
    }

    fn write_disc_info_section<W: Write>(&self, writer: &mut W) -> Result<()> {
        let mtoc = &self.master_toc;

        writeln!(writer, "Disc Information:")?;
        writeln!(
            writer,
            "    Version: {:2}.{:02}",
            mtoc.version.major, mtoc.version.minor
        )?;
        writeln!(
            writer,
            "    Creation date: {:04}-{:02}-{:02}",
            mtoc.disc_date_year, mtoc.disc_date_month, mtoc.disc_date_day
        )?;

        // Print disc flags
        writeln!(writer, "    Hybrid Disc: {}", mtoc.is_hybrid())?;
        writeln!(writer, "    Stereo Disc: {}", mtoc.has_two_channel())?;
        writeln!(
            writer,
            "    Multi-channel Disc: {}",
            mtoc.has_multi_channel()
        )?;

        let disc_catalog = mtoc.disc_catalog();
        if !disc_catalog.is_empty() {
            writeln!(writer, "    Disc Catalog Number: {}", disc_catalog)?;
        }

        // Print disc category and genre
        if let Some(category) = mtoc.disc_category() {
            writeln!(writer, "    Disc Category: {}", category)?;
        }
        if let Some(genre) = mtoc.disc_genre() {
            writeln!(writer, "    Disc Genre: {}", genre)?;
        }

        // Print locales
        for locale in &mtoc.locales {
            let lang_code = String::from_utf8_lossy(&locale.language_code);
            let lang_code = lang_code.trim_end_matches('\0');
            if !lang_code.is_empty() {
                let charset_name = match locale.character_set & 0x07 {
                    0 => "US-ASCII",
                    1 => "ISO646-JP",
                    2 => "ISO-8859-1",
                    3 => "SHIFT_JISX0213",
                    4 => "KSC5601.1987-0",
                    5 => "GB2312.1980-0",
                    6 => "BIG5",
                    7 => "ISO-8859-1",
                    _ => "Unknown",
                };
                writeln!(
                    writer,
                    "    Locale: {}, Code character set:[{}], {}",
                    lang_code, locale.character_set, charset_name
                )?;
            }
        }

        // Print disc text from master text
        if let Some(ref mt) = self.master_text {
            if let Some(ref title) = mt.disc_title {
                writeln!(writer, "    Title: {}", title)?;
            }
            if let Some(ref artist) = mt.disc_artist {
                writeln!(writer, "    Artist: {}", artist)?;
            }
            if let Some(ref publisher) = mt.disc_publisher {
                writeln!(writer, "    Publisher: {}", publisher)?;
            }
            if let Some(ref copyright) = mt.disc_copyright {
                writeln!(writer, "    Copyright: {}", copyright)?;
            }
        }
        Ok(())
    }

    fn write_album_info_section<W: Write>(&self, writer: &mut W) -> Result<()> {
        let mtoc = &self.master_toc;

        writeln!(writer)?;
        writeln!(writer, "Album Information:")?;

        let album_catalog = String::from_utf8_lossy(&mtoc.album_catalog_number)
            .trim_end_matches('\0')
            .trim()
            .to_string();
        writeln!(writer, "    Album Catalog Number: {}", album_catalog)?;

        writeln!(
            writer,
            "    Sequence Number: {}",
            mtoc.album_sequence_number
        )?;
        writeln!(writer, "    Set Size: {}", mtoc.album_set_size)?;

        // Print album text from master text
        if let Some(ref mt) = self.master_text {
            if let Some(ref title) = mt.album_title {
                writeln!(writer, "    Title: {}", title)?;
            }
            if let Some(ref artist) = mt.album_artist {
                writeln!(writer, "    Artist: {}", artist)?;
            }
            if let Some(ref publisher) = mt.album_publisher {
                writeln!(writer, "    Publisher: {}", publisher)?;
            }
            if let Some(ref copyright) = mt.album_copyright {
                writeln!(writer, "    Copyright: {}", copyright)?;
            }
        }
        Ok(())
    }

    fn write_area_count<W: Write>(&self, writer: &mut W) -> Result<()> {
        let mut area_count = 0;
        if self.stereo_toc.is_some() {
            area_count += 1;
        }
        if self.mch_toc.is_some() {
            area_count += 1;
        }
        writeln!(writer)?;
        writeln!(writer, "Area Count: {}", area_count)?;
        Ok(())
    }

    fn write_area_toc<W: Write>(
        &self,
        writer: &mut W,
        area_toc: &AreaToc,
        area_idx: usize,
    ) -> Result<()> {
        writeln!(writer, "    Area Information [{}]:", area_idx)?;
        writeln!(writer)?;
        writeln!(
            writer,
            "    Version: {:2}.{:02}",
            area_toc.version.major, area_toc.version.minor
        )?;

        // Print area type
        if let Ok(id_str) = area_toc.id_string() {
            writeln!(writer, "    Area ID: {}", id_str)?;
        }

        // Print area description if present
        if let Some(ref description) = area_toc.area_description {
            writeln!(writer, "    Area Description: {}", description)?;
        }

        writeln!(writer, "    Track Count: {}", area_toc.track_count)?;
        writeln!(
            writer,
            "    Total Play Time: {:02}:{:02}:{:02} [mins:secs:frames]",
            area_toc.total_playtime.minutes,
            area_toc.total_playtime.seconds,
            area_toc.total_playtime.frames
        )?;
        writeln!(
            writer,
            "    Speaker Config: {} Channel",
            area_toc.channel_count
        )?;

        // Print frame format (DST or DSD)
        let format_str = match area_toc.frame_format {
            types::FrameFormat::Dst => "DST (compressed)",
            types::FrameFormat::Dsd3In14 => "DSD 3-in-14 (uncompressed)",
            types::FrameFormat::Dsd3In16 => "DSD 3-in-16 (uncompressed)",
            types::FrameFormat::Dsd4 => "DSD format 4 (uncompressed)",
            types::FrameFormat::Dsd5 => "DSD format 5 (uncompressed)",
            types::FrameFormat::Dsd6 => "DSD format 6 (uncompressed)",
            types::FrameFormat::Dsd7 => "DSD format 7 (uncompressed)",
            types::FrameFormat::Reserved => "Reserved",
            types::FrameFormat::Unknown(val) => {
                writeln!(writer, "    Frame Format: Unknown ({})", val)?;
                ""
            }
        };
        if !format_str.is_empty() {
            writeln!(writer, "    Frame Format: {}", format_str)?;
        }

        writeln!(writer, "    Track List [{}]:", area_idx)?;
        for (i, track_text) in area_toc.track_texts.iter().enumerate() {
            if let Some(ref title) = track_text.title {
                writeln!(writer, "        Title[{}]: {}", i, title)?;
            }
            if let Some(ref performer) = track_text.performer {
                writeln!(writer, "        Performer[{}]: {}", i, performer)?;
            }
            if let Some(ref composer) = track_text.composer {
                writeln!(writer, "        Composer[{}]: {}", i, composer)?;
            }

            // Print track start time and duration from Area_Tracklist_Time (SACDTRL2)
            if i < area_toc.track_times_start.len() && i < area_toc.track_times_duration.len() {
                let start = &area_toc.track_times_start[i];
                let duration = &area_toc.track_times_duration[i];
                writeln!(
                    writer,
                    "        Track_Start_Time_Code: {:02}:{:02}:{:02} [mins:secs:frames]",
                    start.minutes, start.seconds, start.frames
                )?;
                writeln!(
                    writer,
                    "        Duration: {:02}:{:02}:{:02} [mins:secs:frames]",
                    duration.minutes, duration.seconds, duration.frames
                )?;
            }

            writeln!(writer)?;
        }

        // Print ISRC information from Area_ISRC_Genre (SACD_IGL)
        for (i, isrc) in area_toc.track_isrc.iter().enumerate() {
            if isrc.is_valid() {
                let country = String::from_utf8_lossy(&isrc.country_code);
                let owner = String::from_utf8_lossy(&isrc.owner_code);
                let year = String::from_utf8_lossy(&isrc.recording_year);
                let designation = String::from_utf8_lossy(&isrc.designation_code);

                writeln!(writer, "    ISRC Track [{}]:", i)?;
                writeln!(
                    writer,
                    "      Country: {}, Owner: {}, Year: {}, Designation: {}",
                    country, owner, year, designation
                )?;
            }
        }
        Ok(())
    }

    fn get_total_sectors(&mut self) -> Result<u32> {
        if let Some(sectors) = self.total_sectors {
            Ok(sectors)
        } else {
            let sectors = self.reader.get_total_sectors()?;
            self.total_sectors = Some(sectors);
            Ok(sectors)
        }
    }

    fn write_disc_size<W: Write>(&mut self, writer: &mut W) -> Result<()> {
        if let Ok(total_sectors) = self.get_total_sectors() {
            const SACD_LSN_SIZE: u64 = 2048;
            let total_bytes = total_sectors as u64 * SACD_LSN_SIZE;
            // C code uses 1000^3 for "gigabyte" (not 1024^3)
            let gb = total_bytes as f64 / (1000.0 * 1000.0 * 1000.0);

            writeln!(writer)?;
            writeln!(
                writer,
                "The size of sacd is ok (sectors={}). Size is: {} bytes, {:.3} GB (gigabyte)",
                total_sectors, total_bytes, gb
            )?;
        }
        Ok(())
    }
}

fn read_master_toc<R: SacdReader>(reader: &mut R) -> Result<MasterToc> {
    let res = reader
        .read_data(consts::START_OF_MASTER_TOC, consts::MASTER_TOC_LEN)
        .context("couldn't read master TOC bytes")?;
    MasterToc::from_bytes(&res).context("couldn't parse master TOC bytes")
}

fn read_master_text<R: SacdReader>(reader: &mut R) -> Option<MasterText> {
    // Master text is at sector 511 (START_OF_MASTER_TOC + 1)
    // Read 1 sector (2048 bytes)
    let master_text_sector = consts::START_OF_MASTER_TOC + 1;
    reader
        .read_data(master_text_sector, 1)
        .and_then(|data| MasterText::from_bytes(&data))
        .ok()
}

fn read_stereo_toc<R: SacdReader>(master_toc: &MasterToc, reader: &mut R) -> Option<AreaToc> {
    // Look for stereo TOC 1
    let stereo_toc1 = if master_toc.area_1_toc_1_start > 0 {
        reader
            .read_data(
                master_toc.area_1_toc_1_start,
                master_toc.area_1_toc_size as u32,
            )
            .and_then(|tocdata| AreaToc::from_bytes(&tocdata))
            .ok()
    } else {
        warn!("Couldn't read Stereo TOC 1");
        None
    };

    // see if stereo TOC 2 matches TOC 1
    let stereo_toc2 = if master_toc.area_1_toc_2_start > 0 {
        reader
            .read_data(
                master_toc.area_1_toc_2_start,
                master_toc.area_1_toc_size as u32,
            )
            .and_then(|tocdata| AreaToc::from_bytes(&tocdata))
            .ok()
    } else {
        warn!("Couldn't read Stereo TOC 2");
        None
    };

    match (stereo_toc1, stereo_toc2) {
        (Some(toc1), Some(toc2)) => {
            if toc1 == toc2 {
                // Both exist and are equal - use TOC 1
                Some(toc1)
            } else {
                // By spec, TOC 1 and TOC 2 should be identical/redundant.
                warn!("Stereo TOC 1 and TOC 2 differ, using backup TOC 2");
                Some(toc2)
            }
        }
        (Some(toc1), None) => {
            // Only TOC 1 exists
            Some(toc1)
        }
        (None, Some(toc2)) => {
            // Only TOC 2 exists
            Some(toc2)
        }
        (None, None) => {
            warn!("No stereo TOC found");
            None
        }
    }
}

fn read_mch_toc<R: SacdReader>(master_toc: &MasterToc, reader: &mut R) -> Option<AreaToc> {
    // Look for multichannel TOC 1
    let mch_toc1 = if master_toc.area_2_toc_1_start > 0 {
        reader
            .read_data(
                master_toc.area_2_toc_1_start,
                master_toc.area_2_toc_size as u32,
            )
            .and_then(|tocdata| AreaToc::from_bytes(&tocdata))
            .ok()
    } else {
        warn!("Couldn't read Multichannel TOC 1");
        None
    };

    // see if multichannel TOC 2 matches TOC 1
    let mch_toc2 = if master_toc.area_2_toc_2_start > 0 {
        reader
            .read_data(
                master_toc.area_2_toc_2_start,
                master_toc.area_2_toc_size as u32,
            )
            .and_then(|tocdata| AreaToc::from_bytes(&tocdata))
            .ok()
    } else {
        warn!("Couldn't read Multichannel TOC 2");
        None
    };

    match (mch_toc1, mch_toc2) {
        (Some(toc1), Some(toc2)) => {
            if toc1 == toc2 {
                // Both exist and are equal - use TOC 1
                Some(toc1)
            } else {
                // By spec, TOC 1 and TOC 2 should be identical/redundant.
                warn!("Multichannel TOC 1 and TOC 2 differ, using backup TOC 2");
                Some(toc2)
            }
        }
        (Some(toc1), None) => {
            // Only TOC 1 exists
            Some(toc1)
        }
        (None, Some(toc2)) => {
            // Only TOC 2 exists
            Some(toc2)
        }
        (None, None) => {
            warn!("No multichannel TOC found");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    // #[test]
    // fn it_works() {
    //     // init();
    //     // let handle = open_network_reader(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 130)), 2002).expect("should init");
    // }
}
