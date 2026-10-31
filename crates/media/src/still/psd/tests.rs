use super::*;

const GRAY: u16 = 1;
const RGB: u16 = 3;

/// PackBits as Photoshop writes it: runs of 3+ equal bytes repeated, the rest literal.
fn packbits(row: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < row.len() {
        let run = row[i..].iter().take(128).take_while(|&&b| b == row[i]).count();
        if run >= 3 {
            out.extend([(1 - run as i16) as u8, row[i]]);
            i += run;
        } else {
            let mut j = i + 1;
            while j < row.len() && j - i < 128 && !(j + 2 < row.len() && row[j] == row[j + 1] && row[j] == row[j + 2]) {
                j += 1;
            }
            out.push((j - i - 1) as u8);
            out.extend_from_slice(&row[i..j]);
            i = j;
        }
    }
    out
}

/// A minimal document: header, an empty colour mode section, one image resource, the given layer
/// and mask section, then the composite (`planes` are whole channels, top row first).
fn psd(mode: u16, depth: u16, w: u32, h: u32, planes: &[Vec<u16>], rle: bool, layers: &[u8]) -> Vec<u8> {
    let mut v = b"8BPS".to_vec();
    v.extend(1u16.to_be_bytes());
    v.extend([0; 6]);
    v.extend((planes.len() as u16).to_be_bytes());
    v.extend(h.to_be_bytes());
    v.extend(w.to_be_bytes());
    v.extend(depth.to_be_bytes());
    v.extend(mode.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    let res = b"8BIM\x04\x0c\0\0\0\0\0\x02ab";
    v.extend((res.len() as u32).to_be_bytes());
    v.extend(res);
    v.extend((layers.len() as u32).to_be_bytes());
    v.extend(layers);
    v.extend(u16::from(rle).to_be_bytes());
    let rows: Vec<Vec<u8>> = planes
        .iter()
        .flat_map(|p| {
            p.chunks(w.max(1) as usize).map(|r| r.iter().flat_map(|&s| if depth == 8 { vec![s as u8] } else { s.to_be_bytes().to_vec() }).collect::<Vec<u8>>())
        })
        .collect();
    if rle {
        let packed: Vec<Vec<u8>> = rows.iter().map(|r| packbits(r)).collect();
        for p in &packed {
            v.extend((p.len() as u16).to_be_bytes());
        }
        packed.iter().for_each(|p| v.extend(p));
    } else {
        rows.iter().for_each(|r| v.extend(r));
    }
    v
}

/// A layer and mask section whose layer info holds only a layer count.
fn layer_count(n: i16) -> Vec<u8> {
    let mut v = 2u32.to_be_bytes().to_vec();
    v.extend(n.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    v
}

/// The 16-bit form: an empty layer info, an empty global mask, then the layers in an `Lr16` block.
fn lr16_count(n: i16) -> Vec<u8> {
    let mut v = 0u32.to_be_bytes().to_vec();
    v.extend(0u32.to_be_bytes());
    v.extend(b"8BIMLr16");
    v.extend(2u32.to_be_bytes());
    v.extend(n.to_be_bytes());
    v
}

fn rgba(r: &Raster) -> Vec<[u8; 4]> {
    r.rgba.chunks(4).map(|p| [p[0], p[1], p[2], p[3]]).collect()
}

fn rgb_planes() -> Vec<Vec<u16>> {
    // 4 x 2, with a run long enough for PackBits to repeat
    vec![vec![10, 10, 10, 10, 1, 2, 3, 4], vec![20, 21, 22, 23, 5, 5, 5, 6], vec![30, 31, 32, 33, 7, 8, 9, 9]]
}

#[test]
fn rgb_8bit_raw_and_rle() {
    for rle in [false, true] {
        let r = decode("a.psd", &psd(RGB, 8, 4, 2, &rgb_planes(), rle, &[])).unwrap();
        assert_eq!((r.width, r.height, r.has_alpha), (4, 2, false), "rle {rle}");
        let p = rgba(&r);
        assert_eq!(p[0], [10, 20, 30, 255], "rle {rle}");
        assert_eq!(p[3], [10, 23, 33, 255], "rle {rle}");
        assert_eq!(p[4], [1, 5, 7, 255], "rle {rle}");
        assert_eq!(p[7], [4, 6, 9, 255], "rle {rle}");
    }
}

#[test]
fn gray_8bit_raw_and_rle() {
    for rle in [false, true] {
        let r = decode("a.psd", &psd(GRAY, 8, 3, 1, &[vec![0, 128, 255]], rle, &[])).unwrap();
        assert_eq!(rgba(&r), [[0, 0, 0, 255], [128, 128, 128, 255], [255, 255, 255, 255]], "rle {rle}");
    }
}

#[test]
fn sixteen_bit_is_rounded_to_eight() {
    // 257 * v is exactly v at 8 bits; 0x7fff and 0x8000 straddle the rounding point between 127 and 128
    let gray = vec![0, 0xffff, 257 * 100, 0x7fff, 0x8000];
    for rle in [false, true] {
        let r = decode("a.psd", &psd(GRAY, 16, 5, 1, std::slice::from_ref(&gray), rle, &[])).unwrap();
        let y: Vec<u8> = rgba(&r).iter().map(|p| p[0]).collect();
        assert_eq!(y, [0, 255, 100, 127, 128], "rle {rle}");
        let planes = [gray.clone(), gray.iter().map(|&s| 0xffff - s).collect(), vec![257 * 7; 5]];
        let r = decode("a.psd", &psd(RGB, 16, 5, 1, &planes, rle, &[])).unwrap();
        assert_eq!(rgba(&r)[2], [100, 155, 7, 255], "rle {rle}");
    }
}

#[test]
fn merged_alpha_only_when_the_layer_count_says_so() {
    let mut planes = rgb_planes();
    planes.push(vec![255, 0, 128, 255, 255, 255, 255, 255]);
    // a negative layer count: the first extra channel is the composite's transparency
    let r = decode("a.psd", &psd(RGB, 8, 4, 2, &planes, true, &layer_count(-2))).unwrap();
    assert!(r.has_alpha);
    let a: Vec<u8> = rgba(&r).iter().map(|p| p[3]).collect();
    assert_eq!(a, [255, 0, 128, 255, 255, 255, 255, 255]);
    // a flat document (no layer section) or a positive count: the extra channel is a saved selection
    for layers in [vec![], layer_count(2)] {
        let r = decode("a.psd", &psd(RGB, 8, 4, 2, &planes, false, &layers)).unwrap();
        assert!(!r.has_alpha);
        assert!(rgba(&r).iter().all(|p| p[3] == 255));
        assert_eq!(rgba(&r)[1], [10, 21, 31, 255]);
    }
    // 16-bit documents keep their layers in an Lr16 block; grayscale has its alpha second
    // (the second pixel is 191 at 64/255 opacity, stored matted on white as 239)
    let gray = [vec![257 * 9, 257 * 239], vec![0xffff, 257 * 64]];
    let r = decode("a.psd", &psd(GRAY, 16, 2, 1, &gray, false, &lr16_count(-1))).unwrap();
    assert_eq!(rgba(&r), [[9, 9, 9, 255], [191, 191, 191, 64]]);
    let r = decode("a.psd", &psd(GRAY, 16, 2, 1, &gray, false, &lr16_count(1))).unwrap();
    assert_eq!(rgba(&r), [[9, 9, 9, 255], [239, 239, 239, 255]]);
}

/// Global tagged blocks pad their data to 4 bytes: a 6-byte block before `Lr16` takes 8.
fn padded_lr16_count(n: i16) -> Vec<u8> {
    let mut v = 0u32.to_be_bytes().to_vec();
    v.extend(0u32.to_be_bytes());
    v.extend(b"8BIMPatt");
    v.extend(6u32.to_be_bytes());
    v.extend([1, 2, 3, 4, 5, 6, 0, 0]);
    v.extend(b"8BIMLr16");
    v.extend(2u32.to_be_bytes());
    v.extend(n.to_be_bytes());
    v
}

#[test]
fn tagged_blocks_are_aligned_to_four_bytes() {
    let gray = [vec![0, 0], vec![0xffff, 0]];
    let r = decode("a.psd", &psd(GRAY, 16, 2, 1, &gray, false, &padded_lr16_count(-1))).unwrap();
    assert!(r.has_alpha);
    assert_eq!(rgba(&r)[1][3], 0);
}

#[test]
fn transparent_composite_is_unmatted_from_white() {
    // Photoshop stores the composite blended over white: (204, 115, 225) at alpha 140 is saved as
    // (227, 178, 239); fully transparent pixels are saved white and stay so
    for depth in [8u16, 16] {
        let k = if depth == 8 { 1 } else { 257 };
        let planes: Vec<Vec<u16>> =
            [[204u16, 227, 255], [115, 178, 255], [225, 239, 255], [255, 140, 0]].iter().map(|c| c.iter().map(|&v| v * k).collect()).collect();
        for rle in [false, true] {
            let r = decode("a.psd", &psd(RGB, depth, 3, 1, &planes, rle, &layer_count(-1))).unwrap();
            let p = rgba(&r);
            assert_eq!(p[0], [204, 115, 225, 255], "{depth} bit");
            assert_eq!(p[1], [204, 115, 226, 140], "{depth} bit");
            assert_eq!(p[2], [255, 255, 255, 0], "{depth} bit");
        }
    }
}

/// `file` with its image resources replaced by one version info resource (1057).
fn with_version_info(file: &[u8], real_merged_data: u8) -> Vec<u8> {
    let mut res = b"8BIM\x04\x21\0\0".to_vec();
    res.extend(5u32.to_be_bytes());
    res.extend([0, 0, 0, 1, real_merged_data, 0]);
    let old = u32::from_be_bytes(file[30..34].try_into().unwrap()) as usize;
    let mut v = file[..30].to_vec();
    v.extend((res.len() as u32).to_be_bytes());
    v.extend(res);
    v.extend(&file[34 + old..]);
    v
}

#[test]
fn documents_without_a_real_composite_are_refused() {
    let file = psd(RGB, 8, 4, 2, &rgb_planes(), true, &[]);
    assert_eq!(rgba(&decode("a.psd", &with_version_info(&file, 1)).unwrap())[0], [10, 20, 30, 255]);
    let msg = decode("a.psd", &with_version_info(&file, 0)).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(msg.contains("Maximize Compatibility"), "{msg}");
}

#[test]
fn unsupported_documents_say_why() {
    let msg = |b: &[u8]| decode("a.psd", b).err().map(|e| e.to_string()).unwrap_or_default();
    let ok = psd(RGB, 8, 4, 2, &rgb_planes(), false, &[]);
    let with = |at: usize, bytes: &[u8]| {
        let mut b = ok.clone();
        b[at..at + bytes.len()].copy_from_slice(bytes);
        b
    };
    assert!(msg(&with(4, &[0, 2])).contains("PSB"));
    assert!(msg(&with(24, &[0, 4])).contains("CMYK"));
    assert!(msg(&with(24, &[0, 9])).contains("Lab"));
    assert!(msg(&with(24, &[0, 2])).contains("indexed"));
    assert!(msg(&with(24, &[0, 0])).contains("bitmap"));
    assert!(msg(&with(24, &[0, 7])).contains("multichannel"));
    assert!(msg(&with(24, &[0, 8])).contains("duotone"));
    assert!(msg(&with(22, &[0, 32])).contains("32-bit"));
    assert!(msg(&with(22, &[0, 1])).contains("1-bit"));
    let zip = ok.len() - 3 * 8 - 2;
    assert!(msg(&with(zip, &[0, 2])).contains("ZIP"));
    assert!(msg(&with(zip, &[0, 3])).contains("ZIP"));
    assert!(decode("a.psd", &with(zip, &[0, 9])).is_err());
    assert!(decode("a.psd", &with(4, &[0, 3])).is_err());
}

#[test]
fn hostile_input_is_an_error_not_a_panic() {
    let rgb = |w: u32, h: u32| psd(RGB, 8, w, h, &rgb_planes(), false, &[]);
    assert!(decode("a.psd", &rgb(4, 2)).is_ok());
    assert!(decode("a.psd", &rgb(0, 2)).is_err());
    assert!(decode("a.psd", &rgb(30_001, 1)).is_err());
    assert!(decode("a.psd", &rgb(30_000, 30_000)).is_err());
    // a big but legal size whose data is not there is refused before the picture is allocated
    assert!(decode("a.psd", &rgb(16_000, 16_000)).is_err());
    assert!(decode("a.psd", &psd(RGB, 8, 4, 2, &rgb_planes()[..2], false, &[])).is_err());
    // an RLE row that decodes past the row's end
    let mut b = psd(GRAY, 8, 2, 1, &[vec![1, 2]], true, &[]);
    let n = b.len();
    b.splice(n - 3.., [0xfe, 7]);
    b.splice(n - 5..n - 3, 2u16.to_be_bytes());
    assert!(decode("a.psd", &b).is_err());
    // section lengths that point past the end
    let mut b = rgb(4, 2);
    b[26..30].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(decode("a.psd", &b).is_err());
    assert!(decode("a.psd", b"8BPS").is_err());
    assert!(decode("a.psd", b"GIF89a").is_err());

    let mut planes = rgb_planes();
    planes.push(vec![255, 0, 128, 255, 255, 255, 255, 255]);
    for file in [psd(RGB, 8, 4, 2, &planes, true, &layer_count(-2)), psd(GRAY, 16, 2, 1, &[vec![1, 2], vec![3, 4]], false, &lr16_count(-1))] {
        for cut in 0..file.len() {
            assert!(decode("a.psd", &file[..cut]).is_err(), "cut at {cut}");
        }
        for i in 0..file.len() {
            for v in [0x00, 0x7f, 0x80, 0xff] {
                let mut b = file.clone();
                b[i] = v;
                assert!(std::panic::catch_unwind(|| decode("a.psd", &b)).is_ok(), "byte {i} = {v:#x}");
            }
        }
    }
}

#[test]
fn opens_as_a_still() {
    let bytes = psd(RGB, 8, 4, 2, &rgb_planes(), true, &[]);
    assert!(crate::still::sniff("a.psd", &bytes));
    assert!(crate::is_importable(std::path::Path::new("Poster.PSD")));
    let src = crate::open_bytes("a.psd", bytes.into(), &[]).unwrap();
    assert_eq!(src.info().kind, crate::MediaKind::Still);
    assert_eq!(src.info().video.as_ref().map(|v| (v.width, v.height, v.codec.as_str())), Some((4, 2, "PSD")));
}
