use super::{CapabilitySnapshot, MAX_SNAPSHOT_BYTES};
use crate::{MediaError, Result};
use std::io::{self, Write};

#[derive(Default)]
struct LimitedBytes(Vec<u8>);

impl Write for LimitedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let size = self.0.len().checked_add(bytes.len()).ok_or_else(|| io::Error::other("capability JSON size overflow"))?;
        if size > MAX_SNAPSHOT_BYTES {
            return Err(io::Error::other("capability JSON exceeds 8 MiB"));
        }
        self.0.try_reserve(bytes.len()).map_err(|_| io::Error::other("capability JSON allocation failed"))?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn encode_snapshot(snapshot: &CapabilitySnapshot) -> Result<Vec<u8>> {
    snapshot.validate()?;
    let mut bytes = LimitedBytes::default();
    serde_json::to_writer(&mut bytes, snapshot).map_err(|e| MediaError::Unsupported(format!("encode media capabilities: {e}")))?;
    Ok(bytes.0)
}

pub fn decode_snapshot(bytes: &[u8]) -> Result<CapabilitySnapshot> {
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(MediaError::Unsupported("capability JSON exceeds 8 MiB".into()));
    }
    let snapshot: CapabilitySnapshot = serde_json::from_slice(bytes)
        .map_err(|e| MediaError::Unsupported(format!("media capability JSON {:?} at line {} column {}", e.classify(), e.line(), e.column())))?;
    snapshot.validate()?;
    Ok(snapshot)
}
