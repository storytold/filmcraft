//! ffmpeg-made AVI files (skipped without ffmpeg): the chunk tables match ffprobe's packet counts,
//! and every index the file has (`idx1`; OpenDML once a file passes 1 GB) and a `movi` scan describe
//! the same chunks.

use std::path::{Path, PathBuf};
use std::process::Command;

use filmcraft_avi::{IndexSource, OpenOptions, StreamKind, open, open_with};

fn fixture(name: &str, args: &[&str]) -> Option<PathBuf> {
    let ffmpeg = filmcraft_testkit::ffmpeg_or_skip("avi oracle")?;
    let out = filmcraft_testkit::fixtures_dir("avi").join(name);
    filmcraft_testkit::fixtures::generate(&out, |tmp: &Path| {
        let mut c = Command::new(&ffmpeg);
        c.args(["-v", "error", "-y"]).args(args).args(["-f", "avi"]).arg(tmp);
        c.status().is_ok_and(|s| s.success())
    })
}

fn packets(path: &Path, stream: usize) -> usize {
    let ffprobe = filmcraft_testkit::ffprobe().expect("ffprobe next to ffmpeg");
    let out = Command::new(ffprobe)
        .args(["-v", "error", "-select_streams", &stream.to_string(), "-count_packets", "-show_entries", "stream=nb_read_packets", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
}

fn packet_bytes(path: &Path, stream: usize) -> u64 {
    let ffprobe = filmcraft_testkit::ffprobe().expect("ffprobe next to ffmpeg");
    let out = Command::new(ffprobe)
        .args(["-v", "error", "-select_streams", &stream.to_string(), "-show_entries", "packet=size", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse::<u64>().ok()).sum()
}

const TONE: [&str; 4] = ["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:d=2"];

fn cases() -> Vec<(&'static str, Vec<&'static str>, [u8; 4])> {
    let pic = vec!["-f", "lavfi", "-i", "testsrc2=s=160x120:r=25:d=2"];
    let with = |v: &[&'static str], a: &[&'static str]| -> Vec<&'static str> {
        let mut x = pic.clone();
        x.extend_from_slice(&TONE);
        x.extend_from_slice(v);
        x.extend_from_slice(a);
        x
    };
    vec![
        ("mjpeg_pcm.avi", with(&["-c:v", "mjpeg", "-q:v", "4", "-pix_fmt", "yuvj420p"], &["-c:a", "pcm_s16le"]), *b"MJPG"),
        ("h264_mp3.avi", with(&["-c:v", "libx264", "-bf", "2", "-g", "12", "-pix_fmt", "yuv420p"], &["-c:a", "libmp3lame", "-b:a", "128k"]), *b"H264"),
        ("bgr24_pcm.avi", with(&["-c:v", "rawvideo", "-pix_fmt", "bgr24"], &["-c:a", "pcm_s16le"]), [0; 4]),
        ("yuyv_pcm.avi", with(&["-c:v", "rawvideo", "-pix_fmt", "yuyv422"], &["-c:a", "pcm_s16le"]), *b"YUY2"),
    ]
}

#[test]
fn ffmpeg_files_match_ffprobe_on_every_index_path() {
    for (name, args, fourcc) in cases() {
        let Some(path) = fixture(name, &args) else { return };
        let bytes = std::fs::read(&path).unwrap();
        let f = open(&bytes).unwrap();
        // ffmpeg reserves room for the OpenDML index but writes only idx1 under 1 GB
        assert_eq!(f.index, IndexSource::Idx1, "{name}");
        assert_eq!(f.streams.len(), 2, "{name}");
        let (v, a) = (&f.streams[0], &f.streams[1]);
        assert_eq!((v.kind, a.kind), (StreamKind::Video, StreamKind::Audio), "{name}");
        let bmp = v.video.as_ref().unwrap();
        assert_eq!((bmp.width, bmp.height.abs(), bmp.compression), (160, 120, fourcc), "{name}");
        assert_eq!((v.rate as f64 / v.scale as f64).round(), 25.0, "{name}");
        assert_eq!(a.audio.as_ref().map(|w| (w.sample_rate, w.channels)), Some((48_000, 1)), "{name}");
        // the tables hold what ffprobe reads
        assert_eq!(v.chunks.len(), packets(&path, 0), "{name}: video");
        // PCM chunks are packets; ffprobe re-splits MP3 into its frames, which AVI chunks need not
        // follow, so compressed audio compares by bytes
        if a.audio.as_ref().is_some_and(|w| w.format_tag == 1) {
            assert_eq!(a.chunks.len(), packets(&path, 1), "{name}: audio");
        } else {
            let ours: u64 = a.chunks.iter().map(|c| u64::from(c.size)).sum();
            assert_eq!(ours, packet_bytes(&path, 1), "{name}: audio bytes");
        }
        assert!(v.chunks[0].key, "{name}: the first frame is a key frame");
        if fourcc == *b"H264" {
            assert!(v.chunks.iter().any(|c| !c.key), "{name}: the index marks the delta frames");
        }
        // a scan finds the same chunks (it cannot know key frames)
        let scanned = open_with(&bytes, OpenOptions { ignore_odml: true, ignore_idx1: true }).unwrap();
        assert_eq!(scanned.index, IndexSource::Scan, "{name}");
        for s in 0..2 {
            assert_eq!(sizes(&scanned, s), sizes(&f, s), "{name}: scan stream {s}");
        }
    }
}

fn sizes(f: &filmcraft_avi::AviFile, s: usize) -> Vec<(u64, u32)> {
    f.streams[s].chunks.iter().map(|c| (c.offset, c.size)).collect()
}

/// A file past 1 GB, as long recordings are: ffmpeg writes `RIFF AVIX` extensions and the
/// OpenDML index. Needs ffmpeg and 1.2 GB of disk: `cargo test -p filmcraft-avi --test
/// ffmpeg_oracle -- --ignored`.
#[test]
#[ignore]
fn opendml_file_past_1_gb() {
    let args = [
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=640x480:r=25:d=50",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:d=50",
        "-c:v",
        "rawvideo",
        "-pix_fmt",
        "bgr24",
        "-c:a",
        "pcm_s16le",
    ];
    let Some(path) = fixture("large_opendml.avi", &args) else { return };
    let file = std::fs::File::open(&path).unwrap();
    let src = FileSource(file);
    let f = open(&src).unwrap();
    assert_eq!(f.index, IndexSource::OpenDml);
    assert_eq!(f.streams[0].chunks.len(), packets(&path, 0));
    assert_eq!(f.streams[1].chunks.len(), packets(&path, 1));
    assert_eq!(f.streams[0].chunks.len(), 1250);
    assert!(f.streams[0].chunks.last().unwrap().offset > 1 << 30, "the last frames are in an AVIX extension");
    // idx1 covers the first RIFF only; the AVIX extensions are scanned: the same chunks
    let idx1 = open_with(&src, OpenOptions { ignore_odml: true, ..Default::default() }).unwrap();
    assert_eq!(idx1.index, IndexSource::Idx1);
    for s in 0..2 {
        assert_eq!(sizes(&idx1, s), sizes(&f, s), "stream {s}");
    }
    assert_eq!(f.read_chunk(&src, 0, 1249).unwrap().len(), 640 * 480 * 3);
}

/// A file on disk as a byte source, read where asked.
struct FileSource(std::fs::File);

impl filmcraft_avi::ByteSource for FileSource {
    fn len(&self) -> u64 {
        self.0.metadata().map(|m| m.len()).unwrap_or(0)
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        std::os::unix::fs::FileExt::read_exact_at(&self.0, buf, offset)
    }
}
