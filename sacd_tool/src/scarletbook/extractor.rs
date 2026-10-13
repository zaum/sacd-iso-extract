use dst_decoder::decoder::DstDecoder;
use crate::sacd_reader::SacdReader;
use crate::scarletbook::area_toc::AreaToc;
use crate::scarletbook::audio::AudioSectorParser;
use crate::scarletbook::consts::{DSD64_SAMPLE_RATE, FRAMES_PER_MINUTE, FRAMES_PER_SECOND};
use crate::scarletbook::dsf::{DsfWriter, NopadCarry};
use crate::scarletbook::id3::render_id3;
use crate::scarletbook::master_toc::{MasterText, MasterToc};
use crate::scarletbook::types::FrameFormat;
use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use log::debug;
use rayon::prelude::*;
use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;

/// Producer-thread instrumentation, returned via `JoinHandle::join`.
struct ProducerStats {
    decoded_frames: u64,
    filtered_frames: u64,
    /// Wall-clock spent in `reader.read_data` (sector I/O).
    t_read_ns: u64,
    /// Wall-clock spent in `audio_parser.parse_sector`.
    t_parse_ns: u64,
    /// Wall-clock spent blocked on `tx.send` (channel full -> consumer slow).
    t_send_blocked_ns: u64,
    /// Number of frames sent into the channel.
    send_calls: u64,
}

/// Writer-thread instrumentation, returned alongside the writer.
struct WriterStats {
    /// Wall-clock spent inside `DsfWriter::write_samples`.
    t_write_ns: u64,
    /// Wall-clock spent blocked on `rx.recv` (queue empty -> decoder slow).
    t_recv_blocked_ns: u64,
    /// Number of decoded batches consumed.
    batches: u64,
    /// Number of frames written.
    frames: u64,
}

/// Track extractor for extracting SACD tracks to DSF files
pub struct TrackExtractor<R: SacdReader> {
    reader: R,
    /// Optional sink for non-fatal problems (e.g. a corrupt sector that was
    /// skipped). The CLI wires it up so a GUI can show a live warning row.
    warning: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

impl<R: SacdReader> TrackExtractor<R> {
    /// Create a new track extractor
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            warning: None,
        }
    }

    /// Register a callback invoked for every non-fatal extraction warning.
    pub fn set_warning_handler<F>(&mut self, handler: F)
    where
        F: Fn(&str) + Send + Sync + 'static,
    {
        self.warning = Some(Arc::new(handler));
    }

    /// Extract a single track to a DSF file. The reader must be `Send`
    /// because the producer phase of the streaming pipeline runs on a
    /// separate thread (sectors are read in parallel with decode/write).
    ///
    /// `nopad`: if true, the partial last DSF block is held over instead of
    /// being zero-padded; the held-over bytes are returned as
    /// `Some(NopadCarry)` for the caller to feed into the next consecutive
    /// track. `carry_in`: pre-load the per-channel buffers with a carry
    /// from the previous consecutive track.
    pub fn extract_track(
        &mut self,
        master_toc: &MasterToc,
        master_text: Option<&MasterText>,
        area_toc: &AreaToc,
        track_number: usize,
        output_path: &Path,
        progress_bar: Option<&ProgressBar>,
        nopad: bool,
        carry_in: Option<NopadCarry>,
    ) -> Result<Option<NopadCarry>>
    where
        R: Send,
    {
        // Validate track number
        if track_number < 1 || track_number > area_toc.track_count as usize {
            anyhow::bail!(
                "Invalid track number: {} (valid range: 1-{})",
                track_number,
                area_toc.track_count
            );
        }

        // Get track info
        let track_idx = track_number - 1; // Convert to 0-based
        let track_start_time = area_toc
            .track_times_start
            .get(track_idx)
            .ok_or_else(|| anyhow::anyhow!("Track {} start time not found", track_number))?;
        let track_duration = area_toc
            .track_times_duration
            .get(track_idx)
            .ok_or_else(|| anyhow::anyhow!("Track {} duration not found", track_number))?;

        // Calculate start frame for the track.
        let start_frame = track_start_time.minutes as u32 * FRAMES_PER_MINUTE
            + track_start_time.seconds as u32 * FRAMES_PER_SECOND
            + track_start_time.frames as u32;

        // Get sectors per frame
        let sectors_per_frame = area_toc.frame_format.sectors_per_frame().ok_or_else(|| {
            anyhow::anyhow!(
                "Unsupported or unknown frame format: {:?}",
                area_toc.frame_format
            )
        })?;

        // Prefer explicit per-track LSN ranges from SACDTRL1; fall back to
        // estimating from timecodes only if the disc didn't include them.
        let (start_lsn, end_lsn) = match (
            area_toc.track_start_lsns.get(track_idx).copied(),
            area_toc.track_length_lsns.get(track_idx).copied(),
        ) {
            (Some(start), Some(length)) if start != 0 && length != 0 => (start, start + length),
            _ => {
                let start_lsn = if track_idx == 0 {
                    area_toc.track_start
                } else {
                    area_toc.track_start + (start_frame * sectors_per_frame / 3)
                };
                let end_lsn = if track_idx < area_toc.track_count as usize - 1 {
                    let next = area_toc
                        .track_times_start
                        .get(track_idx + 1)
                        .ok_or_else(|| anyhow::anyhow!("Next track start time not found"))?;
                    let next_fc = next.minutes as u32 * FRAMES_PER_MINUTE
                        + next.seconds as u32 * FRAMES_PER_SECOND
                        + next.frames as u32;
                    area_toc.track_start + (next_fc * sectors_per_frame / 3)
                } else {
                    area_toc.track_end
                };
                (start_lsn, end_lsn)
            }
        };

        let total_sectors = end_lsn - start_lsn;

        log::info!(
            "Track {}: start_lsn={}, end_lsn={}, total_sectors={}, area.track_start={}, area.track_end={}, channels={}",
            track_number,
            start_lsn,
            end_lsn,
            total_sectors,
            area_toc.track_start,
            area_toc.track_end,
            area_toc.channel_count
        );

        debug!(
            "[EXTRACTION] Track {}: channels={}, LSN range: {}-{}, frame_format={:?}",
            track_number, area_toc.channel_count, start_lsn, end_lsn, area_toc.frame_format
        );

        // Drive the progress bar by output frames written.
        // Total expected frames == track duration in 1/75-second frames
        // (already computed as `dur_fc` below).
        if let Some(pb) = progress_bar {
            let total_frames = track_duration.minutes as u64 * FRAMES_PER_MINUTE as u64
                + track_duration.seconds as u64 * FRAMES_PER_SECOND as u64
                + track_duration.frames as u64;
            pb.set_length(total_frames);
            pb.set_message(format!("Track {}", track_number));
        }

        // Total samples per channel for DSF header.
        let duration_seconds = track_duration.minutes as u64 * 60
            + track_duration.seconds as u64
            + track_duration.frames as u64 / FRAMES_PER_SECOND as u64;
        let total_samples_per_channel = duration_seconds * DSD64_SAMPLE_RATE as u64;

        let mut dsf_writer = DsfWriter::create(
            output_path,
            area_toc.channel_count as u32,
            DSD64_SAMPLE_RATE,
            total_samples_per_channel,
            area_toc.extra_setting,
        )
        .with_context(|| format!("Cannot create DSF file {}", output_path.display()))?;

        if nopad {
            dsf_writer.set_nopad(true);
        }
        if let Some(carry) = carry_in {
            dsf_writer.pre_load_carry(carry);
        }

        // Generate ID3v2.3 footer matching the layout sacd-ripper writes.
        let id3 = render_id3(master_toc, master_text, area_toc, track_idx);
        dsf_writer.set_id3_footer(id3);

        // Create audio sector parser; tell it the area's channel count so
        // it can cross-check each frame's per-frame `channel_bits` hint.
        let mut audio_parser = AudioSectorParser::new(area_toc.frame_format)?;
        audio_parser.set_expected_channel_count(area_toc.channel_count);

        // Timecode filter — only frames whose timecode is in
        // [track_start, track_start + track_duration) are decoded. This
        // matches sacd-ripper's default `audio_frame_trimming=1` mode.
        let start_fc = track_start_time.minutes as u32 * FRAMES_PER_MINUTE
            + track_start_time.seconds as u32 * FRAMES_PER_SECOND
            + track_start_time.frames as u32;
        let dur_fc = track_duration.minutes as u32 * FRAMES_PER_MINUTE
            + track_duration.seconds as u32 * FRAMES_PER_SECOND
            + track_duration.frames as u32;
        let end_fc = start_fc + dur_fc;
        // The existing helper takes m/s/f triples, so convert.
        audio_parser.set_timecode_filter(
            (start_fc / FRAMES_PER_MINUTE) as u8,
            ((start_fc / FRAMES_PER_SECOND) % 60) as u8,
            (start_fc % FRAMES_PER_SECOND) as u8,
            (end_fc / FRAMES_PER_MINUTE) as u8,
            ((end_fc / FRAMES_PER_SECOND) % 60) as u8,
            (end_fc % FRAMES_PER_SECOND) as u8,
        );

        // Three-stage streaming pipeline:
        //
        //   producer thread  ->  decoder (main thread)  ->  writer thread
        //          tx_raw                              tx_decoded
        //
        // - producer reads sectors, parses them into raw DST frames, sends
        //   one frame at a time over `tx_raw`.
        // - decoder (main) batches up to BATCH_SIZE raw frames, runs the
        //   parallel rayon decode, and forwards the decoded batch over
        //   `tx_decoded`.
        // - writer owns the `DsfWriter`, drains `tx_decoded` in order, and
        //   calls `write_samples` on each decoded frame.
        //
        // The two channels mean writer-of-batch-N runs concurrently with
        // decoder-of-batch-N+1 and with producer-of-frames-for-batch-N+2.
        // For local-ISO extraction this overlaps the per-byte DSF write
        // path with the next parallel decode.
        //
        // Memory cap: CHANNEL_CAPACITY raw frames + DECODED_QUEUE_DEPTH
        // decoded batches in flight ≈ <60 MB for stereo DSD64.
        const BATCH_SECTORS: u32 = 256;
        const SECTOR: usize = 2048;
        const BATCH_SIZE: usize = 256;
        // ~16k raw DST frames in flight ≈ ~50 MB at typical 3 KB/frame.
        const CHANNEL_CAPACITY: usize = 16384;
        // Small queue between decoder and writer. Each slot holds one
        // decoded batch ≈ BATCH_SIZE * dsd_frame_bytes (~2.4 MB for stereo
        // DSD64). 4 slots = ~10 MB.
        const DECODED_QUEUE_DEPTH: usize = 4;

        let is_dst = matches!(area_toc.frame_format, FrameFormat::Dst);
        let channels = area_toc.channel_count as usize;
        let sample_rate = DSD64_SAMPLE_RATE as usize;
        let track_num = track_number;

        let (tx_raw, rx_raw) = std::sync::mpsc::sync_channel::<Vec<u8>>(CHANNEL_CAPACITY);
        let (tx_decoded, rx_decoded) =
            std::sync::mpsc::sync_channel::<Vec<Vec<u8>>>(DECODED_QUEUE_DEPTH);
        let warning = self.warning.clone();
        let reader: &mut R = &mut self.reader;
        let writer_pb = progress_bar.cloned();
        let dsf_writer_for_thread = dsf_writer;

        let pipeline_start = std::time::Instant::now();

        let carry_out = std::thread::scope(|s| -> Result<Option<NopadCarry>> {
            let producer = s.spawn(move || -> Result<ProducerStats> {
                use std::time::Instant;
                let mut t_read_ns: u64 = 0;
                let mut t_parse_ns: u64 = 0;
                let mut t_send_blocked_ns: u64 = 0;
                let mut send_calls: u64 = 0;

                let mut current_lsn = start_lsn;
                // Tolerate a handful of unreadable sectors instead of
                // throwing the whole track away, but still fail loudly when
                // the damage is widespread (a bad rip would otherwise turn
                // into a track full of silence).
                const MAX_BAD_SECTORS: u32 = 64;
                let mut bad_sectors: u32 = 0;
                let mut first_bad: Option<String> = None;
                'outer: while current_lsn < end_lsn {
                    let want = (end_lsn - current_lsn).min(BATCH_SECTORS);
                    let t_read = Instant::now();
                    let chunk = reader
                        .read_data(current_lsn, want)
                        .with_context(|| format!("read {} sectors @ LSN {}", want, current_lsn))?;
                    t_read_ns += t_read.elapsed().as_nanos() as u64;
                    if chunk.len() < (want as usize) * SECTOR {
                        anyhow::bail!(
                            "Short read at LSN {}: got {} bytes, expected {}",
                            current_lsn,
                            chunk.len(),
                            (want as usize) * SECTOR
                        );
                    }
                    for i in 0..want {
                        let off = (i as usize) * SECTOR;
                        let sector = &chunk[off..off + SECTOR];
                        let t_parse = Instant::now();
                        let frames = match audio_parser.parse_sector(sector) {
                            Ok(frames) => frames,
                            Err(e) => {
                                bad_sectors += 1;
                                let lsn = current_lsn + i as u32;
                                if first_bad.is_none() {
                                    first_bad = Some(format!("LSN {}: {}", lsn, e));
                                }
                                if bad_sectors > MAX_BAD_SECTORS {
                                    anyhow::bail!(
                                        "Too many corrupt audio sectors in this track \
                                         ({} skipped; {})",
                                        bad_sectors,
                                        first_bad.as_deref().unwrap_or("unknown error"),
                                    );
                                }
                                let msg =
                                    format!("Skipped corrupt audio sector at LSN {}: {}", lsn, e);
                                debug!("{}", msg);
                                if let Some(sink) = &warning {
                                    sink(&msg);
                                }
                                continue;
                            }
                        };
                        t_parse_ns += t_parse.elapsed().as_nanos() as u64;
                        for raw in frames {
                            send_calls += 1;
                            let t_send = Instant::now();
                            let send_res = tx_raw.send(raw);
                            t_send_blocked_ns += t_send.elapsed().as_nanos() as u64;
                            if send_res.is_err() {
                                break 'outer;
                            }
                        }
                    }
                    current_lsn += want;
                }
                if let Some(raw) = audio_parser.flush() {
                    let _ = tx_raw.send(raw);
                }
                Ok(ProducerStats {
                    decoded_frames: audio_parser.decoded_frames,
                    filtered_frames: audio_parser.filtered_frames,
                    t_read_ns,
                    t_parse_ns,
                    t_send_blocked_ns,
                    send_calls,
                })
                // tx_raw dropped here -> decoder's rx_raw.recv() returns Err.
            });

            let writer = s.spawn(move || -> Result<(DsfWriter, WriterStats)> {
                use std::time::Instant;
                let mut dsf_writer = dsf_writer_for_thread;
                let mut t_write_ns: u64 = 0;
                let mut t_recv_blocked_ns: u64 = 0;
                let mut batches: u64 = 0;
                let mut frames: u64 = 0;
                loop {
                    let t_recv = Instant::now();
                    let batch = match rx_decoded.recv() {
                        Ok(b) => b,
                        Err(_) => break,
                    };
                    t_recv_blocked_ns += t_recv.elapsed().as_nanos() as u64;

                    let t_w = Instant::now();
                    for f in &batch {
                        dsf_writer.write_samples(f).with_context(|| {
                            format!(
                                "write {} bytes to {}",
                                f.len(),
                                output_path.display()
                            )
                        })?;
                    }
                    t_write_ns += t_w.elapsed().as_nanos() as u64;
                    frames += batch.len() as u64;
                    batches += 1;
                    if let Some(pb) = writer_pb.as_ref() {
                        pb.set_position(frames);
                    }
                    drop(batch);
                }
                Ok((
                    dsf_writer,
                    WriterStats {
                        t_write_ns,
                        t_recv_blocked_ns,
                        batches,
                        frames,
                    },
                ))
            });

            // Per-track rayon pool: workers (and their TLS DstDecoders) are
            // torn down at scope exit so a stereo track's 2-channel decoder
            // can't be reused for a multi-channel track.
            let pool = rayon::ThreadPoolBuilder::new()
                .thread_name(|i| format!("dst-decode-{}", i))
                .build()
                .map_err(|e| anyhow::anyhow!("rayon pool: {}", e))?;

            let mut t_recv_blocked_ns: u64 = 0;
            let mut t_decode_ns: u64 = 0;
            let mut t_send_decoded_blocked_ns: u64 = 0;
            let mut batches_processed: u64 = 0;

            let decoder_res = (|rx_raw: std::sync::mpsc::Receiver<Vec<u8>>,
                                tx_decoded: std::sync::mpsc::SyncSender<Vec<Vec<u8>>>,
                                t_recv_blocked_ns: &mut u64,
                                t_decode_ns: &mut u64,
                                t_send_decoded_blocked_ns: &mut u64,
                                batches_processed: &mut u64|
             -> Result<()> {
                use std::time::Instant;
                thread_local! {
                    static TLS_DECODER: RefCell<Option<DstDecoder>> = const {
                        RefCell::new(None)
                    };
                }
                let mut batch: Vec<Vec<u8>> = Vec::with_capacity(BATCH_SIZE);

                let process = |batch: &mut Vec<Vec<u8>>,
                                   t_decode_ns: &mut u64,
                                   t_send_decoded_blocked_ns: &mut u64,
                                   batches_processed: &mut u64|
                 -> Result<()> {
                    if batch.is_empty() {
                        return Ok(());
                    }
                    let decoded: Vec<Vec<u8>> = if is_dst {
                        let t_dec = Instant::now();
                        let res: Result<Vec<Vec<u8>>> = pool.install(|| {
                            batch
                                .par_iter()
                                .map(|raw| -> Result<Vec<u8>> {
                                    TLS_DECODER.with(|cell| -> Result<Vec<u8>> {
                                        let mut slot = cell.borrow_mut();
                                        if slot.is_none() {
                                            *slot = Some(DstDecoder::new(channels, sample_rate)?);
                                        }
                                        let dec = slot.as_mut().unwrap();
                                        let needed = dec.dsd_frame_bytes();
                                        let mut buf = vec![0u8; needed];
                                        let n = dec.decode_frame(raw, &mut buf)?;
                                        debug_assert_eq!(n, needed);
                                        Ok(buf)
                                    })
                                })
                                .collect()
                        });
                        let d = res?;
                        *t_decode_ns += t_dec.elapsed().as_nanos() as u64;
                        d
                    } else {
                        // Pass-through for uncompressed DSD: the writer
                        // accepts the raw chunks directly.
                        std::mem::take(batch)
                    };
                    batch.clear();

                    let t_send = Instant::now();
                    let send_res = tx_decoded.send(decoded);
                    *t_send_decoded_blocked_ns += t_send.elapsed().as_nanos() as u64;
                    if send_res.is_err() {
                        anyhow::bail!("writer thread dropped its receiver");
                    }
                    *batches_processed += 1;
                    Ok(())
                };

                loop {
                    let t_recv = Instant::now();
                    let raw = match rx_raw.recv() {
                        Ok(r) => r,
                        Err(_) => break,
                    };
                    *t_recv_blocked_ns += t_recv.elapsed().as_nanos() as u64;
                    batch.push(raw);
                    if batch.len() >= BATCH_SIZE {
                        process(
                            &mut batch,
                            t_decode_ns,
                            t_send_decoded_blocked_ns,
                            batches_processed,
                        )?;
                    }
                }
                process(
                    &mut batch,
                    t_decode_ns,
                    t_send_decoded_blocked_ns,
                    batches_processed,
                )?;
                Ok(())
            })(
                rx_raw,
                tx_decoded,
                &mut t_recv_blocked_ns,
                &mut t_decode_ns,
                &mut t_send_decoded_blocked_ns,
                &mut batches_processed,
            );
            // tx_decoded dropped here -> writer's rx_decoded.recv() returns Err.

            let producer_res = producer
                .join()
                .map_err(|_| anyhow::anyhow!("producer thread panicked"))?;
            let writer_join = writer
                .join()
                .map_err(|_| anyhow::anyhow!("writer thread panicked"))?;
            let pipeline_elapsed = pipeline_start.elapsed();

            // Check the producer and writer FIRST: when the writer fails
            // (e.g. disk full), the decoder also fails with a misleading
            // "channel closed" error. The true cause must win.
            let p = producer_res?;
            let (dsf_writer, w) = writer_join?;
            decoder_res?;
            let decoded_n = p.decoded_frames;
            let filtered_n = p.filtered_frames;

            let to_ms = |ns: u64| ns as f64 / 1_000_000.0;
            let pipeline_ns = pipeline_elapsed.as_nanos() as u64;
            log::debug!(
                "[track {} timing] pipeline_wall={:.1}ms\n  \
                 producer: read={:.1}ms parse={:.1}ms send_blocked={:.1}ms (sends={})\n  \
                 decoder:  recv_blocked={:.1}ms decode={:.1}ms send_blocked={:.1}ms (batches={})\n  \
                 writer:   recv_blocked={:.1}ms write={:.1}ms (batches={}, frames={})",
                track_num,
                to_ms(pipeline_ns),
                to_ms(p.t_read_ns),
                to_ms(p.t_parse_ns),
                to_ms(p.t_send_blocked_ns),
                p.send_calls,
                to_ms(t_recv_blocked_ns),
                to_ms(t_decode_ns),
                to_ms(t_send_decoded_blocked_ns),
                batches_processed,
                to_ms(w.t_recv_blocked_ns),
                to_ms(w.t_write_ns),
                w.batches,
                w.frames,
            );
            log::info!(
                "Track {}: yielded {} frames ({} filtered)",
                track_num,
                decoded_n,
                filtered_n,
            );

            // Finalise inside the scope; we own the writer again. In nopad
            // mode the partial tail is returned as a carry for the caller
            // to pass into the next consecutive track.
            let carry = dsf_writer.finalize()?;
            Ok(carry)
        })?;

        if let Some(pb) = progress_bar {
            pb.finish_with_message(format!("Track {} complete", track_number));
        }

        Ok(carry_out)
    }

    /// Extract multiple tracks from an area
    ///
    /// # Arguments
    /// * `area_toc` - Area TOC containing track information
    /// * `track_numbers` - List of track numbers to extract (1-based). If empty, extract all tracks.
    /// * `output_dir` - Output directory for DSF files
    /// * `prefix` - Filename prefix (e.g., "disc_title")
    pub fn extract_tracks(
        &mut self,
        master_toc: &MasterToc,
        master_text: Option<&MasterText>,
        area_toc: &AreaToc,
        track_numbers: &[usize],
        output_dir: &Path,
        prefix: &str,
        nopad: bool,
    ) -> Result<()>
    where
        R: Send,
    {
        // Determine which tracks to extract
        let tracks_to_extract: Vec<usize> = if track_numbers.is_empty() {
            // Extract all tracks
            (1..=area_toc.track_count as usize).collect()
        } else {
            track_numbers.to_vec()
        };

        println!(
            "Extracting {} tracks from {} area...",
            tracks_to_extract.len(),
            if area_toc.channel_count == 2 {
                "stereo"
            } else {
                "multi-channel"
            }
        );

        // Per-track nopad carry: holds the tail (track_idx, carry) of the
        // most recent track that ended without a zero-pad. The next track's
        // index must equal `track_idx + 1` for the carry to apply; on a
        // non-consecutive selection the carry is silently dropped (matches
        // C, where the help text warns "-z cannot be used with -t").
        let mut pending_carry: Option<(usize, NopadCarry)> = None;
        let last_track_idx_in_area = area_toc.track_count.saturating_sub(1) as usize;

        for &track_num in &tracks_to_extract {
            // Get track title if available
            let track_title = area_toc
                .track_texts
                .get(track_num - 1)
                .and_then(|tt| tt.title.as_ref())
                .map(|s| sanitize_filename(s))
                .unwrap_or_else(|| format!("Track_{:02}", track_num));

            // Build output filename
            let filename = format!("{}-{:02}-{}.dsf", prefix, track_num, track_title);
            let output_path = output_dir.join(filename);

            println!("\nExtracting track {}: {}", track_num, track_title);

            // Create progress bar
            let pb = ProgressBar::new(0);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} frames ({percent}%) [{elapsed_precise}]")
                    .unwrap()
                    .progress_chars("#>-"),
            );

            let track_idx = track_num - 1;
            // Apply nopad on every track except the last in the area; on
            // the last track C always pads regardless of the flag, since
            // there's no next track to carry into.
            let apply_nopad = nopad && track_idx < last_track_idx_in_area;
            let carry_in = match pending_carry.take() {
                Some((prev_idx, carry)) if prev_idx + 1 == track_idx => Some(carry),
                _ => None,
            };

            // Extract track
            let carry_out = self.extract_track(
                master_toc,
                master_text,
                area_toc,
                track_num,
                &output_path,
                Some(&pb),
                apply_nopad,
                carry_in,
            )?;

            if let Some(carry) = carry_out {
                pending_carry = Some((track_idx, carry));
            }

            println!("Saved to: {}", output_path.display());
        }

        Ok(())
    }
}

/// Sanitize a filename by removing invalid characters
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

/// Helper function to build a disc prefix for filenames
pub fn build_disc_prefix(disc_title: Option<&str>, disc_artist: Option<&str>) -> String {
    match (disc_title, disc_artist) {
        (Some(title), Some(artist)) => {
            format!("{}-{}", sanitize_filename(artist), sanitize_filename(title))
        }
        (Some(title), None) => sanitize_filename(title),
        (None, Some(artist)) => sanitize_filename(artist),
        (None, None) => "Unknown_Disc".to_string(),
    }
}
