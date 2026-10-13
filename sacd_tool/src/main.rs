use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::fs;
use std::path::PathBuf;

use sacd_tool::sacd_reader::IsoReader;
use sacd_tool::scarletbook::area_toc::AreaToc;
use sacd_tool::scarletbook::consts::DSD64_SAMPLE_RATE;
use sacd_tool::scarletbook::dsf::NopadCarry;
use sacd_tool::scarletbook::extractor::{build_disc_prefix, TrackExtractor};
use sacd_tool::scarletbook::master_toc::{MasterText, MasterToc};
use sacd_tool::scarletbook::reader as sb_reader;

/// Remove filesystem-invalid characters from a name, matching the reference.
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn format_duration(minutes: u8, seconds: u8) -> String {
    format!("{:02}:{:02}", minutes, seconds)
}

fn duration_frames(minutes: u8, seconds: u8, frames: u8) -> u64 {
    minutes as u64 * 60 * 75 + seconds as u64 * 75 + frames as u64
}

/// Emit a single JSON-line event to stdout for the GUI front-end.
fn emit(event: serde_json::Value) {
    println!("{}", event);
}

/// Set once an `error` event carrying a failure has been emitted. A failing
/// track reports itself (with its track number) inside `extract_area`, so
/// `main()` must not print the very same failure a second time.
static ERROR_REPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[derive(Parser)]
#[command(name = "sacd-tool", about = "SACD ISO to DSF extraction tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print disc metadata and the track list as a single JSON document.
    List {
        /// Path to the SACD ISO file
        #[arg(short, long)]
        iso: PathBuf,
    },
    /// Extract DSF files, emitting JSON-line progress events on stdout.
    Extract {
        /// Path to the SACD ISO file
        #[arg(short, long)]
        iso: PathBuf,
        /// Output directory for the extracted DSF files
        #[arg(short, long)]
        output: PathBuf,
        /// Which area to extract: stereo, mch, or both
        #[arg(long, default_value = "both")]
        layer: String,
        /// Select specific tracks (e.g., "1,2,5" or "1-3,5"); default all
        #[arg(short, long)]
        tracks: Option<String>,
        /// Do not zero-pad DSF (gapless playback across consecutive tracks)
        #[arg(short = 'z', long = "dsf-nopad")]
        nopad: bool,
    },
}

/// Parse track selection string (e.g., "1,2,5" or "1-3,5").
fn parse_track_selection(tracks: &str) -> Result<Vec<usize>> {
    let mut selected = Vec::new();
    for part in tracks.split(',') {
        let part = part.trim();
        if part.contains('-') {
            let range_parts: Vec<&str> = part.split('-').collect();
            if range_parts.len() != 2 {
                anyhow::bail!("Invalid track range: {}", part);
            }
            let start: usize = range_parts[0]
                .parse()
                .with_context(|| format!("Invalid track number: {}", range_parts[0]))?;
            let end: usize = range_parts[1]
                .parse()
                .with_context(|| format!("Invalid track number: {}", range_parts[1]))?;
            if start == 0 || end == 0 {
                anyhow::bail!("Track numbers must be 1 or greater");
            }
            if start > end {
                anyhow::bail!("Invalid range: {} > {}", start, end);
            }
            for track in start..=end {
                if !selected.contains(&track) {
                    selected.push(track);
                }
            }
        } else {
            let track: usize = part
                .parse()
                .with_context(|| format!("Invalid track number: {}", part))?;
            if track == 0 {
                anyhow::bail!("Track numbers must be 1 or greater");
            }
            if !selected.contains(&track) {
                selected.push(track);
            }
        }
    }
    Ok(selected)
}

/// Build the JSON representation of one area (stereo or multichannel).
fn area_to_json(area: &AreaToc) -> serde_json::Value {
    let mut tracks = Vec::new();
    for i in 0..area.track_count as usize {
        let num = i + 1;
        let title = area
            .track_texts
            .get(i)
            .and_then(|tt| tt.title.as_ref())
            .cloned()
            .unwrap_or_else(|| format!("Track {:02}", num));
        let performer = area
            .track_texts
            .get(i)
            .and_then(|tt| tt.performer.as_ref())
            .cloned();
        let (minutes, seconds, frames) = area
            .track_times_duration
            .get(i)
            .map(|d| (d.minutes, d.seconds, d.frames))
            .unwrap_or((0, 0, 0));
        let isrc = area
            .track_isrc
            .get(i)
            .filter(|i| i.is_valid())
            .map(|i| i.to_string());
        tracks.push(json!({
            "number": num,
            "title": title,
            "performer": performer,
            "duration": format_duration(minutes, seconds),
            "duration_frames": duration_frames(minutes, seconds, frames),
            "isrc": isrc,
        }));
    }
    json!({
        "channels": area.channel_count,
        "sample_rate": DSD64_SAMPLE_RATE,
        "track_count": area.track_count,
        "frame_format": format!("{:?}", area.frame_format),
        "tracks": tracks,
    })
}

fn cmd_list(iso: &PathBuf) -> Result<()> {
    if !iso.exists() {
        anyhow::bail!("ISO file not found: {}", iso.display());
    }
    let iso_reader = IsoReader::open(iso).context("Failed to open ISO file")?;
    let sb = sb_reader::new(iso_reader).context("Failed to read SACD metadata from ISO")?;

    let master_text = sb.get_master_text();
    let album = master_text
        .and_then(|m| m.album_title.clone())
        .or_else(|| master_text.and_then(|m| m.disc_title.clone()));
    let artist = master_text
        .and_then(|m| m.album_artist.clone())
        .or_else(|| master_text.and_then(|m| m.disc_artist.clone()));
    let publisher = master_text.and_then(|m| m.album_publisher.clone());
    let copyright = master_text.and_then(|m| m.album_copyright.clone());
    let master_toc = sb.get_master_toc();
    let catalog = {
        let c = master_toc.disc_catalog();
        if c.is_empty() { None } else { Some(c) }
    };
    let year = (master_toc.disc_date_year > 0).then_some(master_toc.disc_date_year);
    let genres: Vec<String> = master_toc
        .disc_genre
        .iter()
        .filter_map(|g| g.genre_name().map(|s| s.to_string()))
        .collect();

    let mut areas = serde_json::Map::new();
    if let Some(stereo) = sb.get_stereo_toc() {
        areas.insert("stereo".to_string(), area_to_json(&stereo));
    }
    if let Some(mch) = sb.get_mch_toc() {
        areas.insert("multichannel".to_string(), area_to_json(&mch));
    }

    let out = json!({
        "iso": iso.display().to_string(),
        "album": album,
        "artist": artist,
        "publisher": publisher,
        "copyright": copyright,
        "catalog": catalog,
        "year": year,
        "genres": genres,
        "hybrid": master_toc.is_hybrid(),
        "areas": serde_json::Value::Object(areas),
    });
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}

/// Extract every selected track of one area, emitting structured progress.
#[allow(clippy::too_many_arguments)]
fn extract_area(
    extractor: &mut TrackExtractor<IsoReader>,
    master_toc: &MasterToc,
    master_text: Option<&MasterText>,
    area_toc: &AreaToc,
    layer: &str,
    output: &PathBuf,
    prefix: &str,
    selected_tracks: &[usize],
    nopad: bool,
) -> Result<()> {
    let tracks_to_extract: Vec<usize> = if selected_tracks.is_empty() {
        (1..=area_toc.track_count as usize).collect()
    } else {
        selected_tracks.to_vec()
    };

    emit(json!({
        "event": "layer_start",
        "layer": layer,
        "channels": area_toc.channel_count,
        "total": tracks_to_extract.len(),
    }));

    let last_track_idx_in_area = area_toc.track_count.saturating_sub(1) as usize;
    let mut pending_carry: Option<(usize, NopadCarry)> = None;

    for &track_num in &tracks_to_extract {
        let track_title = area_toc
            .track_texts
            .get(track_num - 1)
            .and_then(|tt| tt.title.as_ref())
            .map(|s| sanitize_filename(s))
            .unwrap_or_else(|| format!("Track_{:02}", track_num));
        let filename = format!("{}-{:02}-{}.dsf", prefix, track_num, track_title);
        let output_path = output.join(&filename);

        let (minutes, seconds, frames) = area_toc
            .track_times_duration
            .get(track_num - 1)
            .map(|d| (d.minutes, d.seconds, d.frames))
            .unwrap_or((0, 0, 0));

        let track_idx = track_num - 1;
        let apply_nopad = nopad && track_idx < last_track_idx_in_area;
        let carry_in = match pending_carry.take() {
            Some((prev_idx, carry)) if prev_idx + 1 == track_idx => Some(carry),
            _ => None,
        };

        emit(json!({
            "event": "track_start",
            "layer": layer,
            "num": track_num,
            "total": tracks_to_extract.len(),
            "title": track_title,
            "duration": format_duration(minutes, seconds),
            "duration_frames": duration_frames(minutes, seconds, frames),
            "path": output_path.display().to_string(),
        }));

        let carry_out = extractor.extract_track(
            master_toc,
            master_text,
            area_toc,
            track_num,
            &output_path,
            None,
            apply_nopad,
            carry_in,
        );

        match carry_out {
            Ok(carry) => {
                if let Some(carry) = carry {
                    pending_carry = Some((track_idx, carry));
                }
                let bytes = fs::metadata(&output_path).map(|m| m.len()).unwrap_or(0);
                emit(json!({
                    "event": "track_done",
                    "layer": layer,
                    "num": track_num,
                    "path": output_path.display().to_string(),
                    "bytes": bytes,
                }));
            }
            Err(e) => {
                // Remove partial output so a stub file never masquerades
                // as a finished track.
                let _ = fs::remove_file(&output_path);
                ERROR_REPORTED.store(true, std::sync::atomic::Ordering::Relaxed);
                emit(json!({
                    "event": "error",
                    "layer": layer,
                    "num": track_num,
                    "message": friendly_error(&e),
                }));
                return Err(e);
            }
        }
    }

    emit(json!({"event": "layer_done", "layer": layer}));
    Ok(())
}

/// Render an anyhow error as the full cause chain plus a plain-language
/// hint for common OS failures (e.g. a full drive), so the UI can show
/// something a human understands instead of an exit code or a bare message.
fn friendly_error(e: &anyhow::Error) -> String {
    let chain = format!("{:#}", e);
    match io_hint(e) {
        Some(hint) => format!("{}\n\n{}", chain, hint),
        None => chain,
    }
}

/// Detect well-known OS failures in an error chain and explain them.
fn io_hint(e: &anyhow::Error) -> Option<&'static str> {
    for cause in e.chain() {
        if let Some(ioe) = cause.downcast_ref::<std::io::Error>() {
            match ioe.kind() {
                std::io::ErrorKind::StorageFull => {
                    return Some(
                        "The destination drive is full. Free up space on it \
                         and extract again.",
                    )
                }
                std::io::ErrorKind::PermissionDenied => {
                    return Some(
                        "Access denied while writing. Check the output \
                         folder permissions.",
                    )
                }
                std::io::ErrorKind::QuotaExceeded => {
                    return Some(
                        "Disk quota exceeded on the destination drive.",
                    )
                }
                _ => {
                    // ENOSPC can surface as a raw code on some platforms.
                    if ioe.raw_os_error() == Some(28) {
                        return Some(
                            "The destination drive is full. Free up space \
                             on it and extract again.",
                        );
                    }
                }
            }
        }
    }
    None
}

fn cmd_extract(
    iso: &PathBuf,
    output: &PathBuf,
    layer: &str,
    tracks: Option<String>,
    nopad: bool,
) -> Result<()> {
    if !iso.exists() {
        anyhow::bail!("ISO file not found: {}", iso.display());
    }
    let extract_stereo = layer == "both" || layer == "stereo";
    let extract_mch = layer == "both" || layer == "mch";
    let selected_tracks = tracks
        .map(|t| parse_track_selection(&t))
        .transpose()?
        .unwrap_or_default();

    let iso_reader = IsoReader::open(iso).context("Failed to open ISO file")?;
    let sb = sb_reader::new(iso_reader).context("Failed to read SACD metadata from ISO")?;

    let master_text = sb.get_master_text();
    let album = master_text.and_then(|m| m.album_title.clone());
    let artist = master_text.and_then(|m| m.album_artist.clone());
    emit(json!({"event": "album", "album": album, "artist": artist}));

    let disc_title = master_text.and_then(|m| m.disc_title.clone());
    let disc_artist = master_text.and_then(|m| m.disc_artist.clone());
    let disc_prefix = build_disc_prefix(disc_title.as_deref(), disc_artist.as_deref());

    let stereo_toc = sb.get_stereo_toc();
    let mch_toc = sb.get_mch_toc();
    let master_toc = sb.get_master_toc();
    let master_text_owned = master_text.cloned();

    if !output.exists() {
        fs::create_dir_all(output).context("Failed to create output directory")?;
    }

    let reader = sb.into_reader();
    let mut extractor = TrackExtractor::new(reader);
    // Non-fatal problems (a corrupt sector that was skipped) reach the GUI
    // as their own event type so the UI can show them in amber.
    extractor.set_warning_handler(|msg| {
        emit(json!({"event": "warning", "message": msg}))
    });

    if extract_stereo {
        match &stereo_toc {
            Some(toc) => extract_area(
                &mut extractor,
                &master_toc,
                master_text_owned.as_ref(),
                toc,
                "stereo",
                output,
                &format!("{}-stereo", disc_prefix),
                &selected_tracks,
                nopad,
            )?,
            None => emit(json!({"event": "info", "message": "No stereo tracks found on disc"})),
        }
    }

    if extract_mch {
        match &mch_toc {
            Some(toc) => extract_area(
                &mut extractor,
                &master_toc,
                master_text_owned.as_ref(),
                toc,
                "mch",
                output,
                &format!("{}-mch", disc_prefix),
                &selected_tracks,
                nopad,
            )?,
            None => emit(json!({"event": "info", "message": "No multi-channel tracks found on disc"})),
        }
    }

    emit(json!({"event": "all_done"}));
    Ok(())
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("error")).init();
    let cli = Cli::parse();
    match cli.command {
        Commands::List { iso } => cmd_list(&iso),
        Commands::Extract {
            iso,
            output,
            layer,
            tracks,
            nopad,
        } => {
            if let Err(e) = cmd_extract(&iso, &output, &layer, tracks, nopad) {
                // Skip the second copy when the failure was already reported
                // with its track number (otherwise the GUI shows it twice).
                if !ERROR_REPORTED.load(std::sync::atomic::Ordering::Relaxed) {
                    emit(json!({"event": "error", "message": friendly_error(&e)}));
                }
                std::process::exit(1);
            }
            Ok(())
        }
    }
}



