//! Conservative native H.264 remux, not a renderer fallback. All eligibility checks
//! complete before reserving a new destination. No third-party codec process is used.
use crate::{ExportError, ExportSettings, ItemId, Progress, Project, Report, Result};

/// Build a stable typed rejection without selecting a render fallback.
pub(crate) fn rejected(code: &'static str, detail: &str) -> ExportError {
    ExportError::CopyIneligible { code, detail: detail.into() }
}

/// Settings-only eligibility; sequence/source compatibility is checked during export.
pub(crate) fn validate_settings(settings: &ExportSettings) -> Result<()> {
    if settings.format != crate::Format::H264 || settings.multiplexer != crate::Multiplexer::Mp4 || settings.include_audio {
        return Err(rejected("format", "copyRequired needs H.264 MP4 with includeAudio=false"));
    }
    if settings.sink.is_some() || settings.output_file.is_some() {
        return Err(rejected("destination", "a new native output path is required"));
    }
    for (field, changed) in [
        ("scale", settings.scale != 1.0),
        ("sdr", settings.sdr),
        ("alpha", settings.alpha),
        ("effects", settings.effects.any_video()),
        ("burnCaptions", settings.burn_captions),
        ("captionSidecar", settings.caption_sidecar.is_some()),
        ("fieldOrder", settings.field_order != crate::FieldOrder::Progressive),
        ("h264IntraOnly", settings.h264_intra_only),
        ("h264Level", settings.h264_level.is_some()),
    ] {
        if changed {
            return Err(rejected("conversion", &format!("{field} requires rendering or conversion")));
        }
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn export(_: &Project, _: ItemId, _: &ExportSettings, _: &dyn filmcraft_render::SourceProvider, _: &Progress) -> Result<Report> {
    Err(rejected("host", "packet copying requires native file access"))
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::{ExportError, ExportSettings, ItemId, Progress, Project, Report, Result, rejected};
    use crate::{Ordering, Out, Tick, export_range};
    use filmcraft_isobmff::{AvcConfig, Brand, CodecConfig, ColorInfo, Mp4File, Mp4Writer, SampleEntry, TrackConfig, TrackKind, WriteSample, WriterOptions};
    use filmcraft_project::{ItemKind, MediaRef, TrackItem};
    use std::{collections::BTreeMap, fs::File, sync::Arc};

    const MAX_SAMPLE_BYTES: u32 = 64 * 1024 * 1024;
    const MAX_FILES: usize = 128;
    const MAX_SOURCE_BYTES: u64 = 1 << 30;
    const MAX_SAMPLES: usize = 1_000_000;

    /// One retained host-approved source, locked shared until the export drops it.
    struct Source {
        file: File,
        movie: Mp4File,
        track: usize,
        metadata: std::fs::Metadata,
    }

    /// Detect ordinary in-place edits while retaining the same approved open handle.
    fn unchanged(input: &Source) -> Result<()> {
        let now = input.file.metadata().map_err(|e| ExportError::Io(e.to_string()))?;
        if now.len() != input.metadata.len() || now.modified().ok() != input.metadata.modified().ok() {
            return Err(rejected("media", "source changed during packet copying"));
        }
        Ok(())
    }

    /// Check access-unit structure, parameter sets and IDR flags without decoding.
    fn packet(data: &[u8], config: &AvcConfig, sync: bool) -> Result<()> {
        let size = usize::from(config.length_size);
        if !matches!(size, 1 | 2 | 4) {
            return Err(rejected("nal", "invalid AVC length size"));
        }
        let mut cursor = 0usize;
        let mut vcl = false;
        let mut idr = false;
        while cursor < data.len() {
            let end = cursor.checked_add(size).ok_or_else(|| rejected("nal", "NAL length overflow"))?;
            let header = data.get(cursor..end).ok_or_else(|| rejected("nal", "truncated NAL length"))?;
            let length = header.iter().fold(0usize, |n, b| (n << 8) | usize::from(*b));
            let stop = end.checked_add(length).ok_or_else(|| rejected("nal", "NAL size overflow"))?;
            let nal = data.get(end..stop).filter(|n| !n.is_empty()).ok_or_else(|| rejected("nal", "invalid NAL extent"))?;
            let first = *nal.first().ok_or_else(|| rejected("nal", "empty NAL"))?;
            if first & 0x80 != 0 {
                return Err(rejected("nal", "forbidden NAL header bit"));
            }
            match first & 31 {
                1 => {
                    if idr {
                        return Err(rejected("nal", "mixed IDR/non-IDR access unit"));
                    }
                    vcl = true;
                }
                5 => {
                    if vcl && !idr {
                        return Err(rejected("nal", "mixed IDR/non-IDR access unit"));
                    }
                    vcl = true;
                    idr = true;
                }
                7 if config.sps.iter().any(|s| s == nal) => {}
                8 if config.pps.iter().any(|p| p == nal) => {}
                6 | 9 | 10 | 11 | 12 => {}
                _ => return Err(rejected("nal", "unsupported NAL or changed in-band parameter set")),
            }
            cursor = stop;
        }
        if !vcl || idr != sync {
            return Err(rejected("random-access", "sample sync flags must agree with IDR access units"));
        }
        Ok(())
    }

    /// Only untouched whole-source clips can preserve compressed samples.
    fn plain(item: &TrackItem) -> Result<()> {
        if !item.enabled
            || item.speed != 1.0
            || item.reverse
            || item.source_in != Tick::ZERO
            || item.frame_hold.is_some()
            || item.scale_to_frame
            || item.multicam.is_some()
            || item.field_options.is_some()
        {
            return Err(rejected("edit", "only enabled, whole-file, 100% forward media is supported"));
        }
        // Intrinsic effects have context-dependent defaults (e.g. anchor point).
        // Refuse modified intrinsics rather than approximate their visual result.
        if item.has_modified_intrinsics() || item.effects.iter().any(|e| e.enabled && e.def().is_none_or(|d| !d.intrinsic)) {
            return Err(rejected("effects", "clip effects, graphics and modified intrinsics require rendering"));
        }
        Ok(())
    }

    /// Preflight all clips, then remux from the same approved open file handles.
    pub(crate) fn run(
        project: &Project,
        seq: ItemId,
        settings: &ExportSettings,
        sources: &dyn filmcraft_render::SourceProvider,
        progress: &Progress,
    ) -> Result<Report> {
        let started = web_time::Instant::now();
        let q = project.sequence(seq).ok_or(ExportError::NoSequence)?;
        let range = export_range(project, seq, settings)?;
        super::validate_settings(settings)?;
        for (field, changed) in [
            ("frameSize", settings.frame_size.is_some_and(|s| s != (q.settings.width, q.settings.height))),
            ("frameRate", settings.frame_rate.is_some_and(|r| r != q.settings.frame_rate)),
            ("pixelAspect", settings.pixel_aspect.is_some_and(|p| p != q.settings.par)),
            ("sequenceColor", q.settings.color != filmcraft_color::ColorPipeline::REC709),
            ("sequencePixelAspect", q.settings.par != (1, 1)),
        ] {
            if changed {
                return Err(rejected("conversion", &format!("{field} differs from the source-preserving sequence")));
            }
        }
        if range.start != Tick::ZERO || range.duration != q.duration() {
            return Err(rejected("range", "only the complete sequence is supported"));
        }
        if q.video_tracks.iter().any(|t| t.solo || t.muted) {
            return Err(rejected("tracks", "solo and muted video tracks are unsupported"));
        }
        let tracks: Vec<_> = q.video_tracks.iter().filter(|t| t.enabled).collect();
        if tracks.iter().any(|t| !t.transitions.is_empty()) || tracks.iter().any(|t| t.solo) {
            return Err(rejected("tracks", "transitions and solo video tracks require rendering"));
        }
        let mut clips: Vec<_> = tracks.iter().flat_map(|t| &t.items).collect();
        clips.sort_by_key(|c| c.start);
        if clips.is_empty() || clips.len() > MAX_SAMPLES {
            return Err(rejected("budget", "empty or oversized sequence"));
        }
        let mut cursor = Tick::ZERO;
        let mut inputs = BTreeMap::new();
        let mut segments = Vec::new();
        let mut reference: Option<(SampleEntry, u32, i64)> = None;
        let mut frames = 0u64;
        let mut sample_budget = MAX_SAMPLES;
        for clip in clips {
            if progress.cancel.load(Ordering::Relaxed) {
                return Err(ExportError::Cancelled);
            }
            plain(clip)?;
            if clip.start != cursor {
                return Err(rejected("timeline", "gaps and overlaps require rendering"));
            }
            let item = project.items.get(&clip.item).ok_or_else(|| rejected("media", "missing project media"))?;
            let ItemKind::Media(media) = &item.kind else {
                return Err(rejected("media", "nested, synthetic and subclip media are unsupported"));
            };
            if media.offline || media.proxy.is_some() || media.interpret != Default::default() {
                return Err(rejected("media", "offline, proxy or interpreted media is unsupported"));
            }
            if media.info.video.as_ref().is_none_or(|v| (v.width, v.height) != (q.settings.width, q.settings.height))
                || media.pixel_aspect() != (1, 1)
                || filmcraft_render::motion_matrix(q, clip, (q.settings.width, q.settings.height), Some((1, 1)), Tick::ZERO) != filmcraft_geom::Affine::IDENTITY
            {
                return Err(rejected("transform", "clip motion must be the identity transform"));
            }
            let MediaRef::File { .. } = &media.media else {
                return Err(rejected("media", "native MP4 files are required"));
            };
            if !inputs.contains_key(&clip.item) {
                if inputs.len() >= MAX_FILES {
                    return Err(rejected("budget", "too many distinct source files"));
                }
                // Native access is an explicit host capability, not inferred from
                // source(), which can legitimately return an offline slate.
                let file = sources
                    .copy_file(clip.item, MAX_SOURCE_BYTES)
                    .ok_or_else(|| rejected("media", "host did not approve a native copy source"))?
                    .map_err(|e| rejected("media", &e.to_string()))?;
                let metadata = file.metadata().map_err(|e| ExportError::Io(e.to_string()))?;
                if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
                    return Err(rejected("budget", "copy source must be a regular file of at most 1 GiB"));
                }
                file.try_lock_shared().map_err(|e| rejected("media", &format!("source shared lock: {e}")))?;
                let movie = filmcraft_isobmff::open(&file).map_err(|e| rejected("container", &e.to_string()))?;
                if movie.timescale == 0 || movie.fragmented {
                    return Err(rejected("container", "fragmented MP4 is unsupported"));
                }
                let videos: Vec<_> = movie.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Video).map(|(i, _)| i).collect();
                let track = *videos.first().filter(|_| videos.len() == 1).ok_or_else(|| rejected("tracks", "one video stream per source is required"))?;
                let t = movie.tracks.get(track).ok_or_else(|| rejected("tracks", "missing video stream"))?;
                if t.entries.len() != 1 || t.matrix != [65536, 0, 0, 0, 65536, 0, 0, 0, 1073741824] || !t.enabled || !t.sample_groups.is_empty() {
                    return Err(rejected("container", "rotation, nontrivial sample descriptions or groups are unsupported"));
                }
                let entry = t.entries.first().ok_or_else(|| rejected("codec", "missing AVC description"))?;
                let CodecConfig::Avc(config) = &entry.codec else {
                    return Err(rejected("codec", "AVC input is required"));
                };
                if entry.data_reference_index != 1 || config.sps.is_empty() || config.pps.is_empty() {
                    return Err(rejected("codec", "self-contained AVC parameter sets and local data reference are required"));
                }
                let video = entry.video.as_ref().ok_or_else(|| rejected("codec", "missing video parameters"))?;
                if entry.format.0 != *b"avc1"
                    || video.clean_aperture.is_some()
                    || video.pixel_aspect.is_some_and(|p| p != (1, 1))
                    || video.field_info.is_some()
                    || video.gamma.is_some()
                    || video.mastering_display.is_some()
                    || video.content_light.is_some()
                    || video.color != Some(ColorInfo::Nclx { primaries: 1, transfer: 1, matrix: 1, full_range: false })
                {
                    return Err(rejected("color", "uncropped progressive square-pixel limited-range BT.709 avc1 is required"));
                }
                if (u32::from(video.width), u32::from(video.height)) != (q.settings.width, q.settings.height)
                    || (t.width, t.height) != (q.settings.width, q.settings.height)
                {
                    return Err(rejected("geometry", "source size differs from the sequence"));
                }
                let rate = q.settings.frame_rate;
                let scaled = i128::from(t.timescale) * i128::from(rate.den);
                if rate.num <= 0 || rate.den <= 0 || t.timescale == 0 || scaled % i128::from(rate.num) != 0 {
                    return Err(rejected("timing", "sample timescale must exactly represent the sequence rate"));
                }
                let step = u32::try_from(scaled / i128::from(rate.num)).ok().filter(|s| *s > 0).ok_or_else(|| rejected("timing", "invalid sample duration"))?;
                if t.samples.is_empty() || !t.samples.first().is_some_and(|s| s.is_sync) {
                    return Err(rejected("random-access", "source must start at an IDR"));
                }
                sample_budget = sample_budget.checked_sub(t.samples.len()).ok_or_else(|| rejected("budget", "source sample tables exceed budget"))?;
                let origin = t.edit_offset.checked_neg().filter(|t| *t >= 0).ok_or_else(|| rejected("timing", "unsupported presentation origin"))?;
                let mut presented = vec![false; t.samples.len()];
                for (i, sample) in t.samples.iter().enumerate() {
                    if progress.cancel.load(Ordering::Relaxed) {
                        return Err(ExportError::Cancelled);
                    }
                    let dts = i128::try_from(i).map_err(|_| rejected("timing", "sample index overflow"))? * i128::from(step);
                    let display = i128::from(sample.pts) - i128::from(origin);
                    if i128::from(sample.dts) != dts
                        || sample.duration != step
                        || sample.description_index != 1
                        || display < 0
                        || display % i128::from(step) != 0
                        || i32::try_from(i128::from(sample.pts) - dts).is_err()
                    {
                        return Err(rejected("timing", "VFR, unsupported composition offsets or nonzero decode origin"));
                    }
                    let index = usize::try_from(display / i128::from(step)).map_err(|_| rejected("timing", "display index overflow"))?;
                    let slot = presented.get_mut(index).ok_or_else(|| rejected("timing", "picture outside the source display interval"))?;
                    if *slot {
                        return Err(rejected("timing", "duplicate presentation timestamp"));
                    }
                    *slot = true;
                    if sample.size == 0 || sample.size > MAX_SAMPLE_BYTES {
                        return Err(rejected("budget", "sample size outside copy budget"));
                    }
                    let data = movie.read_sample(&file, track, i).map_err(|e| rejected("sample", &e.to_string()))?;
                    packet(&data, config, sample.is_sync)?;
                }
                let end = i128::try_from(t.samples.len()).map_err(|_| rejected("timing", "sample count overflow"))? * i128::from(step);
                // A movie-timescale edit duration may be rounded by less than one
                // movie tick. Sample PTS/DTS and the presentation grid stay exact.
                if ![end, end + i128::from(origin)].contains(&i128::from(t.duration))
                    || t.edits.len() > 1
                    || t.edits.iter().any(|e| {
                        e.media_time != origin
                            || e.media_rate != 65536
                            || (i128::from(e.segment_duration) * i128::from(t.timescale) - end * i128::from(movie.timescale)).abs() >= i128::from(t.timescale)
                    })
                {
                    return Err(rejected("edits", "container edits must cover the complete source at normal speed"));
                }
                let mut compatible = entry.clone();
                compatible.bitrate = None; // informational, not coded-sample compatibility
                if reference.as_ref().is_some_and(|r| r != &(compatible.clone(), t.timescale, origin)) {
                    return Err(rejected("codec", "source codec configuration, timescale or presentation origin differs"));
                }
                reference = Some((compatible, t.timescale, origin));
                inputs.insert(clip.item, Arc::new(Source { file, movie, track, metadata }));
            }
            let input = inputs.get(&clip.item).ok_or_else(|| rejected("media", "source was not prepared"))?.clone();
            let t = input.movie.tracks.get(input.track).ok_or_else(|| rejected("tracks", "missing source track"))?;
            let count = i64::try_from(t.samples.len()).map_err(|_| rejected("timing", "sample count overflow"))?;
            if clip.duration != q.settings.frame_rate.tick_of(count) {
                return Err(rejected("trim", "only whole-source clips are supported"));
            }
            cursor = Tick(cursor.0.checked_add(clip.duration.0).ok_or_else(|| rejected("timing", "timeline overflow"))?);
            frames = frames.checked_add(t.samples.len() as u64).ok_or_else(|| rejected("timing", "frame count overflow"))?;
            if frames > MAX_SAMPLES as u64 {
                return Err(rejected("budget", "output sample table exceeds budget"));
            }
            segments.push(input);
        }
        if cursor != range.end() {
            return Err(rejected("timeline", "sequence duration mismatch"));
        }
        let (entry, timescale, origin) = reference.ok_or_else(|| rejected("codec", "no prepared AVC description"))?;
        for input in &segments {
            unchanged(input)?;
        }
        // create_new also refuses aliases/hardlinks of any source: no existing
        // destination is truncated, including when preflight fails.
        let file = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&settings.path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                rejected("destination", "copy destination already exists")
            } else {
                ExportError::Io(format!("{}: {e}", settings.path))
            }
        })?;
        let mut output = settings.clone();
        output.output_file = Some(Arc::new(file));
        let mut options = WriterOptions::new(Brand::Mp4);
        options.metadata = settings.metadata.udta();
        let mut writer = Mp4Writer::new(Out::create(&output)?, options).map_err(|e| ExportError::Io(e.to_string()))?;
        let mut config = TrackConfig::new(entry, timescale);
        config.media_start = (origin > 0).then_some(origin);
        let track = writer.add_track(config).map_err(|e| ExportError::Io(e.to_string()))?;
        if !settings.part_of_batch {
            progress.total.store(frames, Ordering::Relaxed);
            progress.set_status(format!("Copying {frames} compressed video frames (no render/encode)"));
        }
        for input in segments {
            unchanged(&input)?;
            let t = input.movie.tracks.get(input.track).ok_or_else(|| rejected("tracks", "missing prepared track"))?;
            let CodecConfig::Avc(config) = &t.entries.first().ok_or_else(|| rejected("codec", "missing prepared description"))?.codec else {
                return Err(rejected("codec", "missing prepared AVC config"));
            };
            for (i, sample) in t.samples.iter().enumerate() {
                if progress.cancel.load(Ordering::Relaxed) {
                    return Err(ExportError::Cancelled);
                }
                let data = input.movie.read_sample(&input.file, input.track, i).map_err(|e| ExportError::Io(e.to_string()))?;
                packet(&data, config, sample.is_sync)?;
                writer
                    .write_sample(
                        track,
                        WriteSample {
                            data: &data,
                            duration: sample.duration,
                            composition_offset: i32::try_from(i128::from(sample.pts) - i128::from(sample.dts))
                                .map_err(|_| rejected("timing", "composition offset overflow"))?,
                            is_sync: sample.is_sync,
                        },
                    )
                    .map_err(|e| ExportError::Io(e.to_string()))?;
                progress.done.fetch_add(1, Ordering::Relaxed);
            }
            unchanged(&input)?;
        }
        let out = writer.finish_faststart().map_err(|e| ExportError::Io(e.to_string()))?;
        let bytes = out.finish(&output)?;
        let seconds = started.elapsed().as_secs_f64();
        if !settings.part_of_batch {
            progress.set_status(format!("Copied {frames} video frames; 0 rendered"));
            progress.finished.store(true, Ordering::Relaxed);
        }
        Ok(Report {
            copied_video_frames: frames,
            path: settings.path.clone(),
            frames,
            seconds,
            bytes,
            render_fps: frames as f64 / seconds.max(1e-6),
            extra_files: Vec::new(),
        })
    }
    #[cfg(test)]
    mod parser_tests {
        use super::*;
        /// Exercise semantic rejection independently of the mutation/panic check.
        #[test]
        fn access_unit_parameter_sets_and_sync_flags_are_checked() {
            let config = AvcConfig::new(vec![vec![0x67, 100, 0, 40]], vec![vec![0x68, 0]], 4);
            let pack = |nals: &[&[u8]]| -> Vec<u8> { nals.iter().flat_map(|n| (n.len() as u32).to_be_bytes().into_iter().chain(n.iter().copied())).collect() };
            assert!(packet(&pack(&[&config.sps[0], &config.pps[0], &[6, 0], &[0x65, 0]]), &config, true).is_ok());
            for (nals, sync, code) in [
                (vec![vec![0x67, 0], vec![0x65, 0]], true, "nal"),
                (vec![vec![0x68, 1], vec![0x65, 0]], true, "nal"),
                (vec![vec![0x65, 0], vec![0x41, 0]], true, "nal"),
                (vec![vec![0x41, 0], vec![0x65, 0]], true, "nal"),
                (vec![vec![0x74, 0]], true, "nal"),
                (vec![vec![0xe5, 0]], true, "nal"),
                (vec![vec![6, 0]], true, "random-access"),
                (vec![vec![0x65, 0]], false, "random-access"),
            ] {
                let refs: Vec<&[u8]> = nals.iter().map(Vec::as_slice).collect();
                assert!(matches!(packet(&pack(&refs), &config, sync), Err(ExportError::CopyIneligible { code: got, .. }) if got == code));
            }
        }
        #[test]
        fn malformed_avc_lengths_and_random_mutations_never_panic() {
            let config = AvcConfig::new(vec![vec![0x67, 100, 0, 40]], vec![vec![0x68, 0]], 4);
            let valid = vec![0, 0, 0, 2, 0x65, 0x80];
            assert!(packet(&valid, &config, true).is_ok());
            assert!(packet(&valid, &config, false).is_err());
            for end in 0..valid.len() {
                assert!(packet(&valid[..end], &config, true).is_err());
            }
            for bit in 0..valid.len() * 8 {
                let mut changed = valid.clone();
                changed[bit / 8] ^= 1 << (bit % 8);
                assert!(std::panic::catch_unwind(|| packet(&changed, &config, true)).is_ok());
            }
            for length_size in [0, 3, 5, 255] {
                let mut invalid = config.clone();
                invalid.length_size = length_size;
                assert!(packet(&valid, &invalid, true).is_err());
            }
            assert!(packet(&[255, 255, 255, 255], &config, true).is_err());
            assert!(packet(&[0, 0, 0, 1, 0xe5], &config, true).is_err());
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::run as export;
