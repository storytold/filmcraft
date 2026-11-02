//! Bounded input obligations in hardware mode; consumed once into software replay deduplication.

use std::collections::{BTreeMap, VecDeque};

use filmcraft_codecs::hw::SampleOutput;
use filmcraft_codecs::{CodecError, DecodedFrame, Result};

// Replay resource policy: retain the existing 600-input / 256-MiB / 4096-picture bounds.
pub(super) const MAX_INPUTS: usize = 600;
pub(super) const MAX_BYTES: usize = 256 << 20;
pub(super) const MAX_OUTPUTS: usize = 4096;

pub(super) struct Input {
    pub data: Vec<u8>,
    pub pts: i64,
    expected: Option<usize>,
    delivered: usize,
    restart: bool,
    submitted: bool,
}

#[derive(Default)]
pub(super) struct Ledger {
    pub inputs: Vec<Input>,
    pub bytes: usize,
    pub retire_before: usize,
    last_published: Option<i64>,
}

impl Ledger {
    pub fn pending(&self) -> bool {
        self.inputs.iter().any(|input| input.expected.is_some_and(|n| n > input.delivered))
    }

    pub fn ambiguous_pts(&self, pts: i64) -> bool {
        self.last_published.is_some_and(|last| pts <= last) || self.inputs.iter().any(|input| input.pts == pts && input.expected.is_some_and(|n| n != 0))
    }

    pub fn remember(&mut self, sample: &[u8], pts: i64, plan: Option<SampleOutput>) -> bool {
        if sample.len() > MAX_BYTES {
            return false;
        }
        let restart = plan.is_some_and(|plan| plan.restart);
        if restart {
            self.retire_before = if plan.is_some_and(|plan| plan.keep_previous_restart) {
                self.inputs.iter().rposition(|input| input.restart).unwrap_or(0)
            } else {
                self.inputs.len()
            };
        }
        self.retire();
        let bytes = self.bytes.saturating_add(sample.len());
        if self.inputs.len() >= MAX_INPUTS || bytes > MAX_BYTES {
            return false;
        }
        self.inputs.push(Input { data: sample.to_vec(), pts, expected: plan.map(|plan| plan.pictures), delivered: 0, restart, submitted: false });
        self.bytes = bytes;
        true
    }

    pub fn submit(&mut self) {
        if let Some(input) = self.inputs.last_mut() {
            input.submitted = true;
        }
    }

    fn pending_at(&self, pts: i64) -> usize {
        self.inputs.iter().filter(|input| input.pts == pts).map(|input| input.expected.unwrap_or(0).saturating_sub(input.delivered)).sum()
    }

    /// Atomic publication gate: a later output cannot stand in for an earlier required picture.
    pub fn publish(&mut self, frames: &[DecodedFrame], complete: bool) -> bool {
        if frames.len() > MAX_OUTPUTS || frames.windows(2).any(|pair| pair[0].pts > pair[1].pts) {
            return false;
        }
        if frames.first().is_some_and(|frame| self.last_published.is_some_and(|last| frame.pts < last)) {
            return false;
        }
        let mut counts = BTreeMap::<i64, usize>::new();
        for frame in frames {
            *counts.entry(frame.pts).or_default() += 1;
        }
        if counts.iter().any(|(&pts, &count)| count > self.pending_at(pts)) {
            return false;
        }
        let frontier = frames.last().map(|frame| frame.pts);
        if self.inputs.iter().any(|input| {
            input.expected.is_some_and(|expected| expected > input.delivered)
                && (complete || frontier.is_some_and(|pts| input.pts <= pts))
                && counts.get(&input.pts).copied().unwrap_or(0) < self.pending_at(input.pts)
        }) {
            return false;
        }
        for frame in frames {
            if let Some(input) = self.inputs.iter_mut().find(|input| input.pts == frame.pts && input.expected.is_some_and(|n| n > input.delivered)) {
                input.delivered += 1;
            }
        }
        if let Some(frame) = frames.last() {
            self.last_published = Some(frame.pts);
        }
        self.retire();
        true
    }

    pub fn retire(&mut self) {
        let end = self.retire_before.min(self.inputs.len());
        if end != 0 && self.inputs[..end].iter().all(|input| input.expected == Some(input.delivered)) {
            self.inputs.drain(..end);
            self.bytes = self.inputs.iter().map(|input| input.data.len()).sum();
            self.retire_before = 0;
        }
    }

    pub fn into_replay(self, drained: Vec<DecodedFrame>) -> Result<(Vec<Input>, ReplayTail)> {
        if drained.len() > MAX_OUTPUTS {
            return Err(too_many_frames());
        }
        let mut skip = BTreeMap::<i64, usize>::new();
        let mut available = BTreeMap::<i64, usize>::new();
        for input in &self.inputs {
            if input.delivered != 0 {
                *skip.entry(input.pts).or_default() += input.delivered;
            }
            if input.submitted {
                let pending = input.expected.unwrap_or(0).saturating_sub(input.delivered);
                if pending != 0 {
                    *available.entry(input.pts).or_default() += pending;
                }
            }
        }
        let mut tail = ReplayTail { skip, ready: BTreeMap::new(), last_published: self.last_published };
        let mut already_delivered = tail.skip.clone();
        for frame in drained {
            if consume(&mut already_delivered, frame.pts) {
                continue;
            }
            if consume(&mut available, frame.pts) {
                tail.ready.entry(frame.pts).or_default().push_back(frame);
            }
        }
        Ok((self.inputs, tail))
    }
}

#[derive(Default)]
pub(super) struct ReplayTail {
    skip: BTreeMap<i64, usize>,
    ready: BTreeMap<i64, VecDeque<DecodedFrame>>,
    last_published: Option<i64>,
}

fn consume(counts: &mut BTreeMap<i64, usize>, pts: i64) -> bool {
    let Some(count) = counts.get_mut(&pts) else { return false };
    *count -= 1;
    if *count == 0 {
        counts.remove(&pts);
    }
    true
}

fn too_many_frames() -> CodecError {
    CodecError::Decode("software fallback replay produced too many frames".into())
}

pub(super) fn retain(frames: &mut Vec<DecodedFrame>, mut out: Vec<DecodedFrame>) -> Result<()> {
    if frames.len().saturating_add(out.len()) > MAX_OUTPUTS {
        return Err(too_many_frames());
    }
    frames.append(&mut out);
    Ok(())
}

impl ReplayTail {
    /// Preserve accepted replay output when the last replayed input itself is rejected.
    pub fn stage(&mut self, frames: Vec<DecodedFrame>) -> Result<()> {
        let mut accepted = BTreeMap::<i64, VecDeque<DecodedFrame>>::new();
        for frame in frames {
            if !consume(&mut self.skip, frame.pts) {
                accepted.entry(frame.pts).or_default().push_back(frame);
            }
        }
        for (pts, frames) in accepted {
            let current = self.ready.values().map(VecDeque::len).sum::<usize>();
            let replaced = self.ready.get(&pts).map_or(0, VecDeque::len);
            if current.saturating_sub(replaced).saturating_add(frames.len()) > MAX_OUTPUTS {
                return Err(too_many_frames());
            }
            self.ready.insert(pts, frames);
        }
        Ok(())
    }

    fn check_order(&mut self, frames: &[DecodedFrame]) -> Result<()> {
        if frames.windows(2).any(|pair| pair[0].pts > pair[1].pts)
            || frames.first().is_some_and(|frame| self.last_published.is_some_and(|last| frame.pts < last))
        {
            return Err(CodecError::Decode("decoder recovery returned a picture older than published output".into()));
        }
        if let Some(frame) = frames.last() {
            self.last_published = Some(frame.pts);
        }
        Ok(())
    }

    pub fn merge(&mut self, frames: Vec<DecodedFrame>) -> Result<Vec<DecodedFrame>> {
        let mut out = Vec::new();
        for frame in frames {
            if consume(&mut self.skip, frame.pts) {
                continue;
            }
            while let Some(entry) = self.ready.first_entry() {
                if *entry.key() >= frame.pts {
                    break;
                }
                retain(&mut out, entry.remove().into())?;
            }
            if let Some(ready) = self.ready.get_mut(&frame.pts) {
                ready.pop_front();
                if ready.is_empty() {
                    self.ready.remove(&frame.pts);
                }
            }
            if out.len() >= MAX_OUTPUTS {
                return Err(too_many_frames());
            }
            out.push(frame);
        }
        self.check_order(&out)?;
        Ok(out)
    }

    pub fn finish(&mut self, frames: Vec<DecodedFrame>) -> Result<Vec<DecodedFrame>> {
        let mut out = self.merge(frames)?;
        let mut remaining = Vec::new();
        for frames in std::mem::take(&mut self.ready).into_values() {
            retain(&mut remaining, frames.into())?;
        }
        self.check_order(&remaining)?;
        retain(&mut out, remaining)?;
        self.skip.clear();
        Ok(out)
    }
}
