//! Oracle tests: compare our demuxer's sample tables and codec parameters with ffprobe.

use crate::common;

use common::*;
use filmcraft_isobmff::{CodecConfig, Mp4File, TrackKind, open};
use serde_json::Value;

fn pcm_name_matches(ff: &str, c: &CodecConfig) -> bool {
    let CodecConfig::Pcm(p) = c else { return false };
    let Some(rest) = ff.strip_prefix("pcm_") else { return false };
    let kind = rest.as_bytes()[0];
    let digits: String = rest[1..].chars().take_while(|c| c.is_ascii_digit()).collect();
    let bits: u16 = digits.parse().unwrap_or(0);
    let endian = &rest[1 + digits.len()..];
    let kind_ok = match kind {
        b'f' => p.float,
        b's' => p.signed && !p.float,
        b'u' => !p.signed && !p.float,
        _ => false,
    };
    let endian_ok = match endian {
        "le" => !p.big_endian,
        "be" => p.big_endian,
        _ => true, // 8-bit has no endianness
    };
    kind_ok && endian_ok && bits == p.bits
}

fn check_stream(file: &Mp4File, idx: usize, st: &Value) -> Result<(), String> {
    let t = &file.tracks[idx];
    let codec = t.codec().ok_or("no sample entry")?;
    let ffname = st["codec_name"].as_str().unwrap_or("");
    let tag = st["codec_tag_string"].as_str().unwrap_or("");
    let entry = &t.entries[0];
    if tag != entry.format.to_string() {
        return Err(format!("tag {tag} vs {}", entry.format));
    }
    let name_ok = match codec {
        CodecConfig::Pcm(_) => pcm_name_matches(ffname, codec),
        CodecConfig::Timecode(_) => st["codec_type"] == "data",
        c => c.name() == ffname,
    };
    if !name_ok {
        return Err(format!("codec {ffname} vs {codec:?}"));
    }
    let tb = format!("1/{}", t.timescale);
    if st["time_base"].as_str() != Some(tb.as_str()) {
        return Err(format!("time_base {} vs {tb}", st["time_base"]));
    }
    match t.kind {
        TrackKind::Video => {
            let v = t.video().ok_or("no video params")?;
            if int(&st["width"]) != Some(v.width as i64) || int(&st["height"]) != Some(v.height as i64) {
                return Err(format!("size {}x{} vs {}x{}", st["width"], st["height"], v.width, v.height));
            }
            if let Some(n) = int(&st["nb_frames"])
                && n != t.samples.len() as i64
            {
                return Err(format!("nb_frames {n} vs {}", t.samples.len()));
            }
            if let CodecConfig::Avc(a) = codec {
                if int(&st["level"]) != Some(a.level as i64) {
                    return Err(format!("level {} vs {}", st["level"], a.level));
                }
                if st["nal_length_size"].as_str() != Some(&a.length_size.to_string()) {
                    return Err("nal_length_size".into());
                }
            }
            if let Some(cp) = st["color_primaries"].as_str()
                && cp == "bt709"
            {
                match &v.color {
                    Some(filmcraft_isobmff::ColorInfo::Nclx { primaries: 1, .. }) | Some(filmcraft_isobmff::ColorInfo::Nclc { primaries: 1, .. }) => {}
                    other => return Err(format!("colr {other:?}")),
                }
            }
            if let Some(sar) = st["sample_aspect_ratio"].as_str()
                && sar != "1:1"
                && sar != "0:1"
            {
                let (h, vv) = v.pixel_aspect.ok_or("no pasp")?;
                let want = format!("{}:{}", h / gcd(h, vv), vv / gcd(h, vv));
                if sar != want {
                    return Err(format!("sar {sar} vs {want}"));
                }
            }
        }
        TrackKind::Audio => {
            let a = t.audio().ok_or("no audio params")?;
            let sr = int(&st["sample_rate"]).unwrap_or(0);
            let ch = int(&st["channels"]).unwrap_or(0);
            let (our_sr, our_ch) = match codec {
                CodecConfig::Pcm(p) => (p.sample_rate, p.channels),
                CodecConfig::Flac(f) => (f.sample_rate as f64, f.channels as u32),
                CodecConfig::Opus(o) => (48000.0, o.output_channels as u32),
                _ => (a.sample_rate, a.channels),
            };
            if sr != our_sr.round() as i64 || ch != our_ch as i64 {
                return Err(format!("audio {sr}/{ch} vs {our_sr}/{our_ch}"));
            }
            if let CodecConfig::Aac(c) = codec
                && c.object_type != 2
            {
                return Err(format!("aac object type {}", c.object_type));
            }
        }
        TrackKind::Timecode => {
            let CodecConfig::Timecode(tc) = codec else { return Err("tmcd codec".into()) };
            let want = st["tags"]["timecode"].as_str().ok_or("no timecode tag")?;
            let got = tc.format_frame(tc.start_frame.ok_or("no start frame")?);
            if got != want {
                return Err(format!("timecode {got} vs {want}"));
            }
        }
        _ => {}
    }
    Ok(())
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

/// `strict`: ffprobe timestamps must equal ours + edit offset. Otherwise (edit starting mid-frame,
/// which ffmpeg snaps to a frame boundary) only a constant shift within one frame is required.
fn check_packets(file: &Mp4File, idx: usize, pk: &[&Value], strict: bool) -> Result<(), String> {
    let t = &file.tracks[idx];
    if t.pcm_chunked {
        // ffmpeg re-packetizes raw PCM; compare totals.
        let ours: u64 = t.samples.iter().map(|s| s.size as u64).sum();
        let theirs: i64 = pk.iter().filter_map(|p| int(&p["size"])).sum();
        let od: u64 = t.samples.iter().map(|s| s.duration as u64).sum();
        let td: i64 = pk.iter().filter_map(|p| int(&p["duration"])).sum();
        if ours as i64 != theirs || od as i64 != td {
            return Err(format!("pcm totals: bytes {ours} vs {theirs}, dur {od} vs {td}"));
        }
        return Ok(());
    }
    if pk.len() != t.samples.len() {
        return Err(format!("packet count {} vs {}", pk.len(), t.samples.len()));
    }
    let e = if strict || pk.is_empty() {
        t.edit_offset
    } else {
        let e = int(&pk[0]["pts"]).unwrap_or(0) - t.samples[0].pts;
        let max_dur = t.samples.iter().map(|s| s.duration as i64).max().unwrap_or(0);
        if (e - t.edit_offset).abs() >= max_dur {
            return Err(format!("shift {e} vs edit offset {}", t.edit_offset));
        }
        e
    };
    let last = pk.len() - 1;
    for (i, (p, s)) in pk.iter().zip(&t.samples).enumerate() {
        let size = int(&p["size"]);
        let pos = int(&p["pos"]);
        let pts = int(&p["pts"]);
        let dts = int(&p["dts"]);
        let key = p["flags"].as_str().unwrap_or("").starts_with('K');
        // ffmpeg extends the last packet's duration when it carries discard padding.
        let ok = size == Some(s.size as i64)
            && pos == Some(s.offset as i64)
            && pts == Some(s.pts + e)
            && dts == Some(s.dts + e)
            && key == s.is_sync
            && (i == last || int(&p["duration"]).is_none_or(|d| d == s.duration as i64));
        if !ok {
            return Err(format!("packet {i}: ffprobe {p} vs ours {s:?} (edit_offset {e})"));
        }
    }
    Ok(())
}

fn check_fixture(name: &str) {
    let Some(path) = fixture(name) else {
        eprintln!("skipping {name}: ffmpeg or encoder unavailable");
        return;
    };
    if ffprobe().is_none() {
        return;
    }
    let f = std::fs::File::open(&path).unwrap();
    let file = open(&f).unwrap_or_else(|e| panic!("{name}: open failed: {e}"));
    let streams = streams(&path);
    assert_eq!(streams.len(), file.tracks.len(), "{name}: stream count");
    let packets = packets(&path);
    let mut errors = Vec::new();
    for (i, st) in streams.iter().enumerate() {
        if let Err(e) = check_stream(&file, i, st) {
            errors.push(format!("stream {i}: {e}"));
        }
        if matches!(file.tracks[i].kind, TrackKind::Video | TrackKind::Audio) {
            let pk: Vec<&Value> = packets.iter().filter(|p| int(&p["stream_index"]) == Some(i as i64)).collect();
            if let Err(e) = check_packets(&file, i, &pk, name != "editlist_cut.mp4") {
                errors.push(format!("stream {i}: {e}"));
            }
        }
    }
    // Every sample must be readable.
    for (ti, t) in file.tracks.iter().enumerate() {
        for si in [0, t.samples.len() / 2, t.samples.len().saturating_sub(1)] {
            if si < t.samples.len() {
                let d = file.read_sample(&f, ti, si).unwrap();
                assert_eq!(d.len(), t.samples[si].size as usize);
            }
        }
    }
    assert!(errors.is_empty(), "{name}:\n{}", errors.join("\n"));
    eprintln!("{name}: ok ({} tracks)", file.tracks.len());
}

macro_rules! fixture_tests {
    ($($fn:ident => $name:expr),* $(,)?) => {
        $(#[test] fn $fn() { check_fixture($name); })*
    };
}

fixture_tests! {
    h264_bframes_mp4 => "h264_bframes.mp4",
    h264_mov => "h264.mov",
    hevc_mp4 => "hevc.mp4",
    prores_mov => "prores.mov",
    prores4444_mov => "prores4444.mov",
    mjpeg_mov => "mjpeg.mov",
    dnxhd_mov => "dnxhd.mov",
    pcm_f32_mov => "pcm_f32.mov",
    pcm_s16be_mov => "pcm_s16be.mov",
    aac_mp4 => "aac.mp4",
    aac_m4a => "aac_stereo44.m4a",
    frag_mp4 => "frag.mp4",
    frag_dash_mp4 => "frag_dash.mp4",
    faststart_mp4 => "faststart.mp4",
    tmcd_2997df_mov => "tmcd_2997df.mov",
    tmcd_23976_mov => "tmcd_23976.mov",
    h264_2997_mp4 => "h264_2997.mp4",
    flac_mp4 => "flac.mp4",
    alac_m4a => "alac.m4a",
    opus_mp4 => "opus.mp4",
    ac3_mp4 => "ac3.mp4",
    vp9_mp4 => "vp9.mp4",
    av1_mp4 => "av1.mp4",
    colr_pasp_mov => "colr_pasp.mov",
    editlist_cut_mp4 => "editlist_cut.mp4",
}

#[test]
fn specific_properties() {
    let Some(p) = fixture("h264_bframes.mp4") else { return };
    let data = std::fs::read(&p).unwrap();
    let f = open(data.as_slice()).unwrap();
    assert!(!f.is_quicktime);
    let v = &f.tracks[f.track_of_kind(TrackKind::Video).unwrap()];
    // B-frames → non-trivial composition offsets.
    assert!(v.samples.iter().any(|s| s.pts != s.dts));
    assert!(v.samples.iter().filter(|s| s.is_sync).count() >= 4);
    let i = v.sample_at_pts(v.samples[7].pts).unwrap();
    assert_eq!(i, 7);
    assert_eq!(v.sync_sample_before(13), 12);
    let a = &f.tracks[f.track_of_kind(TrackKind::Audio).unwrap()];
    assert_eq!(a.edits.len(), 1);
    assert_eq!(a.edit_offset, -1024);

    let Some(p) = fixture("tmcd_2997df.mov") else { return };
    let f = open(std::fs::read(&p).unwrap()).unwrap();
    assert!(f.is_quicktime);
    let t = &f.tracks[f.track_of_kind(TrackKind::Timecode).unwrap()];
    let CodecConfig::Timecode(tc) = t.codec().unwrap() else { panic!() };
    assert!(tc.drop_frame());
    assert_eq!(tc.frames_per_second, 30);
    assert_eq!(tc.start_frame, Some(107892));
    let v = &f.tracks[f.track_of_kind(TrackKind::Video).unwrap()];
    assert!(v.references.iter().any(|(k, ids)| k.to_string() == "tmcd" && ids == &vec![t.id]));
}

/// Pre-generate every fixture (`cargo xtask fixtures`).
#[test]
#[ignore]
fn generate_fixtures() {
    for name in all_fixture_names() {
        let out = [fixture_dir().join(name)];
        filmcraft_testkit::fixtures::generate_and_report(&format!("isobmff/{name}"), &out, || fixture(name));
    }
}
