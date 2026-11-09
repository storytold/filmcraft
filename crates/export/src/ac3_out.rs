//! AC-3/E-AC-3 export through the shared standards-based encoder.
//!
//! A raw stream retains encoder priming and final padding, because it has no exact-duration
//! metadata. MP4/MOV uses one 1536-sample access unit containing all dependent substreams.
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use craft_audio::ac3::encoder::{Codec, CodingMode, Config, Encoder};
use filmcraft_isobmff::{CodecConfig, FourCc, SampleEntry};
use filmcraft_project::{ItemId, Project};
use filmcraft_render::SourceProvider;
use filmcraft_time::TimeRange;

use crate::audio_out::AudioOut;
use crate::{AudioCodec, AudioEncoder, ExportError, ExportSettings, Out, Progress, Result};

/// Resolve FilmCraft's supported speaker layouts and validate the concrete rate against the
/// shared encoder. Settings validation and actual encoder construction use this same contract.
pub(crate) fn encoder_config(codec: AudioCodec, rate: u32, channels: u32, bitrate_kbps: u32) -> Result<Config> {
    let codec = match codec {
        AudioCodec::Ac3 => Codec::Ac3,
        AudioCodec::Eac3 => Codec::Eac3,
        _ => return Err(ExportError::Unsupported("expected AC-3 or E-AC-3 audio".into())),
    };
    let (mode, lfe) = match channels {
        1 => (CodingMode::Mono, false),
        2 => (CodingMode::Stereo, false),
        6 => (CodingMode::ThreeFrontTwoSurround, true),
        8 if codec == Codec::Eac3 => (CodingMode::Surround71, true),
        _ => return Err(ExportError::Unsupported("AC-3 supports mono, stereo and 5.1; E-AC-3 also supports discrete 7.1".into())),
    };
    let config = Config { codec, sample_rate: rate, bitrate_kbps, mode, lfe };
    config.validate().map_err(|e| ExportError::Unsupported(e.to_string()))?;
    Ok(config)
}

pub(crate) struct Ac3Audio {
    encoder: Encoder,
    codec: Codec,
    rate: u32,
}

impl Ac3Audio {
    pub(crate) fn new(rate: u32, channels: u32, settings: &ExportSettings) -> Result<Self> {
        let config = encoder_config(settings.audio_codec(), rate, channels, settings.audio.bitrate_kbps)?;
        let codec = config.codec;
        let encoder = Encoder::new(config).map_err(|e| ExportError::Unsupported(e.to_string()))?;
        Ok(Self { encoder, codec, rate })
    }
}

impl AudioEncoder for Ac3Audio {
    fn sample_entry(&self) -> SampleEntry {
        let config = self.encoder.codec_configuration();
        let (fourcc, codec) = match self.codec {
            Codec::Ac3 => (*b"ac-3", CodecConfig::Ac3 { dac3: config }),
            Codec::Eac3 => (*b"ec-3", CodecConfig::Eac3 { dec3: config }),
        };
        // Annex F fixes ChannelCount to 2; dac3/dec3 carries the actual speaker layout.
        SampleEntry::audio(FourCc(fourcc), codec, 2, f64::from(self.rate), 16)
    }

    fn encode(&mut self, planar: &[Vec<f32>]) -> Result<Vec<Vec<u8>>> {
        // A floating-point timeline mix may legitimately exceed full scale. Like integer
        // PCM export, perceptual encoding clips finite overload at the delivery boundary;
        // NaN/Inf remain errors instead of silently contaminating the bitstream.
        if planar.iter().flatten().any(|sample| !sample.is_finite()) {
            return Err(ExportError::Encode("AC-3/E-AC-3 input contains a non-finite sample".into()));
        }
        let clipped: Vec<Vec<f32>> = planar.iter().map(|channel| channel.iter().map(|sample| sample.clamp(-1.0, 1.0)).collect()).collect();
        let refs: Vec<&[f32]> = clipped.iter().map(Vec::as_slice).collect();
        self.encoder.push(&refs).map(|packets| packets.into_iter().map(|p| p.data).collect()).map_err(|e| ExportError::Encode(e.to_string()))
    }

    fn flush(&mut self) -> Result<Vec<Vec<u8>>> {
        self.encoder.finish().map(|tail| tail.packets.into_iter().map(|p| p.data).collect()).map_err(|e| ExportError::Encode(e.to_string()))
    }

    fn priming(&self) -> u32 {
        self.encoder.priming_samples()
    }

    fn frame_size(&self) -> u32 {
        1536
    }
}

pub(crate) fn export(
    project: &Arc<Project>,
    sequence: ItemId,
    settings: &ExportSettings,
    range: TimeRange,
    sources: &dyn SourceProvider,
    progress: &Progress,
) -> Result<(u64, u64)> {
    let _ = filmcraft_media::pending::take();
    let cancelled = || progress.cancel.load(Ordering::Relaxed);
    if cancelled() {
        return Err(ExportError::Cancelled);
    }
    let mut audio = AudioOut::new(project.clone(), sequence, settings, range)?;
    // Construct and validate before opening the destination: unsupported settings must not
    // truncate an existing output file.
    let mut encoder = Ac3Audio::new(audio.sr, audio.channels as u32, settings)?;
    let samples = audio.remaining();
    let chunk = i64::from(audio.sr.max(1));
    if !settings.part_of_batch {
        progress.total.store(samples.div_ceil(chunk as u64).max(1), Ordering::Relaxed);
        progress.set_status(format!("Exporting audio ({})", settings.format.label()));
    }
    audio.measure(settings, sources, &cancelled)?;
    // Raw export is synchronous: deferred source metadata must be retried, not cached as
    // silence or allowed to replace an existing destination with an incomplete programme.
    refuse_pending_media()?;
    *progress.loudness.lock().unwrap_or_else(|e| e.into_inner()) = audio.loudness;
    // Smaller layouts do not need the speaker preflight. Read one bounded chunk before
    // opening output so deferred PCM cannot truncate a destination either.
    let mut next = audio.pull(audio.out.saturating_add(chunk), sources);
    refuse_pending_media()?;
    // Cancellation during validation, measurement or the first read must not truncate output.
    if cancelled() {
        return Err(ExportError::Cancelled);
    }
    let mut output = Out::create(settings)?;
    while let Some(planar) = next {
        if cancelled() {
            return Err(ExportError::Cancelled);
        }
        for unit in encoder.encode(&planar)? {
            output.write_all(&unit).map_err(|e| ExportError::Io(e.to_string()))?;
        }
        if !settings.part_of_batch {
            progress.done.fetch_add(1, Ordering::Relaxed);
        }
        drop(planar);
        next = audio.pull(audio.out.saturating_add(chunk), sources);
        refuse_pending_media()?;
    }
    if cancelled() {
        return Err(ExportError::Cancelled);
    }
    for unit in encoder.flush()? {
        output.write_all(&unit).map_err(|e| ExportError::Io(e.to_string()))?;
    }
    Ok((output.finish(settings)?, samples))
}

/// A synchronous raw export cannot suspend its call stack like a stepped container job.
fn refuse_pending_media() -> Result<()> {
    if filmcraft_media::pending::take() { Err(ExportError::Io("audio media is still loading; retry the export once it is ready".into())) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Format, Multiplexer};
    use filmcraft_frame::{AudioBuffer, VideoFrame};
    use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource};
    use filmcraft_project::{AudioChannels, ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
    use filmcraft_render::SourceMap;
    use filmcraft_time::{FrameRate, Tick};

    const RATE: u32 = 48_000;
    const FREQUENCIES: [f32; 8] = [437.0, 677.0, 919.0, 59.0, 1249.0, 1543.0, 1847.0, 2143.0];
    struct Tones {
        info: MediaInfo,
        known_layout: bool,
    }
    fn value(channel: usize, sample: i64) -> f32 {
        0.2 * (std::f32::consts::TAU * FREQUENCIES[channel] * sample as f32 / RATE as f32).sin()
    }
    impl MediaSource for Tones {
        fn info(&self) -> &MediaInfo {
            &self.info
        }
        fn audio_channel_mask(&self, _: usize) -> Option<u32> {
            (self.known_layout && self.info.audio_streams[0].channels == 8).then_some(filmcraft_media::SPEAKER_MASK_7_1)
        }
        fn video_frame(&self, _: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
            Err(MediaError::NoStream("video"))
        }
        fn audio(&self, start: i64, frames: usize, rate: u32) -> filmcraft_media::Result<AudioBuffer> {
            Ok(AudioBuffer {
                sample_rate: rate,
                channels: (0..self.info.audio_streams[0].channels as usize).map(|c| (0..frames).map(|i| value(c, start + i as i64)).collect()).collect(),
            })
        }
    }
    fn fixture(channels: u32, known_layout: bool) -> (Arc<Project>, ItemId, SourceMap) {
        let mut project = Project::new("AC-3 export routing");
        let info = MediaInfo {
            name: "original tones".into(),
            kind: MediaKind::AudioOnly,
            duration: Tick::from_units(48_000, 48_000),
            video: None,
            audio_streams: vec![AudioStreamInfo { sample_rate: RATE, channels, codec: "test".into(), bits_per_sample: Some(32) }],
            container: "generated".into(),
            start_timecode: None,
            file_size: None,
        };
        let media = project.add_item(
            "tones",
            Label::Iris,
            ItemKind::Media(Box::new(MediaClip {
                media: MediaRef::File { path: "original-tones.wav".into() },
                info: info.clone(),
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            })),
            None,
        );
        let layout = match channels {
            1 => AudioChannels::Mono,
            6 => AudioChannels::Surround51,
            8 => AudioChannels::Surround71,
            _ => AudioChannels::Stereo,
        };
        let sequence = project.new_sequence("mix", SequenceSettings { width: 32, height: 32, audio_master: layout, ..Default::default() }, 1, 1, None);
        let clip = project
            .make_track_item(media, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick::from_units(48_000, 48_000)), FrameRate::FPS_24)
            .unwrap();
        let seq = project.sequence_mut(sequence).unwrap();
        seq.audio_tracks[0].channels = layout;
        seq.audio_tracks[0].items.push(clip);
        let mut sources = SourceMap::default();
        sources.0.insert(media, Arc::new(Tones { info, known_layout }));
        (Arc::new(project), sequence, sources)
    }
    fn settings(format: Format, codec: AudioCodec, channels: u32, name: &str) -> ExportSettings {
        let root = std::env::temp_dir().join(format!("filmcraft-ac3-export-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        ExportSettings {
            format,
            path: root.join(name).to_string_lossy().into_owned(),
            range: Some(TimeRange::new(Tick::ZERO, Tick::from_units(3001, i64::from(RATE)))),
            audio: crate::AudioSettings { codec, channels, sample_rate: Some(RATE), bitrate_kbps: if channels >= 6 { 640 } else { 192 }, ..Default::default() },
            ..Default::default()
        }
    }

    #[test]
    fn pending_layout_is_rechecked_before_any_output_is_opened() {
        struct Loading {
            sources: SourceMap,
            ready: std::sync::atomic::AtomicBool,
        }
        impl filmcraft_render::SourceProvider for Loading {
            fn source(&self, item: ItemId) -> Option<Arc<dyn MediaSource>> {
                if !self.ready.load(Ordering::Relaxed) {
                    filmcraft_media::pending::mark();
                    None
                } else {
                    self.sources.source(item)
                }
            }
        }
        for known_layout in [false, true] {
            let (project, sequence, sources) = fixture(8, known_layout);
            let loading = Loading { sources, ready: std::sync::atomic::AtomicBool::new(false) };
            let settings = settings(Format::H264, AudioCodec::Eac3, 8, &format!("pending-{known_layout}.mp4"));
            let sentinel = b"existing user output";
            std::fs::write(&settings.path, sentinel).unwrap();
            let progress = Progress::default();
            let mut job = crate::Exporter::new(project, sequence, &settings, &progress).unwrap();
            assert!(matches!(job.step(&loading, &progress).unwrap(), crate::Step::Pending));
            assert_eq!(std::fs::read(&settings.path).unwrap(), sentinel);
            loading.ready.store(true, Ordering::Relaxed);
            let result = job.step(&loading, &progress);
            if known_layout {
                assert!(result.is_ok(), "verified layout should resume: {result:?}");
            } else {
                assert!(matches!(result, Err(ExportError::Unsupported(_))), "unknown layout must be checked after loading: {result:?}");
                assert_eq!(std::fs::read(&settings.path).unwrap(), sentinel);
            }
            let _ = filmcraft_media::pending::take();
        }
    }

    #[test]
    fn raw_pending_source_preserves_destination_and_stale_flags_do_not_block_export() {
        struct Loading;
        impl filmcraft_render::SourceProvider for Loading {
            fn source(&self, _: ItemId) -> Option<Arc<dyn MediaSource>> {
                filmcraft_media::pending::mark();
                None
            }
        }
        for channels in [1, 2, 6, 8] {
            let (project, sequence, sources) = fixture(channels, true);
            let settings = settings(Format::Eac3, AudioCodec::Eac3, channels, &format!("raw-pending-{channels}.eac3"));
            let sentinel = b"existing user output";
            std::fs::write(&settings.path, sentinel).unwrap();
            assert!(crate::export(&project, sequence, &settings, &Loading, &Progress::default()).is_err());
            assert_eq!(std::fs::read(&settings.path).unwrap(), sentinel, "preserve destination for {channels} channels");
            filmcraft_media::pending::mark();
            crate::export(&project, sequence, &settings, &sources, &Progress::default()).unwrap();
            assert_ne!(std::fs::read(&settings.path).unwrap(), sentinel, "a previous unrelated pending flag is cleared");
            assert!(!filmcraft_media::pending::is_set());
        }
    }

    #[test]
    fn raw_ac3_family_export_reimports_every_discrete_channel() {
        for (format, codec, count) in [
            (Format::Ac3, AudioCodec::Ac3, 1),
            (Format::Ac3, AudioCodec::Ac3, 2),
            (Format::Ac3, AudioCodec::Ac3, 6),
            (Format::Eac3, AudioCodec::Eac3, 2),
            (Format::Eac3, AudioCodec::Eac3, 6),
            (Format::Eac3, AudioCodec::Eac3, 8),
        ] {
            let (project, sequence, sources) = fixture(count, true);
            let settings = settings(format, codec, count, &format!("raw-{count}.{}", format.extension()));
            crate::export(&project, sequence, &settings, &sources, &Progress::default()).unwrap();
            let bytes: Arc<[u8]> = std::fs::read(&settings.path).unwrap().into();
            let source = filmcraft_codecs::open_bytes(&settings.path, bytes).unwrap();
            assert_eq!(source.info().audio_streams[0].channels, count);
            if count == 8 {
                assert_eq!(source.audio_channel_mask(0), Some(filmcraft_media::SPEAKER_MASK_7_1));
            }
            let decoded = source.audio(256, 2500, RATE).unwrap();
            for (channel, samples) in decoded.channels.iter().enumerate() {
                let correlation = |frequency: usize| samples.iter().enumerate().map(|(i, x)| f64::from(*x * value(frequency, i as i64))).sum::<f64>().abs();
                let own = correlation(channel);
                assert!(own > 5.0, "{format:?} channel {channel} has no matching content: {own}");
                for other in 0..count as usize {
                    if other != channel {
                        assert!(own > 2.0 * correlation(other), "{format:?} channel {channel} swapped with {other}");
                    }
                }
            }
        }
    }

    #[test]
    fn ac3_family_mp4_mov_signal_layout_and_exact_presentation_duration() {
        for (codec, channels) in [(AudioCodec::Ac3, 2), (AudioCodec::Eac3, 8)] {
            for mux in [Multiplexer::Mp4, Multiplexer::Mov] {
                let (project, sequence, sources) = fixture(channels, true);
                let mut settings =
                    settings(Format::H264, codec, channels, &format!("{codec:?}-{mux:?}.{}", if mux == Multiplexer::Mp4 { "mp4" } else { "mov" }));
                settings.multiplexer = mux;
                crate::export(&project, sequence, &settings, &sources, &Progress::default()).unwrap();
                let bytes = std::fs::read(&settings.path).unwrap();
                let file = filmcraft_isobmff::open(bytes.as_slice()).unwrap();
                let audio = file.tracks.iter().find(|t| t.entries.first().is_some_and(|entry| entry.audio.is_some())).unwrap();
                assert_eq!(file.timescale, RATE);
                assert_eq!(audio.edits[0].segment_duration, 3001);
                assert_eq!(audio.edits[0].media_time, 256);
                assert_eq!(audio.entries[0].audio.as_ref().unwrap().channels, 2, "Annex F sample-entry ChannelCount");
                assert!(audio.samples.iter().all(|sample| sample.duration == 1536));
                let source = filmcraft_codecs::open_bytes(&settings.path, bytes.into()).unwrap();
                assert_eq!(source.info().audio_streams[0].channels, channels);
                let tail = source.audio(2990, 100, RATE).unwrap();
                assert!(
                    tail.channels.iter().all(|samples| samples[11..].iter().all(|sample| *sample == 0.0)),
                    "reimport must not play padding beyond the edit-list end"
                );
                let resampled_end = (3001_usize * 44_100).div_ceil(RATE as usize);
                let tail = source.audio(resampled_end as i64 - 10, 100, 44_100).unwrap();
                assert!(tail.channels.iter().all(|samples| samples[10..].iter().all(|sample| *sample == 0.0)), "resampling must not restore edit-list padding");
                assert!(tail.channels[0][9].abs() > 1e-5, "the last sample before a fractional edit end remains audible");
                let decoded = source.audio(0, 2500, RATE).unwrap();
                let error =
                    |shift: i64| decoded.channels[0].iter().enumerate().skip(256).map(|(i, x)| f64::from(*x - value(0, i as i64 + shift)).powi(2)).sum::<f64>();
                assert!(
                    error(0) < error(256) * 0.25 && error(0) < error(-256) * 0.25,
                    "priming applied exactly once: {}, {}, {}",
                    error(0),
                    error(256),
                    error(-256)
                );
            }
        }
    }

    #[test]
    fn ac3_export_rejects_unknown_71_layout_before_creating_output() {
        let (project, sequence, sources) = fixture(8, false);
        let settings = settings(Format::Eac3, AudioCodec::Eac3, 8, "unknown-layout.eac3");
        let error = crate::export(&project, sequence, &settings, &sources, &Progress::default()).unwrap_err();
        assert!(error.to_string().contains("unknown speaker layout"), "{error}");
        assert!(!std::path::Path::new(&settings.path).exists());
    }

    #[test]
    fn reordered_adaptive_channels_need_explicit_71_assignment_for_export() {
        for explicit in [false, true] {
            let (mut project, sequence, sources) = fixture(8, true);
            let selected = vec![1, 0, 2, 3, 4, 5, 6, 7];
            let p = Arc::make_mut(&mut project);
            let clip = &mut p.sequence_mut(sequence).unwrap().audio_tracks[0].items[0];
            clip.source_channels = selected.clone();
            let item = clip.item;
            let ItemKind::Media(media) = &mut p.item_mut(item).unwrap().kind else { panic!("media") };
            media.interpret.audio_channels = Some(filmcraft_project::AudioChannelMap {
                format: if explicit { AudioChannels::Surround71 } else { AudioChannels::Adaptive },
                clips: vec![selected],
            });
            let settings = settings(Format::Eac3, AudioCodec::Eac3, 8, "reordered.eac3");
            let range = TimeRange::new(Tick::ZERO, Tick::from_units(3001, RATE as i64));
            let mut out = AudioOut::new(project, sequence, &settings, range).unwrap();
            let result = out.validate_source_layouts(&sources);
            assert_eq!(result.is_ok(), explicit, "explicit={explicit}: {result:?}");
        }
    }

    #[test]
    fn raw_ac3_rejects_same_count_speaker_layout_changes() {
        let mut stream = Vec::new();
        for (mode, lfe) in [(CodingMode::Stereo, false), (CodingMode::Mono, true)] {
            let mut encoder = Encoder::new(Config { codec: Codec::Ac3, sample_rate: RATE, bitrate_kbps: 192, mode, lfe }).unwrap();
            let pcm = vec![0.0; 1536];
            let packets = encoder.push(&[&pcm, &pcm]).unwrap();
            stream.extend_from_slice(&packets[0].data);
        }
        let result = filmcraft_codecs::open_bytes("changing.ac3", stream.into());
        assert!(matches!(result, Err(error) if error.to_string().contains("speaker layout")), "same count is not the same speaker layout");
    }

    #[test]
    fn raw_ac3_rejects_a_truncated_primary_or_sample_rate_change() {
        let stereo_frame = |sample_rate| {
            let mut encoder = Encoder::new(Config { codec: Codec::Ac3, sample_rate, bitrate_kbps: 192, mode: CodingMode::Stereo, lfe: false }).unwrap();
            let pcm = [0.0; 1536];
            encoder.push(&[pcm.as_slice(); 2]).unwrap().remove(0).data
        };
        let first = stereo_frame(RATE);
        let truncated = [&first[..], &first[..first.len() - 1]].concat();
        let changed_rate = [&first[..], &stereo_frame(32_000)].concat();
        for (bytes, reason) in [(truncated, "truncated AC-3 syncframe"), (changed_rate, "changed programme or sample rate")] {
            let error = filmcraft_codecs::open_bytes("bad.ac3", bytes.into()).err().expect("malformed AC-3 must fail");
            assert!(error.to_string().contains(reason), "expected {reason}, got {error}");
        }
    }

    #[test]
    fn raw_eac3_rejects_a_truncated_or_orphaned_dependent_frame() {
        let mut encoder = Encoder::new(Config { codec: Codec::Eac3, sample_rate: RATE, bitrate_kbps: 640, mode: CodingMode::Surround71, lfe: true }).unwrap();
        let pcm = [0.0; 1536];
        let mut unit = encoder.push(&[pcm.as_slice(); 8]).unwrap().remove(0).data;
        let primary_end = craft_audio::ac3::parse_header(&unit).unwrap().frame_bytes;
        let orphaned = unit[primary_end..].to_vec();
        unit.truncate(unit.len() - 1);
        for (bytes, reason) in [(unit, "truncated E-AC-3 dependent frame"), (orphaned, "changed programme or sample rate")] {
            let error = filmcraft_codecs::open_bytes("bad.eac3", bytes.into()).err().expect("malformed E-AC-3 must fail");
            assert!(error.to_string().contains(reason), "expected {reason}, got {error}");
        }
    }

    #[test]
    fn pre_cancelled_raw_ac3_preserves_existing_destination() {
        let (project, sequence, sources) = fixture(2, true);
        let settings = settings(Format::Ac3, AudioCodec::Ac3, 2, "preserved.ac3");
        let sentinel = b"existing user output";
        std::fs::write(&settings.path, sentinel).unwrap();
        let progress = Progress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        assert!(matches!(crate::export(&project, sequence, &settings, &sources, &progress), Err(ExportError::Cancelled)));
        assert_eq!(std::fs::read(&settings.path).unwrap(), sentinel);
    }

    #[test]
    fn unused_unknown_71_source_does_not_block_export() {
        for mode in [0, 1, 2] {
            let (mut project, sequence, sources) = fixture(8, false);
            let seq = Arc::make_mut(&mut project).sequence_mut(sequence).unwrap();
            match mode {
                0 => seq.audio_tracks[0].items[0].start = Tick::from_units(5, 1),
                1 => seq.audio_tracks[0].muted = true,
                _ => {
                    let mut solo = seq.audio_tracks[0].clone();
                    solo.id = filmcraft_project::TrackId(999);
                    solo.items.clear();
                    solo.solo = true;
                    seq.audio_tracks.push(solo);
                }
            }
            let settings = settings(Format::Eac3, AudioCodec::Eac3, 8, &format!("unused-{mode}.eac3"));
            crate::export(&project, sequence, &settings, &sources, &Progress::default()).unwrap();
        }
    }

    #[test]
    fn muted_prefader_send_still_checks_unknown_71_layout() {
        let (mut project, sequence, sources) = fixture(8, false);
        let seq = Arc::make_mut(&mut project).sequence_mut(sequence).unwrap();
        let mut submix = seq.audio_tracks[0].clone();
        submix.id = filmcraft_project::TrackId(998);
        submix.items.clear();
        seq.submix_tracks.push(submix);
        let track = &mut seq.audio_tracks[0];
        track.muted = true;
        let mut send = filmcraft_project::TrackSend::new(filmcraft_project::TrackId(998));
        send.pre_fader = true;
        track.mixer.sends.push(send);
        let settings = settings(Format::Eac3, AudioCodec::Eac3, 8, "prefader.eac3");
        assert!(crate::export(&project, sequence, &settings, &sources, &Progress::default()).is_err());
    }

    #[test]
    fn nested_71_validation_maps_selected_source_range() {
        let (mut project, nested, sources) = fixture(8, false);
        let p = Arc::make_mut(&mut project);
        p.sequence_mut(nested).unwrap().audio_tracks[0].items[0].start = Tick::from_units(5, 1);
        let mut item = p.sequence(nested).unwrap().audio_tracks[0].items[0].clone();
        item.item = nested;
        item.start = Tick::ZERO;
        item.source_in = Tick::ZERO;
        let outer = p.new_sequence("Outer", SequenceSettings { audio_master: AudioChannels::Surround71, ..Default::default() }, 1, 1, None);
        p.sequence_mut(outer).unwrap().audio_tracks[0].channels = AudioChannels::Surround71;
        p.sequence_mut(outer).unwrap().audio_tracks[0].items.push(item);
        let settings = settings(Format::Eac3, AudioCodec::Eac3, 8, "nested.eac3");
        crate::export(&project, outer, &settings, &sources, &Progress::default()).unwrap();
        Arc::make_mut(&mut project).sequence_mut(outer).unwrap().audio_tracks[0].items[0].source_in = Tick::from_units(5, 1);
        assert!(crate::export(&project, outer, &settings, &sources, &Progress::default()).is_err());
    }

    #[test]
    fn perceptual_boundary_clips_finite_overload_and_rejects_nonfinite() {
        let settings = settings(Format::Ac3, AudioCodec::Ac3, 2, "unused.ac3");
        let mut encoder = Ac3Audio::new(RATE, 2, &settings).unwrap();
        assert!(encoder.encode(&[vec![1.5; 1536], vec![-1.5; 1536]]).is_ok());
        assert!(encoder.encode(&[vec![f32::NAN], vec![0.0]]).is_err());
    }
}
