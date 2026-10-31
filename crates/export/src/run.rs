use super::format::{ExportSettings, Format};
use super::output::{AudioWriter, Out, SharedBuf, write_output};
use super::still_encode::encode_still_with;
use super::{
    Arc, ExportError, Exporter, FrameRate, IntoParallelIterator, ItemId, Ordering, ParallelIterator, PoisonError, Progress, Project, Report, Result,
    SourceProvider, Step, Tick, TimeRange, Write, aac_out, audio_out, pcm, pipeline, stepped,
};

/// The range to export (settings → In/Out → whole sequence).
pub fn export_range(project: &Project, seq: ItemId, settings: &ExportSettings) -> Result<TimeRange> {
    let q = project.sequence(seq).ok_or(ExportError::NoSequence)?;
    q.check_bounds().map_err(ExportError::Unsupported)?;
    if let Some(r) = settings.range {
        validate_range(r)?;
        return Ok(r);
    }
    let fd = q.settings.frame_rate.frame_duration();
    let a = q.mark_in.unwrap_or(Tick::ZERO);
    let minimum_end = a.0.checked_add(fd.0).ok_or_else(|| ExportError::Unsupported("export In point overflows the time range".into()))?;
    let b = match q.mark_out {
        Some(out) => out.0.checked_add(fd.0).ok_or_else(|| ExportError::Unsupported("export Out point overflows the time range".into()))?,
        None => q.duration().0,
    };
    let duration = b.max(minimum_end).checked_sub(a.0).ok_or_else(|| ExportError::Unsupported("export range duration overflows".into()))?;
    let range = TimeRange::new(a, Tick(duration));
    validate_range(range)?;
    Ok(range)
}

/// Validate input time before frame/sample conversion or any `TimeRange::end` arithmetic.
pub fn validate_range(range: TimeRange) -> Result<()> {
    if range.start.0 < 0 || range.duration.0 <= 0 || range.start.0.checked_add(range.duration.0).is_none_or(|end| end > Tick::MAX.0) {
        return Err(ExportError::Unsupported("export range must start at or after zero, have positive duration and end within supported time bounds".into()));
    }
    Ok(())
}

/// Output frames `[f0, f1)` of `range` at `rate` (frame `f` is at `rate.tick_of(f)`).
pub fn frame_span(rate: FrameRate, range: TimeRange) -> (i64, i64) {
    let f0 = rate.frame_at(range.start);
    let f1 = rate.frame_at(range.end() - Tick(1)) + 1;
    (f0, f1.max(f0))
}

/// The delay of the `i`th GIF frame (from the start of the export) in centiseconds, the unit GIF
/// stores. Each frame runs from its rounded start time to the next frame's, so the delays add up
/// to the exact duration (24 fps: 4, 4, 5, 4, 4, 4 cs …) instead of every frame truncating
/// 4.17 cs to 4 cs and the animation playing 4 % fast (#854).
pub(crate) fn gif_delay_cs(rate: FrameRate, i: i64) -> u16 {
    let num = i128::from(rate.num.max(1));
    let den = i128::from(rate.den.max(0));
    // start of frame `k` in centiseconds, rounded half up
    let at = |k: i64| i128::from(k.max(0)).saturating_mul(200).saturating_mul(den).saturating_add(num) / num.saturating_mul(2);
    let cs = at(i.saturating_add(1)).saturating_sub(at(i));
    u16::try_from(cs.clamp(0, i128::from(u16::MAX))).unwrap_or(u16::MAX)
}

/// A pool of `max_threads` threads to run an export on, or `None` when it should run where it is:
/// no cap (0), a cap that is not below the pool the caller is in already, or no threads to spare
/// (the browser).
fn capped_pool(max_threads: u32) -> Option<rayon::ThreadPool> {
    let cap = max_threads as usize;
    if cap == 0 || cap >= rayon::current_num_threads() || cfg!(target_arch = "wasm32") {
        return None;
    }
    rayon::ThreadPoolBuilder::new().num_threads(cap).thread_name(|i| format!("filmcraft-export-{i}")).build().ok()
}

/// Run an export (blocking; call from a worker thread). With `settings.max_threads` set it runs on
/// a pool of that many threads, so its rendering and software encoding use at most that many cores.
pub fn export(project: &Arc<Project>, seq: ItemId, settings: &ExportSettings, sources: &dyn SourceProvider, progress: &Progress) -> Result<Report> {
    match capped_pool(settings.max_threads) {
        Some(pool) => pool.install(|| export_here(project, seq, settings, sources, progress)),
        None => export_here(project, seq, settings, sources, progress),
    }
}

/// [`export`] on the threads it is called from.
fn export_here(project: &Arc<Project>, seq: ItemId, settings: &ExportSettings, sources: &dyn SourceProvider, progress: &Progress) -> Result<Report> {
    settings.validate()?;
    if stepped(settings.format) {
        let mut ex = Exporter::new(project.clone(), seq, settings, progress)?;
        loop {
            match ex.step(sources, progress)? {
                Step::Progress => {}
                Step::Done(r) => return Ok(r),
                // only asynchronous (web) sources defer; a blocking export cannot wait for them
                Step::Pending => return Err(ExportError::Io("media data is still loading; run the export as a stepped job".into())),
            }
        }
    }
    let t0 = web_time::Instant::now();
    let range = export_range(project, seq, settings)?;
    let cancelled = || progress.cancel.load(Ordering::Relaxed);
    let batch = rayon::current_num_threads().clamp(2, 16) as i64;
    let (bytes, nframes) = match settings.format {
        Format::Aac => aac_out::export(project, seq, settings, range, sources, progress)?,
        Format::Wav | Format::Aiff | Format::Flac => {
            let mut a = audio_out::AudioOut::new(project.clone(), seq, settings, range)?;
            let (ch, sr, fmt) = (a.channels as u16, a.sr, settings.audio.sample_format());
            let frames = a.remaining();
            // the sizes are known up front: refuse an AIFF that cannot hold them before measuring
            let mut writer = match settings.format {
                Format::Flac => {
                    let mut cfg = filmcraft_flac::EncoderConfig::new(sr, ch as u8, fmt.bits() as u8);
                    cfg.level = settings.audio.flac_level.min(8);
                    AudioWriter::Flac(Box::new(filmcraft_flac::Encoder::new(cfg).map_err(|e| ExportError::Unsupported(format!("FLAC: {e}")))?))
                }
                Format::Wav => AudioWriter::Pcm(pcm::PcmContainer::Wav),
                _ => AudioWriter::Pcm(pcm::PcmContainer::Aiff),
            };
            let head = match &writer {
                AudioWriter::Pcm(c) => pcm::header(*c, ch, sr, fmt, frames).map_err(ExportError::Unsupported)?,
                AudioWriter::Flac(enc) => enc.header(),
            };
            // one second per chunk: memory stays flat however long the range is
            let chunk = i64::from(sr.max(1));
            if !settings.part_of_batch {
                progress.total.store(frames.div_ceil(chunk as u64).max(1), Ordering::Relaxed);
                progress.set_status(format!("Exporting audio ({})", settings.format.label()));
            }
            a.measure(settings, sources, &cancelled)?;
            *progress.loudness.lock().unwrap_or_else(PoisonError::into_inner) = a.loudness;
            let mut out = Out::create(settings)?;
            let io = |e: std::io::Error| ExportError::Io(e.to_string());
            out.write_all(&head).map_err(io)?;
            let mut buf = Vec::new();
            while let Some(planar) = a.pull(a.out.saturating_add(chunk), sources) {
                if cancelled() {
                    return Err(ExportError::Cancelled);
                }
                buf.clear();
                match &mut writer {
                    AudioWriter::Pcm(c) => pcm::encode(*c, &audio_out::interleave(&planar), fmt, &mut buf),
                    AudioWriter::Flac(enc) => {
                        let ints: Vec<Vec<i32>> = planar.iter().map(|c| c.iter().map(|&s| fmt.quantise(s)).collect()).collect();
                        let refs: Vec<&[i32]> = ints.iter().map(Vec::as_slice).collect();
                        buf = enc.encode(&refs).map_err(|e| ExportError::Encode(format!("FLAC: {e}")))?;
                    }
                }
                out.write_all(&buf).map_err(io)?;
                if !settings.part_of_batch {
                    progress.done.fetch_add(1, Ordering::Relaxed);
                }
            }
            match &mut writer {
                AudioWriter::Pcm(_) => out.write_all(pcm::pad(frames, ch, fmt)).map_err(io)?,
                AudioWriter::Flac(enc) => {
                    out.write_all(&enc.finish()).map_err(io)?;
                    // the frame sizes and MD5 are known now: fill them into STREAMINFO
                    use std::io::{Seek, SeekFrom};
                    out.seek(SeekFrom::Start(filmcraft_flac::STREAMINFO_OFFSET)).map_err(io)?;
                    out.write_all(&enc.streaminfo()).map_err(io)?;
                    out.seek(SeekFrom::End(0)).map_err(io)?;
                }
            }
            let n = out.finish(settings)?;
            if settings.part_of_batch {
                progress.done.store(1, Ordering::Relaxed);
            }
            (n, frames)
        }
        f if f.is_image_sequence() || f == Format::Gif => {
            let pipe = pipeline::Pipeline::new(project.clone(), seq, settings, false)?;
            let batch = pipe.render_batch_limit(batch);
            let (f0, f1) = frame_span(pipe.rate, range);
            let count = (f1 - f0) as u64;
            let (w, h) = (pipe.w, pipe.h);
            if !settings.part_of_batch {
                progress.total.store(count, Ordering::Relaxed);
                progress.set_status(format!("Exporting {count} frames ({})", settings.format.label()));
            }
            let mut total = 0u64;
            let gif_buf = SharedBuf::default();
            let mut gif = if settings.format == Format::Gif {
                let mut enc = image::codecs::gif::GifEncoder::new_with_speed(gif_buf.clone(), 10);
                enc.set_repeat(image::codecs::gif::Repeat::Infinite).map_err(|e| ExportError::Encode(e.to_string()))?;
                Some(enc)
            } else {
                None
            };
            let mut f = f0;
            while f < f1 {
                if cancelled() {
                    return Err(ExportError::Cancelled);
                }
                let end = (f + batch).min(f1);
                if let Some(enc) = gif.as_mut() {
                    let frames: Vec<Vec<u8>> = (f..end).into_par_iter().map(|fi| pipe.frame(fi, sources).map(|f| f.0)).collect::<Result<Vec<_>>>()?;
                    for (fi, mut rgba) in (f..end).zip(frames) {
                        // GIF89a has one transparent palette index, not partial alpha; decide coverage before quantization and keep the straight colour of visible pixels.
                        if settings.keeps_alpha() {
                            for px in rgba.as_chunks_mut::<4>().0 {
                                if px[3] < 128 {
                                    *px = [0; 4];
                                } else {
                                    px[3] = 255;
                                }
                            }
                        }
                        let img = image::RgbaImage::from_raw(w, h, rgba).ok_or_else(|| ExportError::Encode("frame".into()))?;
                        let delay = image::Delay::from_numer_denom_ms(u32::from(gif_delay_cs(pipe.rate, fi - f0)) * 10, 1);
                        enc.encode_frame(image::Frame::from_parts(img, 0, 0, delay)).map_err(|e| ExportError::Encode(e.to_string()))?;
                    }
                } else {
                    let written: Vec<Result<u64>> = (f..end)
                        .into_par_iter()
                        .map(|fi| {
                            let data = encode_still_with(
                                settings.format,
                                pipe.frame(fi, sources)?.0,
                                w,
                                h,
                                settings.alpha,
                                settings.quality,
                                settings.exr_compression,
                            )?;
                            write_output(settings, &pcm::image_sequence_path(&settings.path, (fi - f0) as u64, count), data)
                        })
                        .collect();
                    for r in written {
                        total += r?;
                    }
                }
                progress.done.fetch_add((end - f) as u64, Ordering::Relaxed);
                f = end;
            }
            if let Some(enc) = gif {
                let buf = gif_buf.clone();
                drop(enc);
                let data = std::mem::take(&mut *buf.0.lock().unwrap_or_else(PoisonError::into_inner));
                total = write_output(settings, &settings.path, data)?;
            }
            (total, count)
        }
        _ => {
            // Handled by the stepped exporter above; reaching here would be a dispatch bug.
            return Err(ExportError::Unsupported(format!("{:?} must run as a stepped export", settings.format)));
        }
    };
    let secs = t0.elapsed().as_secs_f64();
    if !settings.part_of_batch {
        progress.finished.store(true, Ordering::Relaxed);
        progress.set_status(format!("Done in {secs:.1}s"));
    }
    Ok(Report { path: settings.path.clone(), frames: nframes, seconds: secs, bytes, render_fps: nframes as f64 / secs.max(1e-6), extra_files: Vec::new() })
}
