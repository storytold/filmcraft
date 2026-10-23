use filmcraft_rotoscope::{Frame, Input, Kind, Point, runtime::Runtime};
use serde::Deserialize;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::AtomicBool;

#[derive(Deserialize)]
struct Fixture {
    width: usize,
    height: usize,
    files: Vec<String>,
    times: Vec<i64>,
    end: i64,
    points: Vec<FixturePoint>,
    quality: bool,
    band: usize,
    #[serde(default)]
    base_alpha: Option<String>,
}
#[derive(Deserialize)]
struct FixturePoint {
    frame: usize,
    x: f32,
    y: f32,
    kind: String,
}

/// Load one pinned runtime and process bounded fixture/result pairs, reusing caches between pairs.
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    let path = args.get(1).ok_or("Provide the native model directory")?;
    let mut runtime = Runtime::load(Path::new(path))?;
    println!("Native model graphs loaded");
    let pairs = args.get(2..).ok_or("Invalid oracle arguments")?;
    if pairs.len() > 32 || pairs.len() % 2 != 0 {
        return Err("Provide at most 16 fixture/result pairs".into());
    }
    for pair in pairs.chunks_exact(2) {
        let [path, output] = pair else {
            return Err("Provide fixture/result pairs".into());
        };
        let fixture: Fixture = serde_json::from_slice(&read_bounded(Path::new(path), 64 * 1024)?).map_err(|e| e.to_string())?;
        let pixels = fixture.width.checked_mul(fixture.height).ok_or("Fixture dimensions overflow")?;
        if fixture.files.is_empty()
            || fixture.files.len() > 120
            || fixture.width.max(fixture.height) > 720
            || pixels == 0
            || pixels.checked_mul(fixture.files.len()).is_none_or(|n| n > 40_000_000)
        {
            return Err("Fixture exceeds the frame/pixel budget".into());
        }
        let limit = pixels.checked_mul(4).ok_or("Fixture RGBA size overflow")?;
        let data: Vec<_> = fixture.files.iter().map(|path| read_bounded(Path::new(path), limit)).collect::<Result<_, _>>()?;
        let base = fixture
            .base_alpha
            .as_ref()
            .map(|path| {
                let bytes = read_bounded(Path::new(path), pixels * 2)?;
                if bytes.len() != pixels * 2 {
                    return Err("Existing alpha length differs".into());
                }
                Ok::<Vec<u16>, String>(bytes.chunks_exact(2).map(|value| u16::from_le_bytes([value[0], value[1]])).collect())
            })
            .transpose()?;
        if fixture.times.len() != data.len() {
            return Err("Fixture times/frames differ".into());
        }
        let input = Input {
            width: fixture.width,
            height: fixture.height,
            end: filmcraft_time::Tick(fixture.end),
            quality: fixture.quality,
            band: fixture.band,
            base_alpha: base.as_deref(),
            frames: data.iter().zip(&fixture.times).map(|(rgba, time)| Frame { rgba, time: filmcraft_time::Tick(*time) }).collect(),
            points: fixture
                .points
                .into_iter()
                .map(|p| {
                    Ok(Point {
                        frame: p.frame,
                        x: p.x,
                        y: p.y,
                        kind: match p.kind.as_str() {
                            "include" => Kind::Include,
                            "exclude" => Kind::Exclude,
                            "refine" => Kind::Refine,
                            _ => return Err("Invalid point kind"),
                        },
                    })
                })
                .collect::<Result<_, _>>()?,
        };
        let started = std::time::Instant::now();
        let mask = runtime.infer(&input, &AtomicBool::new(false), |done, status| println!("{done} {status}"))?;
        std::fs::write(output, serde_json::to_vec(&mask).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        println!("Completed in {:.3}s", started.elapsed().as_secs_f64());
    }
    Ok(())
}

/// Read fixture bytes with a hard size cap and reject oversized or unreadable inputs.
fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1) as u64).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    if bytes.len() > limit {
        return Err("Oracle input exceeds the size budget".into());
    }
    Ok(bytes)
}
