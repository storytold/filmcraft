use super::*;
use serde_json::{Value, json};
fn reference(n: u32) -> Value {
    if n == 0 { Value::Null } else { json!({"ref":n}) }
}
fn text_value(t: &Option<Text<'_>>) -> Value {
    t.as_ref().map_or(Value::Null, |t| json!(t.value))
}
fn identity(id: &Option<[u8; 32]>) -> Value {
    id.map_or(Value::Null, |mut id| {
        id[16..20].reverse();
        id[20..22].reverse();
        id[22..24].reverse();
        json!({"id_le":id.iter().map(|b|format!("{b:02x}")).collect::<String>()})
    })
}
fn common(c: &Component<'_>) -> Value {
    let (n, d) = c.rate.rational().unwrap();
    let mut value = json!({"left_bob":reference(c.left),"right_bob":reference(c.right),"media_kind_id":c.media_kind,
        "edit_rate":n as f64/d as f64,"name":text_value(&c.name),"effect_id":text_value(&c.effect),
        "attributes":reference(c.attributes),"session_attrs":reference(c.session_attributes),"precomputed":reference(c.precomputed)});
    if let Some(r) = c.parameters {
        value["param_list"] = reference(r);
    }
    value
}
fn field(v: &Field<'_>) -> Value {
    match v {
        Field::Integer(v) => json!(v),
        Field::Boolean(v) => json!(v),
        Field::Ratio(v) => json!(v),
        Field::Float(v) => json!(v),
        Field::Reference(v) => reference(*v),
        Field::Bytes(v) => json!({"hex":v.iter().map(|b|format!("{b:02x}")).collect::<String>()}),
    }
}
fn track(t: &Track) -> Value {
    let mut v = json!({});
    if let Some(i) = t.index {
        v["index"] = json!(i);
    }
    for (key, r) in [
        ("attributes", t.attributes),
        ("session_attr", t.session_attributes),
        ("component", t.component),
        ("filler_proxy", t.filler_proxy),
        ("bob_data", t.bob_data),
    ] {
        if let Some(r) = r {
            v[key] = reference(r);
        }
    }
    if let Some(i) = t.start {
        v["start_pos"] = json!(i);
    }
    if let Some(i) = t.lock {
        v["lock_number"] = json!(i);
    }
    v
}
fn oracle(o: &Decoded<'_>) -> Value {
    match o {
        Decoded::Group(g) => {
            let mut v = common(&g.common);
            v["mc_mode"] = json!(g.mode);
            v["length"] = json!(g.length);
            v["num_scalars"] = json!(0);
            v["tracks"] = json!(g.tracks.iter().map(track).collect::<Vec<_>>());
            for (key, f) in &g.fields {
                v[*key] = field(f);
            }
            v
        }
        Decoded::Auxiliary { common: c, length, fields } => {
            let mut v = common(c);
            v["length"] = json!(length);
            for (key, f) in fields {
                v[*key] = field(f);
            }
            v
        }
        Decoded::Locator(l) => {
            let mut v = json!({});
            for (key, text) in [
                ("path", &l.path),
                ("path_posix", &l.posix),
                ("path_utf8", &l.utf8),
                ("last_known_volume", &l.volume),
                ("last_known_volume_utf8", &l.volume_utf8),
            ] {
                if text.is_some() {
                    v[key] = text_value(text);
                }
            }
            if l.id.is_some() {
                v["mob_id"] = identity(&l.id);
            }
            if let Some(domain) = l.domain {
                v["domain_type"] = json!(domain);
            }
            v
        }
        Decoded::BinReference(b) => {
            let mut v = json!({"uid_high":b.high,"uid_low":b.low,"name":text_value(&b.name)});
            if b.utf8.is_some() {
                v["name_utf8"] = text_value(&b.utf8);
            }
            v
        }
        Decoded::Descriptor(d) => {
            let mut v =
                json!({"mob_kind":d.mob_kind,"locator":reference(d.locator),"intermediate":d.intermediate,"physical_media":reference(d.physical_media)});
            if let Some(mut uuid) = d.uuid {
                uuid[..4].reverse();
                uuid[4..6].reverse();
                uuid[6..8].reverse();
                v["uuid"] = json!({"id_le":uuid.iter().map(|b|format!("{b:02x}")).collect::<String>()});
            }
            if let Some(raw) = d.plugin_raw {
                v["wchar"] = json!({"hex":raw.iter().map(|b|format!("{b:02x}")).collect::<String>()});
            }
            if let Some(r) = d.attributes {
                v["attributes"] = reference(r);
            }
            if let Some(f) = &d.file {
                let (n, den) = f.rate.rational().unwrap();
                v["edit_rate"] = json!(n as f64 / den as f64);
                v["length"] = json!(f.length);
                v["is_omfi"] = json!(f.omfi);
                v["data_offset"] = json!(f.data_offset);
            }
            if let Some(p) = &d.picture {
                for (key, value) in &p.integers {
                    v[*key] = json!(value);
                }
                for (key, value) in &p.booleans {
                    v[*key] = json!(value);
                }
                for (key, value) in &p.labels {
                    v[*key] = json!({"id_le":value.iter().map(|b|format!("{b:02x}")).collect::<String>()});
                }
                v["aspect_ratio"] = json!(p.aspect);
                v["line_map"] = json!(p.line_map);
                v["next_did_desc"] = reference(p.next);
                v["compress_method"] = json!({"hex":p.compression.iter().map(|b|format!("{b:02x}")).collect::<String>()});
                v["frame_index_offset"] = json!(p.frame_index_offset);
                if let Some(boxes) = p.boxes {
                    for (key, rectangle) in ["valid_box", "essence_box", "source_box"].into_iter().zip(boxes) {
                        v[key] = json!(rectangle);
                    }
                }
            }
            if let Some(p) = &d.pcm {
                let (n, den) = p.rate.rational().unwrap();
                let fields = json!({"channels":p.channels,"quantization_bits":p.bits,"sample_rate":n as f64/den as f64,"locked":p.locked,
                    "audio_ref_level":p.reference_level,"electro_spatial_formulation":p.spatial_formulation,"dial_norm":p.dial_norm,
                    "coding_format":p.coding_format,"block_align":p.block_align,"sequence_offset":p.sequence_offset,"average_bps":p.average_bps,
                    "has_peak_envelope_data":p.peak_present,"peak_envelope_version":p.peak_version,"peak_envelope_format":p.peak_format,
                    "points_per_peak_value":p.points_per_peak,"peak_envelope_block_size":p.peak_block_size,"peak_channel_count":p.peak_channels,
                    "peak_frame_count":p.peak_frames,"peak_of_peaks_offset":p.peak_offset,"peak_envelope_timestamp":p.peak_timestamp_raw});
                v.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
            }
            v
        }
        Decoded::Attributes { entries } => {
            let mut v = json!({});
            for entry in entries {
                v[entry.name.value.as_ref()] = match &entry.value {
                    AttributeValue::Integer(i) => json!(i),
                    AttributeValue::Text(t) => text_value(t),
                    AttributeValue::Reference(r) => reference(*r),
                    AttributeValue::Bytes(b) => json!({"hex":b.iter().map(|b|format!("{b:02x}")).collect::<String>()}),
                };
            }
            v
        }
        Decoded::Mob(m) => {
            let mut v = common(&m.common);
            v["mc_mode"] = json!(m.mode);
            v["length"] = json!(m.length);
            v["num_scalars"] = json!(0);
            v["last_modified"] = json!({"timestamp":m.modified_raw});
            if let Some(t) = m.created_raw {
                v["creation_time"] = json!({"timestamp":t});
            }
            v["mob_type_id"] = json!(m.kind);
            v["usage_code"] = json!(m.usage);
            v["descriptor"] = reference(m.descriptor);
            if m.id.is_some() {
                v["mob_id"] = identity(&m.id);
            }
            v["tracks"] = json!(
                m.tracks
                    .iter()
                    .map(|t| {
                        let mut v = json!({});
                        if let Some(i) = t.index {
                            v["index"] = json!(i);
                        }
                        for (k, r) in [
                            ("attributes", t.attributes),
                            ("session_attr", t.session_attributes),
                            ("component", t.component),
                            ("filler_proxy", t.filler_proxy),
                            ("bob_data", t.bob_data),
                        ] {
                            if let Some(r) = r {
                                v[k] = reference(r);
                            }
                        }
                        if let Some(i) = t.start {
                            v["start_pos"] = json!(i);
                        }
                        if let Some(i) = t.lock {
                            v["lock_number"] = json!(i);
                        }
                        v
                    })
                    .collect::<Vec<_>>()
            );
            v
        }
        Decoded::Sequence { common: c, components } => {
            let mut v = common(c);
            v["components"] = json!(components.iter().copied().map(reference).collect::<Vec<_>>());
            v
        }
        Decoded::SourceClip { common: c, length, track, start, id, .. } => {
            let mut v = common(c);
            v["length"] = json!(length);
            v["track_id"] = json!(track);
            v["start_time"] = json!(start);
            if id.is_some() {
                v["mob_id"] = identity(id);
            }
            v
        }
        Decoded::Filler { common: c, length } => {
            let mut v = common(c);
            v["length"] = json!(length);
            v
        }
        Decoded::Timecode { common: c, length, flags, fps, start } => {
            let mut v = common(c);
            v["length"] = json!(length);
            v["flags"] = json!(flags);
            v["fps"] = json!(fps);
            v["start"] = json!(start);
            v
        }
        // A selector is a track group, and is exported as one.
        Decoded::Selector { common: c, mode, length, tracks, selection_raw, trailing_raw } => {
            let mut v = common(c);
            v["mc_mode"] = json!(mode);
            v["length"] = json!(length);
            v["num_scalars"] = json!(0);
            v["tracks"] = json!(tracks.iter().map(track).collect::<Vec<_>>());
            v["selected_raw"] = json!(selection_raw);
            v["trailing_raw"] = json!(trailing_raw);
            v
        }
    }
}
#[test]
#[ignore = "set FILMCRAFT_AVB_SEMANTIC_ORACLE to a private black-box property export"]
fn semantic_corpus_matches_every_supported_property() {
    let files: Value = serde_json::from_slice(&std::fs::read(std::env::var("FILMCRAFT_AVB_SEMANTIC_ORACLE").unwrap()).unwrap()).unwrap();
    let mut count = 0;
    for file in files.as_array().unwrap() {
        let bytes = std::fs::read(file["path"].as_str().unwrap()).unwrap();
        let bin = index(&bytes).unwrap();
        for o in file["objects"].as_array().unwrap() {
            let id = o["index"].as_u64().unwrap() as u32;
            let decoded = bin.decode(id).unwrap_or_else(|e| panic!("{} object {id}: {e}", file["path"]));
            assert_eq!(oracle(&decoded), o["properties"], "{} object {id}", file["path"]);
            count += 1;
        }
    }
    eprintln!("{count} semantic objects matched every exported property");
}

fn bin<'a>(class: [u8; 4], payload: &'a [u8], order: ByteOrder) -> Bin<'a> {
    Bin {
        byte_order: order,
        root_index: 1,
        saved_at_raw: &[],
        creator_raw: &[],
        save_fields_raw: &[],
        reserved_raw: &[],
        objects: vec![Object { index: 1, class, offset: 0, payload }],
    }
}
fn attributes() -> Vec<u8> {
    // Entirely synthetic: integer, padded MacRoman text and a self-reference (no recursion).
    let mut b = vec![2, 1];
    b.extend_from_slice(&3u32.to_le_bytes());
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&[1, 0, b'I']);
    b.extend_from_slice(&(-12i32).to_le_bytes());
    b.extend_from_slice(&2u32.to_le_bytes());
    b.extend_from_slice(&[1, 0, b'T']);
    b.extend_from_slice(&[5, 0, b'c', b'a', b'f', 0x8e, 0]);
    b.extend_from_slice(&3u32.to_le_bytes());
    b.extend_from_slice(&[1, 0, b'R']);
    b.extend_from_slice(&1u32.to_le_bytes());
    b.push(3);
    b
}
#[test]
fn raw_text_attributes_decimal_rates_and_cycles() {
    let b = attributes();
    let bin = bin(*b"ATTR", &b, ByteOrder::Little);
    let d = bin.decode(1).unwrap();
    assert_eq!(oracle(&d), json!({"I":-12,"T":"café","R":{"ref":1}}));
    let Decoded::Attributes { entries } = d else { panic!() };
    let AttributeValue::Text(Some(t)) = &entries[1].value else { panic!() };
    assert_eq!(t.raw, b"caf\x8e\0");
    assert_eq!(DecimalRate { mantissa: 2997, exponent: -2 }.rational().unwrap(), (2997, 100));
    for r in [DecimalRate { mantissa: 0, exponent: 0 }, DecimalRate { mantissa: 1, exponent: i16::MIN }, DecimalRate { mantissa: i32::MIN, exponent: 9 }] {
        assert!(r.rational().is_err());
    }
}
#[test]
fn semantic_mutations_are_bounded_and_unknown_data_is_explicit() {
    let b = attributes();
    for end in 0..b.len() {
        assert!(bin(*b"ATTR", &b[..end], ByteOrder::Little).decode(1).is_err());
    }
    for at in 0..b.len() {
        for bit in 0..8 {
            let mut bytes = b.clone();
            bytes[at] ^= 1 << bit;
            std::panic::catch_unwind(|| {
                let _ = bin(*b"ATTR", &bytes, ByteOrder::Little).decode(1);
            })
            .unwrap();
        }
    }
    let mut bad = b.clone();
    bad[6] = 99;
    assert!(bin(*b"ATTR", &bad, ByteOrder::Little).decode(1).unwrap_err().to_string().contains("attribute kind"));
    let mut duplicate = b.clone();
    duplicate[23] = b'I';
    assert!(bin(*b"ATTR", &duplicate, ByteOrder::Little).decode(1).is_err());
    assert!(bin(*b"Zzzz", &b, ByteOrder::Little).decode(1).is_err());
}

#[test]
#[ignore = "set FILMCRAFT_AVB_LOCATION_ORACLE to private black-box exports"]
fn location_corpus_matches_every_supported_property() {
    let files: Value = serde_json::from_slice(&std::fs::read(std::env::var("FILMCRAFT_AVB_LOCATION_ORACLE").unwrap()).unwrap()).unwrap();
    let mut count = 0;
    for file in files.as_array().unwrap() {
        let bytes = std::fs::read(file["path"].as_str().unwrap()).unwrap();
        let bin = index(&bytes).unwrap();
        for o in file["objects"].as_array().unwrap() {
            if !matches!(o["class"].as_str(), Some("FILE" | "MSML" | "MCBR" | "URLL" | "MDES" | "PCMA" | "CDCI" | "TKFX" | "SPED" | "MASK" | "ECCP" | "TRKR")) {
                continue;
            }
            let id = o["index"].as_u64().unwrap() as u32;
            let decoded = bin.decode(id).unwrap_or_else(|e| panic!("{} object {id}: {e}", file["path"]));
            assert_eq!(oracle(&decoded), o["properties"], "{} object {id}", file["path"]);
            count += 1;
        }
    }
    eprintln!("{count} location/descriptor objects match every exported property");
}

#[test]
fn a_windows_source_locator_carries_both_forms_of_its_path_and_nothing_assumed() {
    // WINF records where a bin's media was originally imported from, in FILE's path shape. Every
    // one observed in the corpus carries a MacRoman path and a UTF-8 form of it, and no other
    // field, so FILE's POSIX and alternate extensions must stay refused here rather than guessed.
    for order in [ByteOrder::Little, ByteOrder::Big] {
        let text = "/Volumes/LIBRARY/SOUND EFFECTS/caf\u{00e9}.wav";
        let mac = b"/Volumes/LIBRARY/SOUND EFFECTS/caf\x8e.wav";
        let mut b = vec![2, 2];
        n(&mut b, mac.len() as u64, 2, order);
        b.extend_from_slice(mac);
        b.extend_from_slice(&[1, 2, 0x4c]);
        n(&mut b, (text.len() + 2) as u64, 2, order);
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(text.as_bytes());
        b.push(3);
        let Decoded::Locator(l) = bin(*b"WINF", &b, order).decode(1).unwrap() else { panic!("WINF must decode as a locator") };
        assert_eq!(l.path.unwrap().value, "/Volumes/LIBRARY/SOUND EFFECTS/caf\u{00e9}.wav");
        assert_eq!(l.utf8.unwrap().value, text);
        assert!(l.posix.is_none() && l.alternate_utf8.is_none() && l.id.is_none() && l.volume.is_none());
        // An extension FILE supports but WINF has never been seen to use is not assumed.
        let mut unobserved = b.clone();
        unobserved.pop();
        unobserved.extend_from_slice(&[1, 3, 0x4c]);
        n(&mut unobserved, 2, 2, order);
        unobserved.extend_from_slice(&[0, 0, 3]);
        assert!(bin(*b"WINF", &unobserved, order).decode(1).is_err());
        // Truncation and single-bit damage stay errors, never panics.
        for end in 0..b.len() {
            assert!(std::panic::catch_unwind(|| bin(*b"WINF", &b[..end], order).decode(1)).unwrap().is_err());
        }
        for at in 0..b.len() {
            for bit in 0..8 {
                let mut mutant = b.clone();
                mutant[at] ^= 1 << bit;
                std::panic::catch_unwind(|| {
                    let _ = bin(*b"WINF", &mutant, order).decode(1);
                })
                .unwrap();
            }
        }
    }
}
#[test]
fn a_group_clip_selector_reads_its_alternatives_as_the_track_group_it_is() {
    // A selector is a track group: each track is one alternative this track of the group clip can
    // play, one per angle carrying its media kind. Which alternative is active is not established,
    // so the trailing fields are preserved rather than interpreted.
    for order in [ByteOrder::Little, ByteOrder::Big] {
        let mut b = synthetic_common(order);
        b.extend_from_slice(&[2, 8]); // a track group
        b.push(0); // mode
        n(&mut b, 530, 4, order); // length
        n(&mut b, 0, 4, order); // scalar count, always zero
        n(&mut b, 2, 4, order); // two alternatives
        for component in [1u64, 0] {
            n(&mut b, 4, 2, order); // flags: a component and nothing else
            n(&mut b, component, 4, order);
        }
        b.extend_from_slice(&[1, 1]); // extension: a lock per alternative
        for _ in 0..2 {
            b.push(0x45);
            n(&mut b, 0, 2, order);
        }
        b.extend_from_slice(&[2, 1]);
        b.push(1); // the only field that could name the active angle
        n(&mut b, 0, 2, order);
        b.push(3);
        let Decoded::Selector { mode, length, tracks, selection_raw, trailing_raw, .. } = bin(*b"SLCT", &b, order).decode(1).unwrap() else {
            panic!("a selector must decode as one")
        };
        assert_eq!((mode, length, selection_raw, trailing_raw), (0, 530, 1, 0));
        assert_eq!(tracks.len(), 2, "one track per alternative");
        assert_eq!(tracks[0].component, Some(1));
        // The flag says a component field is present; its reference being null is separate.
        assert_eq!(tracks[1].component, Some(0), "a present but null alternative reference");
        // Truncation and single-bit damage stay errors, never panics.
        for end in 0..b.len() {
            assert!(std::panic::catch_unwind(|| bin(*b"SLCT", &b[..end], order).decode(1)).unwrap().is_err());
        }
        for at in 0..b.len() {
            for bit in 0..8 {
                let mut mutant = b.clone();
                mutant[at] ^= 1 << bit;
                std::panic::catch_unwind(|| {
                    let _ = bin(*b"SLCT", &mutant, order).decode(1);
                })
                .unwrap();
            }
        }
    }
}
fn n(out: &mut Vec<u8>, value: u64, width: usize, order: ByteOrder) {
    match order {
        ByteOrder::Little => out.extend_from_slice(&value.to_le_bytes()[..width]),
        ByteOrder::Big => out.extend_from_slice(&value.to_be_bytes()[8 - width..]),
    }
}
fn synthetic_common(order: ByteOrder) -> Vec<u8> {
    let mut b = vec![2, 3];
    b.extend_from_slice(&[0; 8]);
    n(&mut b, 3, 2, order);
    n(&mut b, 24, 4, order);
    n(&mut b, 0, 2, order);
    b.extend_from_slice(&[255; 4]);
    b.extend_from_slice(&[0; 12]);
    b
}
#[test]
fn endian_id_utf8_and_high_word_timecode_are_independent() {
    for order in [ByteOrder::Little, ByteOrder::Big] {
        let mut b = vec![2, 2];
        b.extend_from_slice(&[0; 8]);
        b.extend_from_slice(&[255; 2]);
        b.extend_from_slice(&[1, 2, 0x41]);
        n(&mut b, 12, 4, order);
        b.extend_from_slice(&[6; 12]);
        for v in [19, 0, 0, 0] {
            b.extend_from_slice(&[0x44, v]);
        }
        b.push(0x48);
        n(&mut b, 0x12345678, 4, order);
        b.push(0x46);
        n(&mut b, 0x9abc, 2, order);
        b.push(0x46);
        n(&mut b, 0xdef0, 2, order);
        b.push(0x41);
        n(&mut b, 8, 4, order);
        b.extend_from_slice(&[0x87; 8]);
        b.push(3);
        let Decoded::Locator(l) = bin(*b"MSML", &b, order).decode(1).unwrap() else { panic!() };
        assert_eq!(&l.id.unwrap()[16..24], &[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0]);
        let mut t = synthetic_common(order);
        t.extend_from_slice(&[2, 1]);
        n(&mut t, 100, 4, order);
        t.extend_from_slice(&[2, 1]);
        n(&mut t, 0, 4, order);
        n(&mut t, 24, 4, order);
        n(&mut t, u32::MAX as u64, 4, order);
        n(&mut t, 0x87654321, 4, order);
        t.push(3);
        let Decoded::Timecode { start, .. } = bin(*b"TCCP", &t, order).decode(1).unwrap() else { panic!() };
        assert_eq!(start, -2023406815);
        let mut f = vec![2, 2, 255, 255, 1, 2, 0x4c];
        let text = "/媒体/café.mov";
        n(&mut f, (text.len() + 2) as u64, 2, order);
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(text.as_bytes());
        f.extend_from_slice(&[1, 3, 0x4c]);
        let alternate = "\\\\server\\媒体\\café.mov";
        n(&mut f, (alternate.len() + 2) as u64, 2, order);
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(alternate.as_bytes());
        f.push(3);
        let Decoded::Locator(l) = bin(*b"FILE", &f, order).decode(1).unwrap() else { panic!() };
        assert_eq!(l.utf8.unwrap().value, text);
        assert_eq!(l.alternate_utf8.unwrap().value, alternate);
        for (class, bytes) in [(*b"FILE", f), (*b"MSML", b), (*b"TCCP", t)] {
            for end in 0..bytes.len() {
                assert!(std::panic::catch_unwind(|| bin(class, &bytes[..end], order).decode(1)).unwrap().is_err());
            }
            for at in 0..bytes.len() {
                for bit in 0..8 {
                    let mut mutant = bytes.clone();
                    mutant[at] ^= 1 << bit;
                    std::panic::catch_unwind(|| {
                        let _ = bin(class, &mutant, order).decode(1);
                    })
                    .unwrap();
                }
            }
        }
    }
}
