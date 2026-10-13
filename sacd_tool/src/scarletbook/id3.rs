//! Minimal ID3v2.3 writer matching the layout produced by sacd-ripper's
//! `scarletbook_id3_tag_render`. Only what we need for byte-exact DSF
//! footers.
//!
//! ID3v2.3 layout:
//!   header: "ID3" + version(2) + flags(1) + size(4, syncsafe)  = 10 bytes
//!   frames: id(4) + size(4, BE) + flags(2) + payload
//!
//! Text frames use ISO-8859-1 encoding (encoding byte = 0x00) and a single
//! trailing null. TXXX frames carry `encoding + description\0 + value\0`.

use crate::scarletbook::area_toc::AreaToc;
use crate::scarletbook::master_toc::{MasterText, MasterToc};

fn push_frame(out: &mut Vec<u8>, id: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&[0u8, 0u8]); // flags
    out.extend_from_slice(payload);
}

fn iso8859_bytes(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len());
    for c in s.chars() {
        let n = c as u32;
        v.push(if n <= 0xFF { n as u8 } else { b'?' });
    }
    v
}

fn text_payload(s: &str) -> Vec<u8> {
    let mut p = Vec::with_capacity(2 + s.len());
    p.push(0x00); // ISO-8859-1
    p.extend_from_slice(&iso8859_bytes(s));
    p.push(0x00);
    p
}

fn txxx_payload(description: &str, value: &str) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(0x00); // ISO-8859-1
    p.extend_from_slice(&iso8859_bytes(description));
    p.push(0x00);
    p.extend_from_slice(&iso8859_bytes(value));
    p.push(0x00);
    p
}

fn add_text(out: &mut Vec<u8>, id: &[u8; 4], s: &str) {
    push_frame(out, id, &text_payload(s));
}

fn add_txxx(out: &mut Vec<u8>, description: &str, value: &str) {
    push_frame(out, b"TXXX", &txxx_payload(description, value));
}

fn syncsafe_u32(n: u32) -> [u8; 4] {
    [
        ((n >> 21) & 0x7F) as u8,
        ((n >> 14) & 0x7F) as u8,
        ((n >> 7) & 0x7F) as u8,
        (n & 0x7F) as u8,
    ]
}

fn ascii_trim(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        if b == 0 {
            break;
        }
        s.push(b as char);
    }
    s.trim().to_string()
}

fn ascii_concat(parts: &[&[u8]]) -> String {
    let mut s = String::new();
    for p in parts {
        for &b in *p {
            if b == 0 {
                break;
            }
            s.push(b as char);
        }
    }
    s
}

/// Render an ID3v2.3 tag matching the layout produced by sacd-ripper for the
/// given (master_toc, master_text, area_toc, track_index_zero_based).
pub fn render_id3(
    master_toc: &MasterToc,
    master_text: Option<&MasterText>,
    area_toc: &AreaToc,
    track_idx: usize,
) -> Vec<u8> {
    let track_text = area_toc.track_texts.get(track_idx);
    let mut frames: Vec<u8> = Vec::new();

    // TIT2 — track title
    let title = track_text
        .and_then(|t| t.title.as_deref().or(t.title_phonetic.as_deref()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "no track title".to_string());
    add_text(&mut frames, b"TIT2", &title);

    // TALB — album title
    let album_title = master_text.and_then(|m| m.album_title.as_deref());
    if let Some(album) = album_title {
        add_text(&mut frames, b"TALB", album);
    }
    // Disc title special handling: if disc_title differs from album_title or
    // album_set_size > 1, sacd-ripper emits TXXX DISCSUBTITLE. If only
    // disc_title is present (no album_title), it's used as TALB.
    let disc_title = master_text.and_then(|m| m.disc_title.as_deref());
    match (album_title, disc_title) {
        (Some(at), Some(dt)) if (dt != at || master_toc.album_set_size > 1) => {
            add_txxx(&mut frames, "DISCSUBTITLE", dt);
        }
        (None, Some(dt)) => {
            add_text(&mut frames, b"TALB", dt);
        }
        _ => {}
    }

    // TPE1 — artist (track performer; falls back to disc/album artist).
    let track_perf =
        track_text.and_then(|t| t.performer.as_deref().or(t.performer_phonetic.as_deref()));
    let artist_for_tpe1: Option<&str> = track_perf.or_else(|| {
        master_text.and_then(|m| m.disc_artist.as_deref().or(m.album_artist.as_deref()))
    });
    if let Some(a) = artist_for_tpe1 {
        add_text(&mut frames, b"TPE1", a);
    }

    // TPE2 — album artist (only when present in master text).
    if let Some(album_artist) = master_text.and_then(|m| m.album_artist.as_deref()) {
        // sacd-ripper only writes TPE2 in the non-minimal mode. Default is non-minimal.
        // Place TPE2 after TPE1 (matches reference order when present).
        // Note: reference DSF for Bacewicz has no TPE2 because master_text.album_artist is null.
        add_text(&mut frames, b"TPE2", album_artist);
    }

    // TXXX PERFORMER — duplicate of track performer.
    if let Some(p) = track_perf {
        add_txxx(&mut frames, "PERFORMER", p);
    }

    // TCOM — composer
    if let Some(c) =
        track_text.and_then(|t| t.composer.as_deref().or(t.composer_phonetic.as_deref()))
    {
        add_text(&mut frames, b"TCOM", c);
    }

    // TXXX Catalog Number — from disc catalog number, trimmed.
    let disc_cat = ascii_trim(&master_toc.disc_catalog_number);
    if !disc_cat.is_empty() {
        add_txxx(&mut frames, "Catalog Number", &disc_cat);
    }
    // TXXX Album Catalog Number — only if album catalog differs from disc.
    let album_cat = ascii_trim(&master_toc.album_catalog_number);
    if !album_cat.is_empty() {
        if disc_cat.is_empty() {
            add_txxx(&mut frames, "Catalog Number", &album_cat);
        } else if album_cat != disc_cat {
            add_txxx(&mut frames, "Album Catalog Number", &album_cat);
        }
    }

    // TSRC — ISRC, concatenated as country+owner+year+designation (12 chars).
    if let Some(isrc) = area_toc.track_isrc.get(track_idx)
        && (isrc.country_code[0] != 0
            || isrc.owner_code[0] != 0
            || isrc.recording_year[0] != 0
            || isrc.designation_code[0] != 0)
    {
        let s = ascii_concat(&[
            &isrc.country_code,
            &isrc.owner_code,
            &isrc.recording_year,
            &isrc.designation_code,
        ]);
        add_text(&mut frames, b"TSRC", &s);
    }

    // TPOS — disc number/disc count
    let tpos = format!(
        "{}/{}",
        master_toc.album_sequence_number,
        master_toc.album_set_size.max(1)
    );
    add_text(&mut frames, b"TPOS", &tpos);

    // TCON — genre. Prefer track_genre; fall back to first disc_genre entry
    // with `category == 1` (General Genre Table). Mirrors sacd-ripper's
    // selection logic.
    let genre_entry = area_toc
        .track_genres
        .get(track_idx)
        .filter(|g| g.category == 0x01)
        .copied()
        .or_else(|| {
            master_toc
                .disc_genre
                .iter()
                .find(|g| g.category == 0x01)
                .copied()
        })
        .or_else(|| {
            master_toc
                .album_genre
                .iter()
                .find(|g| g.category == 0x01)
                .copied()
        });
    if let Some(g) = genre_entry {
        let idx = g.genre as usize;
        let names = &crate::scarletbook::consts::GENRE_NAMES;
        if idx > 0 && idx < names.len() {
            add_text(&mut frames, b"TCON", names[idx]);
        }
    }

    // TYER and TDAT — recording year / date.
    add_text(
        &mut frames,
        b"TYER",
        &format!("{:04}", master_toc.disc_date_year),
    );
    add_text(
        &mut frames,
        b"TDAT",
        &format!(
            "{:02}{:02}",
            master_toc.disc_date_day, master_toc.disc_date_month
        ),
    );

    // TRCK — track/total
    let trck = format!("{}/{}", track_idx + 1, area_toc.track_count);
    add_text(&mut frames, b"TRCK", &trck);

    let mut out = Vec::with_capacity(10 + frames.len());
    out.extend_from_slice(b"ID3");
    out.push(0x03); // version major (v2.3)
    out.push(0x00); // version revision
    out.push(0x00); // flags
    out.extend_from_slice(&syncsafe_u32(frames.len() as u32));
    out.extend_from_slice(&frames);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syncsafe_encoding() {
        assert_eq!(syncsafe_u32(0), [0, 0, 0, 0]);
        assert_eq!(syncsafe_u32(127), [0, 0, 0, 127]);
        assert_eq!(syncsafe_u32(128), [0, 0, 1, 0]);
        assert_eq!(syncsafe_u32(323), [0, 0, 2, 67]);
    }

    #[test]
    fn text_payload_iso88591() {
        let p = text_payload("AB");
        assert_eq!(p, vec![0x00, b'A', b'B', 0x00]);
    }

    #[test]
    fn txxx_payload_format() {
        let p = txxx_payload("KEY", "VAL");
        assert_eq!(
            p,
            vec![0x00, b'K', b'E', b'Y', 0x00, b'V', b'A', b'L', 0x00]
        );
    }
}
