//! NTSC DV25 QuickTime (`dvc `) with big-endian PCM against ffmpeg: frames requested out of order
//! and then sequentially are the right frames within the `filmcraft-dv` oracle tolerance, the
//! 4:1:1 chroma reaches the compositor replicated (not resampled) into 4:2:2, and the audio is
//! sample-exact.

mod common;

use common::*;
use filmcraft_media::FrameRequest;

const W: usize = 720;
const H: usize = 480;

#[test]
fn native_dv_mov_with_big_endian_pcm_and_random_seeking() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let name = "native_dv_ntsc_pcm.mov";
    let args = [
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=720x480:rate=30000/1001:duration=1",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:duration=1",
        "-c:v",
        "dvvideo",
        "-pix_fmt",
        "yuv411p",
        "-threads",
        "1",
        "-c:a",
        "pcm_s16be",
        "-ac",
        "2",
        "-shortest",
    ];
    let f = fixture(&ff, name, &args).unwrap_or_else(|| panic!("could not write {name}"));
    let src = filmcraft_codecs::open_bytes(name, bytes(&f)).unwrap();
    let video = src.info().video.clone().unwrap();
    assert_eq!(video.codec, "DV / DVCPRO NTSC");
    assert_eq!(video.pixel_format, "YUV 4:1:1 8-bit");
    assert_eq!(video.frame_rate, filmcraft_time::FrameRate::FPS_29_97);
    assert_eq!((video.width, video.height), (W as u32, H as u32));

    let reference = ffmpeg_frames(&ff, &f, "yuv411p");
    let frame_len = W * H + 2 * (W / 4) * H;
    // 29 frames: `-shortest` ends the 29.97 fps video with the one-second sine.
    let frames = reference.len() / frame_len;
    assert_eq!((frames, reference.len() % frame_len), (29, 0));
    // Back and forth, then sequential playback. testsrc2 changes every frame, so a seek that lands
    // on a neighbouring frame is far outside the tolerance.
    for k in [frames - 1, 1, 18, 0, 15].into_iter().chain(20..26) {
        let frame = src.video_frame(FrameRequest::full(video.frame_rate.tick_of(k as i64))).unwrap();
        assert_eq!((frame.width, frame.height), (W as u32, H as u32));
        let ours = planes(&frame);
        for c in &ours[1..] {
            assert_eq!(c.len(), (W / 2) * H, "frame {k}: chroma is not 4:2:2");
            assert!(c.as_chunks::<2>().0.iter().all(|[a, b]| a == b), "frame {k}: chroma is not replicated");
        }
        let ours = [ours[0].clone(), ours[1].iter().step_by(2).copied().collect(), ours[2].iter().step_by(2).copied().collect()];
        let want = raw_planes(&reference[k * frame_len..(k + 1) * frame_len], W, H, W / 4, H, 1);
        let max = max_diff(&ours, &want);
        assert!(max <= 3, "frame {k}: max difference {max} against ffmpeg");
    }

    let want = ffmpeg_audio_f32(&ff, &f, &[]);
    let got = src.audio(0, want.len() / 2, 48_000).unwrap();
    assert_eq!(got.channels.len(), 2);
    let interleaved: Vec<f32> = (0..want.len() / 2).flat_map(|i| [got.channels[0][i], got.channels[1][i]]).collect();
    assert_eq!(interleaved, want, "PCM differs from ffmpeg");
}
