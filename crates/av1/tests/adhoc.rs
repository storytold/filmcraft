//! Compare an arbitrary IVF file with libdav1d:
//! `AV1_IVF=/path/x.ivf AV1_PIX=yuv420p cargo test -p filmcraft-av1 --test adhoc -- --ignored --nocapture`

mod common;
use common::*;

#[test]
#[ignore]
fn adhoc_compare() {
    let Some(ff) = ffmpeg_dav1d() else { return };
    let Some(path) = std::env::var_os("AV1_IVF") else { return };
    let path = std::path::PathBuf::from(path);
    let pics = decode_all(&path).unwrap();
    let pix = pix_fmt_for(&pics[0]).to_string();
    let raw = reference(&ff, &path, &pix);
    let per = picture_samples(&pics[0]);
    println!("{} pictures, reference has {} frames", pics.len(), raw.len() / per);
    for (i, p) in pics.iter().enumerate() {
        if (i + 1) * per > raw.len() {
            break;
        }
        let (first, count) = compare(p, &raw[i * per..(i + 1) * per]);
        println!("frame {i}: {} mismatches, first {:?}", count, first);
    }
}
