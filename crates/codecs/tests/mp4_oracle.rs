//! MP4 audio against ffmpeg: ffmpeg writes an AAC track (with the edit list that skips the encoder
//! priming), and reads at another sample rate must land on the same source samples as the native
//! decode, however the requests are cut.

mod common;

use common::*;

/// A source whose rate differs from the sequence's is resampled per request. An AAC track in MP4
/// carries an edit list that skips the encoder priming (ffmpeg: 1024 samples), and every request
/// must still land on the same source samples: reading at 44.1 kHz in video-frame-sized pieces (as
/// an export does) equals the 48 kHz decode interpolated at the same times.
#[test]
fn aac_mp4_resampled_reads_follow_the_edit_list() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(f) = fixture(&ff, "tone_aac_48k.mp4", &["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:d=3", "-c:a", "aac", "-b:a", "128k"]) else {
        panic!("could not write the AAC fixture")
    };
    let src = filmcraft_codecs::open_bytes("tone_aac_48k.mp4", bytes(&f)).unwrap();
    let native = src.audio(0, 2 * 48_000, 48_000).unwrap().channels[0].clone();
    let ratio = 48_000.0 / 44_100.0;
    // 30 fps at 44.1 kHz, starting mid-file and off the 48 kHz grid
    let (mut start, piece) = (22_051i64, 1470usize);
    let (mut worst, mut peak) = (0f32, 0f32);
    for _ in 0..20 {
        let got = src.audio(start, piece, 44_100).unwrap();
        for (k, &v) in got.channels[0].iter().enumerate() {
            let pos = (start + k as i64) as f64 * ratio;
            let i0 = pos.floor() as usize;
            let want = native[i0] + (native[i0 + 1] - native[i0]) * (pos - i0 as f64) as f32;
            worst = worst.max((v - want).abs());
            peak = peak.max(v.abs());
        }
        start += piece as i64;
    }
    let level = native.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(peak <= level * 1.01, "resampled peak {peak} above the source's {level}");
    assert!(worst < 1e-4, "resampled read differs from the source by {worst}");
}

/// ffmpeg's MP4 muxer skips the B-frame reorder delay with an edit list, so the media (`mdhd`) runs
/// one frame past what plays. The clip must be as long as ffmpeg decodes it: 60 frames, not 61.
#[test]
fn bframe_mp4_duration_is_the_decoded_frame_count() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let args = ["-f", "lavfi", "-i", "testsrc2=size=160x96:rate=30:duration=2", "-c:v", "libx264", "-bf", "2", "-pix_fmt", "yuv420p"];
    let Some(f) = fixture(&ff, "bframes_x264.mp4", &args) else {
        eprintln!("SKIPPED: ffmpeg without libx264");
        return;
    };
    let n = ffmpeg_frames(&ff, &f, "yuv420p").len() / (160 * 96 * 3 / 2);
    assert_eq!(n, 60);
    let src = filmcraft_codecs::open_bytes("bframes_x264.mp4", bytes(&f)).unwrap();
    assert_eq!(src.info().duration, filmcraft_time::FrameRate::FPS_30.tick_of(n as i64));
}

/// `mp4` (moov after mdat, as ffmpeg writes it) with a clean aperture (`clap`) of `w`×`h` moved by
/// (`dx`, `dy`) from the centre, inserted at the end of the first video sample entry.
fn with_clap(mp4: &[u8], (w, h, dx, dy): (u32, u32, i32, i32)) -> Vec<u8> {
    let be = |b: &[u8], at: usize| u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize;
    // the chain moov > trak > mdia > minf > stbl > stsd > avc1 (stsd has 8 bytes before its entries)
    let mut chain = Vec::new();
    let (mut off, mut end) = (0, mp4.len());
    for (want, skip) in [(b"moov", 8), (b"trak", 8), (b"mdia", 8), (b"minf", 8), (b"stbl", 8), (b"stsd", 16), (b"avc1", 0)] {
        loop {
            assert!(off + 8 <= end, "no {}", String::from_utf8_lossy(want));
            let size = be(mp4, off);
            if &mp4[off + 4..off + 8] == want {
                chain.push(off);
                end = off + size;
                off += skip;
                break;
            }
            off += size;
        }
    }
    let entry = *chain.last().unwrap();
    assert!(mp4.windows(4).position(|x| x == b"mdat").unwrap() < chain[0], "moov must follow mdat");
    let mut clap = Vec::new();
    for v in [40u32, u32::from_be_bytes(*b"clap"), w, 1, h, 1, dx as u32, 1, dy as u32, 1] {
        clap.extend_from_slice(&v.to_be_bytes());
    }
    let mut out = mp4.to_vec();
    let at = entry + be(mp4, entry);
    out.splice(at..at, clap);
    for &b in &chain {
        let size = (be(&out, b) + 40) as u32;
        out[b..b + 4].copy_from_slice(&size.to_be_bytes());
    }
    out
}

/// Cropped pictures are the size and the samples ffmpeg decodes (#288): H.264 frame cropping (SPS)
/// and an HEVC conformance window while the sample entry gives the uncropped size, and a clean
/// aperture (`clap`, as iPhones write), centred, off-centre, and with a 90° display rotation
/// applied after it.
#[test]
fn cropped_pictures_match_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let src = ["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=1,noise=alls=10:allf=t", "-c:v", "libx264", "-bf", "2", "-pix_fmt", "yuv420p"];
    let Some(plain) = fixture(&ff, "crop_plain_x264.mp4", &src) else {
        eprintln!("SKIPPED: ffmpeg without libx264");
        return;
    };
    let sps =
        fixture(&ff, "crop_sps_x264.mp4", &[&src[..], &["-bsf:v", "h264_metadata=crop_left=16:crop_right=16:crop_top=8:crop_bottom=8"]].concat()).unwrap();
    // the display matrix added without re-encoding (an encode would turn the pixels instead)
    let rotated = fixture(&ff, "crop_rotated_x264.mp4", &["-display_rotation", "90", "-i", plain.to_str().unwrap(), "-c", "copy"]).unwrap();
    let dir = dir();
    let mut cases = vec![("SPS crop", sps, (288, 224))];
    // HEVC: the conformance window
    let hevc = [
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x240:rate=30:duration=1,noise=alls=10:allf=t",
        "-c:v",
        "libx265",
        "-x265-params",
        "log-level=error:bframes=3",
        "-tag:v",
        "hvc1",
        "-pix_fmt",
        "yuv420p",
    ];
    match fixture(&ff, "crop_conformance_x265.mp4", &[&hevc[..], &["-bsf:v", "hevc_metadata=crop_left=16:crop_right=16:crop_top=8:crop_bottom=8"]].concat()) {
        Some(p) => cases.push(("HEVC conformance window", p, (288, 224))),
        None => eprintln!("SKIPPED (HEVC case): ffmpeg without libx265"),
    }
    for (name, base, clap, size) in [
        ("centred clap", &plain, (280, 200, 0, 0), (280, 200)),
        ("off-centre clap", &plain, (200, 150, -30, 22), (200, 150)),
        ("clap then rotation", &rotated, (280, 200, 0, 0), (200, 280)),
    ] {
        let path = dir.join(format!("crop_{}.mp4", name.replace(' ', "_")));
        std::fs::write(&path, with_clap(&bytes(base), clap)).unwrap();
        cases.push((name, path, size));
    }
    for (name, path, (w, h)) in cases {
        let src = filmcraft_codecs::open_bytes(path.file_name().unwrap().to_str().unwrap(), bytes(&path)).unwrap();
        let v = src.info().video.clone().unwrap();
        assert_eq!((v.width, v.height), (w, h), "{name}: reported size");
        let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
        // `-flags unaligned`: ffmpeg otherwise skips a left crop that would misalign its frame
        // pointers (on AVX-512 machines it needs 64-byte alignment) and returns wider frames
        let raw = ffmpeg_out(
            &ff,
            &["-flags", "unaligned", "-i", path.to_str().unwrap(), "-map", "0:v:0", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"],
        );
        let frame_bytes = (w * h) as usize + 2 * cw * ch;
        if name.contains("clap") && !raw.len().is_multiple_of(frame_bytes) {
            // ffmpeg before 7.1 ignores `clap` and decodes the whole picture
            eprintln!("SKIPPED ({name}): this ffmpeg does not apply the clean aperture");
            continue;
        }
        assert_eq!(raw.len() % frame_bytes, 0, "{name}: ffmpeg's frames are {w}x{h}");
        let n = raw.len() / frame_bytes;
        assert_eq!(n, 30, "{name}");
        for (i, want) in raw.chunks(frame_bytes).enumerate() {
            let f = src.video_frame(filmcraft_media::FrameRequest::full(v.frame_rate.tick_of(i as i64))).unwrap();
            assert_eq!((f.width, f.height), (w, h), "{name}: frame {i} size");
            assert!(planes(&f) == raw_planes(want, w as usize, h as usize, cw, ch, 1), "{name}: frame {i} differs from ffmpeg's");
        }
    }
}
