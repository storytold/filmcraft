//! Writes a contact sheet of the generator outputs: `cargo run -p filmcraft-media --example demo_frames -- out.png`
use filmcraft_media::generators::{DemoScene, Generator, render};
use filmcraft_time::FrameRate;

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "demo_frames.png".into());
    let (w, h) = (480u32, 270u32);
    let mut gens: Vec<Generator> = DemoScene::ALL.iter().map(|s| Generator::Demo(*s)).collect();
    gens.push(Generator::BarsAndTone);
    gens.push(Generator::CountingLeader);
    let cols = 4;
    let rows = gens.len().div_ceil(cols);
    let mut sheet = image::RgbaImage::new(w * cols as u32, h * rows as u32);
    for (i, g) in gens.iter().enumerate() {
        let px = render(g, w, h, 3.3, 79, FrameRate::FPS_23_976).unwrap();
        let img = image::RgbaImage::from_raw(w, h, px).unwrap();
        image::imageops::overlay(&mut sheet, &img, ((i % cols) as u32 * w) as i64, ((i / cols) as u32 * h) as i64);
    }
    sheet.save(&out).unwrap();
}
