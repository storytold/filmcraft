//! Native AVB inspection only. Does not create project items or imply semantic import coverage.
use crate::{EngineError, Result, Session};
use filmcraft_interchange::avid_bin::{self, ByteOrder};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn inspect(s: &Session, p: &Value) -> Result<Value> {
    // Same capped snapshot read and size-change checks as the catalog commands.
    let bytes = crate::avid_catalog::read_report(s, p)?;
    let index = avid_bin::index(&bytes).map_err(|e| EngineError::Other(e.to_string()))?;
    let mut classes = BTreeMap::<[u8; 4], usize>::new();
    for object in &index.objects {
        *classes.entry(object.class).or_default() += 1;
    }
    let classes: Vec<_> = classes.into_iter().map(|(raw, count)| json!({"raw":raw,"name":std::str::from_utf8(&raw).ok(),"count":count})).collect();
    let mut diagnostics = vec![
        "Container and membership inspection only; mob identities, attributes, descriptors, timeline objects and root settings are not decoded.".to_string(),
    ];
    let membership = match index.membership() {
        Ok(bin) => json!({"uid":format!("{:016x}",bin.uid),"viewIndex":bin.view_index,"members":bin.members.len(),
            "userPlaced":bin.members.iter().filter(|m|m.user_placed).count(),"opaqueTailBytes":bin.tail_raw.len()}),
        Err(e) => {
            diagnostics.push(e.to_string());
            Value::Null
        }
    };
    Ok(
        json!({"coverage":if membership.is_null(){"container"}else{"container_and_membership"},"bytes":bytes.len(),"byteOrder":match index.byte_order{ByteOrder::Little=>"little",ByteOrder::Big=>"big"},
        "objects":index.objects.len(),"rootIndex":index.root_index,"classes":classes,"membership":membership,
        "creatorRaw":index.creator_raw,"diagnostics":diagnostics}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use std::sync::Arc;

    struct Input(Vec<u8>);
    impl Services for Input {
        fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
            Ok(self.0.clone())
        }
        fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
            panic!("inspection must not write");
        }
    }
    fn fixture() -> Vec<u8> {
        let mut bytes = b"\x06\x00DomainDJBO\x07\x00AObjDoc\x04\x00\x00".to_vec();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 38]); // opaque save fields, empty creator string, reserved bytes
        bytes.extend_from_slice(b"NIBA");
        bytes.extend_from_slice(&119u32.to_le_bytes());
        bytes.extend_from_slice(&[2, 0x0e]);
        bytes.extend_from_slice(&[0; 117]);
        bytes
    }

    #[test]
    fn inspection_and_hostile_parameters_never_edit_the_project() {
        let mut s = Session::new(Arc::new(Input(fixture())));
        let before = s.project.clone();
        let state = serde_json::to_value(&s.state).unwrap();
        let revision = s.revision;
        let output = s.execute("file.inspectAvidBin", json!({"path":"test.avb"})).unwrap();
        assert_eq!(output["objects"], 1);
        assert_eq!(output["membership"]["userPlaced"], 0);
        assert_eq!(output["coverage"], "container_and_membership");
        assert!(!output["diagnostics"].as_array().unwrap().is_empty());
        for p in [Value::Null, json!({"path":0}), json!({"path":""}), json!({"path":"\0"}), json!({"path":"x".repeat(4097)})] {
            assert!(s.execute("file.inspectAvidBin", p).is_err());
        }
        assert_eq!(s.project, before);
        assert_eq!(serde_json::to_value(&s.state).unwrap(), state);
        assert_eq!(s.revision, revision);
        assert!(s.history.undo.is_empty());
        assert!(s.history.redo.is_empty());
        let mut malformed = Session::new(Arc::new(Input(vec![0; 100])));
        assert!(malformed.execute("file.inspectAvidBin", json!({"path":"bad.avb"})).is_err());
        assert!(malformed.history.undo.is_empty());
        let mut unsupported = fixture();
        let version = unsupported.len() - 119 + 1;
        unsupported[version] = 0x0f;
        let mut future = Session::new(Arc::new(Input(unsupported)));
        let output = future.execute("file.inspectAvidBin", json!({"path":"future.avb"})).unwrap();
        assert_eq!(output["coverage"], "container");
        assert!(output["membership"].is_null());
        assert_eq!(output["diagnostics"].as_array().unwrap().len(), 2);
    }
}
