use filmcraft_av1enc::{Encoder, EncoderConfig, YuvFrame, coeff, frame_header, obu, symbol, tables};

#[test]
fn test_residual_decode_non_skip_block() {
    let (w, h) = (64u32, 64u32);
    let mut enc = Encoder::new(EncoderConfig::new(w, h, 30, 1)).unwrap();

    // Create a block with high constant value (e.g. 200) so there's a strong DC residual
    let y_plane = vec![200u8; (w * h) as usize];
    let uv_plane = vec![128u8; ((w / 2) * (h / 2)) as usize];
    let frame = YuvFrame { y: &y_plane, u: &uv_plane, v: &uv_plane, y_stride: w as usize, uv_stride: (w / 2) as usize };

    let packet = enc.encode(&frame, 0).unwrap();
    assert!(!packet.data.is_empty());

    let mut dec = filmcraft_av1::Decoder::new();
    let mut pics = dec.decode(&packet.data).unwrap();
    pics.extend(dec.flush());
    assert_eq!(pics.len(), 1);
    assert_eq!(pics[0].width, w);
    assert_eq!(pics[0].height, h);
    let decoded_val = pics[0].planes[0][0];
    println!("Decoded Y for constant 200 source: {decoded_val}");
    assert!(decoded_val > 180, "Decoded value should be close to 200 with non-skip DC residual, got {decoded_val}");
}

#[test]
fn test_raw_non_skip_residual_tile() {
    let (w, h) = (64u32, 64u32);
    let seq_cfg = obu::SequenceConfig { width: w, height: h, bit_depth: 8, mono_chrome: false, enable_cdef: false };
    let fh_cfg = frame_header::FrameHeaderConfig {
        frame_type: frame_header::KEY_FRAME,
        show_frame: true,
        error_resilient_mode: false,
        disable_cdf_update: true,
        qindex: 100, // Tier 2 (61..=120)
        order_hint: 0,
    };

    let mut tu = Vec::new();
    tu.extend_from_slice(&obu::write_temporal_delimiter());
    tu.extend_from_slice(&obu::write_sequence_header(&seq_cfg));
    tu.extend_from_slice(&obu::write_obu(obu::OBU_FRAME_HEADER, &frame_header::write_frame_header(&fh_cfg, &seq_cfg)));

    let mut sym = symbol::SymbolEncoder::new();
    // 1. Partition 64x64
    sym.write_symbol_no_update(0, &tables::DEFAULT_PARTITION_W64_CDF[0]);
    // 2. Skip = 0 (has residuals!)
    sym.write_symbol_no_update(0, &tables::DEFAULT_SKIP_CDF[0]);
    // 3. Y mode: DC_PRED (symbol 0)
    sym.write_symbol_no_update(0, &tables::DEFAULT_INTRA_FRAME_Y_MODE_CDF[0][0]);
    // 4. UV mode: DC_PRED (symbol 0)
    sym.write_symbol_no_update(0, &tables::DEFAULT_UV_MODE_CFL_NOT_ALLOWED_CDF[0]);

    // 5. Residuals for Y (plane 0, tx_sz = TX_64X64, ctx = 0):
    coeff::encode_dc_residual(&mut sym, 100, 0, 4, 0, 0, 3, false, 0);

    // 6. Residuals for U (plane 1, tx_sz = TX_32X32, ctx = 7):
    coeff::write_txb_skip(&mut sym, 100, 3, 7, true);

    // 7. Residuals for V (plane 2, tx_sz = TX_32X32, ctx = 7):
    coeff::write_txb_skip(&mut sym, 100, 3, 7, true);

    let tile_bytes = sym.finish();
    tu.extend_from_slice(&obu::write_obu(obu::OBU_TILE_GROUP, &tile_bytes));

    let mut dec = filmcraft_av1::Decoder::new();
    let mut pics = dec.decode(&tu).unwrap();
    pics.extend(dec.flush());
    assert_eq!(pics.len(), 1);
    assert_eq!(pics[0].width, w);
    assert_eq!(pics[0].height, h);

    // Verify that the Y plane decoded value is DIFFERENT from flat DC_PRED (128)
    // because positive DC residual was added!
    let decoded_y = pics[0].planes[0][0];
    println!("Decoded Y sample with positive DC residual: {decoded_y}");
    assert!(decoded_y > 128, "Positive residual must increase pixel value!");

    // Now test negative DC residual:
    let mut sym_neg = symbol::SymbolEncoder::new();
    sym_neg.write_symbol_no_update(0, &tables::DEFAULT_PARTITION_W64_CDF[0]);
    sym_neg.write_symbol_no_update(0, &tables::DEFAULT_SKIP_CDF[0]);
    sym_neg.write_symbol_no_update(0, &tables::DEFAULT_INTRA_FRAME_Y_MODE_CDF[0][0]);
    sym_neg.write_symbol_no_update(0, &tables::DEFAULT_UV_MODE_CFL_NOT_ALLOWED_CDF[0]);
    coeff::encode_dc_residual(&mut sym_neg, 100, 0, 4, 0, 0, 10, true, 0); // negative level 10!
    coeff::write_txb_skip(&mut sym_neg, 100, 3, 7, true);
    coeff::write_txb_skip(&mut sym_neg, 100, 3, 7, true);

    let tile_bytes_neg = sym_neg.finish();
    let mut tu_neg = Vec::new();
    tu_neg.extend_from_slice(&obu::write_temporal_delimiter());
    tu_neg.extend_from_slice(&obu::write_sequence_header(&seq_cfg));
    tu_neg.extend_from_slice(&obu::write_obu(obu::OBU_FRAME_HEADER, &frame_header::write_frame_header(&fh_cfg, &seq_cfg)));
    tu_neg.extend_from_slice(&obu::write_obu(obu::OBU_TILE_GROUP, &tile_bytes_neg));

    let mut dec_neg = filmcraft_av1::Decoder::new();
    let mut pics_neg = dec_neg.decode(&tu_neg).unwrap();
    pics_neg.extend(dec_neg.flush());
    assert_eq!(pics_neg.len(), 1);
    let decoded_y_neg = pics_neg[0].planes[0][0];
    println!("Decoded Y sample with negative DC residual: {decoded_y_neg}");
    assert!(decoded_y_neg < 128, "Negative residual must decrease pixel value!");
}

#[test]
fn test_split_partition_decode() {
    let (w, h) = (64usize, 64usize);
    let mut enc = Encoder::new(EncoderConfig::new(w as u32, h as u32, 24, 1)).unwrap();
    // 64x64 frame with top half dark (30) and bottom half bright (220)
    let mut y = vec![30u8; w * h];
    for row in 32..64 {
        for col in 0..64 {
            y[row * w + col] = 220;
        }
    }
    let uv = vec![128u8; (w / 2) * (h / 2)];
    let frame = YuvFrame { y: &y, u: &uv, v: &uv, y_stride: w, uv_stride: w / 2 };

    let p = enc.encode(&frame, 0).unwrap();
    let mut dec = filmcraft_av1::Decoder::new();
    let mut pics = dec.decode(&p.data).unwrap();
    pics.extend(dec.flush());
    assert_eq!(pics.len(), 1);
    assert_eq!(pics[0].width, 64);
    assert_eq!(pics[0].height, 64);
    let top_left = pics[0].planes[0][0];
    let bottom_left = pics[0].planes[0][32 * 64];
    println!("Top-left pixel (expected ~30): {top_left}");
    println!("Bottom-left pixel (expected ~220): {bottom_left}");
}
