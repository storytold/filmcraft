//! `filmcraft-cli probe`: media info as JSON, plus container details for MPEG TS / PS (program,
//! streams with their codecs, access units, picture types and timestamp ranges), MXF (operational
//! pattern, tracks, index, timecode), Ogg (logical streams, pre-skip, granules), Broadcast WAV
//! (time reference) and image sequences (frames, missing numbers).

use std::sync::Arc;

use serde_json::{Value, json};

fn rate(r: filmcraft_mxf::Rational) -> String {
    format!("{}/{}", r.num, r.den)
}

fn mxf_details(src: &filmcraft_codecs::MxfSource) -> Value {
    let f = src.file();
    let tracks: Vec<Value> = f
        .tracks
        .iter()
        .map(|t| {
            let mut v = json!({
                "trackId": t.track_id,
                "trackNumber": format!("{:08x}", t.track_number),
                "kind": format!("{:?}", t.kind),
                "codec": t.codec.name(),
                "wrapping": format!("{:?}", t.wrapping),
                "editRate": rate(t.edit_rate),
                "materialEditRate": t.material_edit_rate.map(rate),
                "materialDuration": t.material_duration,
                "origin": t.origin,
                "startPosition": t.start_position,
                "duration": t.duration,
            });
            if t.kind == filmcraft_mxf::TrackKind::Picture {
                v["pictures"] = json!(t.samples.len());
                v["keyFrames"] = json!(t.samples.iter().filter(|s| s.key).count());
                v["indexed"] = json!(t.indexed);
                v["temporalOffsets"] = json!(t.temporal_offsets);
                v["reorderedFromBitstream"] = json!(t.needs_reorder);
            }
            if let Some(s) = &t.sound {
                v["sampleRate"] = json!(rate(s.sample_rate));
                v["channels"] = json!(s.channels);
                v["bits"] = json!(s.bits);
                v["sampleFrames"] = json!(t.stored_sample_frames());
            }
            v
        })
        .collect();
    json!({
        "operationalPattern": f.operational_pattern.name(),
        "timecode": f.timecode.map(|t| json!({"start": t.start, "roundedBase": t.rounded_base, "dropFrame": t.drop_frame, "text": t.format()})),
        "materialPackage": f.material_package_name,
        "product": f.product_name,
        "company": f.company_name,
        "partitions": f.partitions.len(),
        "indexSegments": f.index_segments.len(),
        "runIn": f.run_in,
        "tracks": tracks,
        "warnings": f.warnings,
    })
}

fn ogg_details(src: &filmcraft_codecs::OggSource) -> Value {
    let f = src.file();
    let streams: Vec<Value> = f
        .streams
        .iter()
        .map(|s| {
            let mut v = json!({"serial": s.serial, "codec": s.codec.name(), "packets": s.packets.len(), "pages": s.pages, "lastGranule": s.last_granule, "endOfStream": s.saw_eos});
            if s.codec == filmcraft_ogg::Codec::Opus
                && let Some(h) = s.headers.first()
                && h.len() >= 12
            {
                let pre_skip = u16::from_le_bytes([h[10], h[11]]);
                let t = filmcraft_ogg::OpusTiming::of(s, pre_skip as u32);
                v["preSkip"] = json!(pre_skip);
                v["samples48k"] = json!(t.total);
            }
            v
        })
        .collect();
    json!({"streams": streams, "warnings": f.warnings})
}

fn mpeg_details(src: &filmcraft_codecs::MpegSource) -> Value {
    let Some(f) = src.file() else {
        return json!({"format": "MPEG video elementary stream"});
    };
    let (vi, ai) = src.stream_indices();
    let streams: Vec<Value> = f
        .streams
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let id = match s.id {
                filmcraft_mpegts::StreamId::Pid(pid) => json!({"pid": pid}),
                filmcraft_mpegts::StreamId::Ps { stream_id, sub_id } => {
                    json!({"streamId": format!("0x{stream_id:02x}"), "subStreamId": sub_id.map(|x| format!("0x{x:02x}"))})
                }
            };
            let mut v = json!({
                "id": id,
                "streamType": format!("0x{:02x}", s.stream_type),
                "codec": s.codec.name(),
                "kind": format!("{:?}", s.kind()),
                "units": s.units.len(),
                "keyUnits": s.units.iter().filter(|u| u.key).count(),
                "pesPackets": s.pes_packets,
                "ptsRange": s.pts_range().map(|(a, b)| json!([a, b])),
                "used": Some(i) == vi || Some(i) == ai,
            });
            if let Some(l) = &s.language {
                v["language"] = json!(l);
            }
            if let Some(l) = s.lpcm {
                v["lpcm"] = json!({"sampleRate": l.sample_rate, "channels": l.channels, "bits": l.bits});
            }
            if s.units.iter().any(|u| u.picture.is_some()) {
                let p = |t: u8| s.units.iter().filter(|u| u.picture.is_some_and(|p| p.coding_type == t)).count();
                v["pictures"] =
                    json!({"I": p(1), "P": p(2), "B": p(3), "fieldPairs": s.units.iter().filter(|u| u.picture.is_some_and(|p| p.structure != 3)).count()});
            }
            v
        })
        .collect();
    json!({
        "format": f.format.name(),
        "program": f.program,
        "pcrRange": f.pcr_range.map(|(a, b)| json!([a, b])),
        "streams": streams,
        "warnings": f.warnings,
    })
}

/// Probe `path`; with `image_sequence`, `path` is the first frame of a numbered still sequence.
pub fn probe(path: &str, image_sequence: bool) -> Result<Value, String> {
    if image_sequence {
        use filmcraft_media::sequence::{ImageSequenceSource, Numbered, sequence_frames};
        let n = Numbered::parse(path).ok_or_else(|| format!("{path}: not a numbered still image"))?;
        let dir = if n.dir.is_empty() { ".".to_string() } else { n.dir.clone() };
        let names: Vec<String> =
            std::fs::read_dir(&dir).map_err(|e| format!("{dir}: {e}"))?.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        let frames = sequence_frames(&n, &names);
        let name = std::path::Path::new(path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let loader: filmcraft_media::sequence::FrameLoader = Arc::new(|p: &str| std::fs::read(p));
        // the timebase of a fresh install (Settings ▸ Media ▸ Indeterminate Media Timebase)
        let src = ImageSequenceSource::new(&name, frames, filmcraft_time::FrameRate::FPS_29_97, loader).map_err(|e| format!("{path}: {e}"))?;
        let mut v = serde_json::to_value(filmcraft_media::MediaSource::info(&src)).unwrap_or_default();
        v["imageSequence"] = json!({
            "first": src.frame_path(0),
            "firstNumber": n.number,
            "lastNumber": n.number + src.frame_count() as u64 - 1,
            "frames": src.frame_count(),
            "missing": src.missing_frames().iter().map(|i| n.number + *i as u64).collect::<Vec<_>>(),
        });
        return Ok(v);
    }
    let bytes: Arc<[u8]> = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?.into();
    let name = std::path::Path::new(path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    let head = &bytes[..bytes.len().min(70_000)];
    if filmcraft_mxf::sniff(head) {
        let src = filmcraft_codecs::MxfSource::open(&name, bytes).map_err(|e| format!("{path}: {e}"))?;
        let mut v = serde_json::to_value(filmcraft_media::MediaSource::info(&src)).unwrap_or_default();
        v["mxf"] = mxf_details(&src);
        return Ok(v);
    }
    if filmcraft_codecs::mpeg::sniff(head) {
        let src = filmcraft_codecs::MpegSource::open(&name, bytes).map_err(|e| format!("{path}: {e}"))?;
        let mut v = serde_json::to_value(filmcraft_media::MediaSource::info(&src)).unwrap_or_default();
        v["mpeg"] = mpeg_details(&src);
        return Ok(v);
    }
    if filmcraft_ogg::sniff(head)
        && let Ok(src) = filmcraft_codecs::OggSource::open(&name, bytes.clone())
    {
        let mut v = serde_json::to_value(filmcraft_media::MediaSource::info(&src)).unwrap_or_default();
        v["ogg"] = ogg_details(&src);
        return Ok(v);
    }
    let src = filmcraft_codecs::open_bytes(&name, bytes.clone()).map_err(|e| format!("{path}: {e}"))?;
    let mut v = serde_json::to_value(src.info()).unwrap_or_default();
    if filmcraft_media::wav::sniff(head)
        && let Ok(w) = filmcraft_media::wav::WavSource::parse(&name, bytes)
        && let Some(tr) = w.time_reference()
    {
        v["bwf"] = json!({"timeReference": tr});
    }
    Ok(v)
}
