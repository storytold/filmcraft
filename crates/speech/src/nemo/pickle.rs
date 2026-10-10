//! The subset of Python's pickle virtual machine that `torch.save(state_dict)` emits, enough to
//! recover each tensor's storage key, element type, storage offset, shape and strides. Nothing is
//! executed: the only callables understood are `collections.OrderedDict`,
//! `torch._utils._rebuild_tensor_v2` and `torch._utils._rebuild_parameter`; any other global
//! becomes an opaque value.
//!
//! Format reference: the `pickletools` documentation of the opcodes (protocols 0–5) and PyTorch's
//! description of its zip serialization format (`torch/serialization.py`, BSD-style licence).
//!
//! Hostile input: the stack, memo and value counts are capped, lengths are checked against the
//! data, and integers are range-checked before use.

use std::collections::HashMap;
use std::rc::Rc;

use crate::SpeechError;

/// Largest `data.pkl` accepted.
pub const MAX_PICKLE: u64 = 256 << 20;
const MAX_STACK: usize = 1 << 20;
const MAX_MEMO: usize = 1 << 22;
const MAX_DIMS: usize = 8;
/// Elements copied when a shared dict or list is modified (copy-on-write), summed over the run.
const MAX_COPY_WORK: usize = 64 << 20;

/// Element type of a tensor storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DType {
    F32,
    F16,
    BF16,
    F64,
    I64,
    I32,
    I16,
    I8,
    U8,
    Bool,
}

impl DType {
    pub fn size(self) -> usize {
        match self {
            DType::F64 | DType::I64 => 8,
            DType::F32 | DType::I32 => 4,
            DType::F16 | DType::BF16 | DType::I16 => 2,
            DType::I8 | DType::U8 | DType::Bool => 1,
        }
    }

    fn from_storage(name: &str) -> Option<Self> {
        Some(match name {
            "FloatStorage" => DType::F32,
            "HalfStorage" => DType::F16,
            "BFloat16Storage" => DType::BF16,
            "DoubleStorage" => DType::F64,
            "LongStorage" => DType::I64,
            "IntStorage" => DType::I32,
            "ShortStorage" => DType::I16,
            "CharStorage" => DType::I8,
            "ByteStorage" => DType::U8,
            "BoolStorage" => DType::Bool,
            _ => return None,
        })
    }
}

/// Where a tensor's data lives and how it is laid out (in elements).
#[derive(Clone, Debug, PartialEq)]
pub struct TensorInfo {
    /// Storage key: the file `data/<key>` of the checkpoint.
    pub storage: String,
    pub dtype: DType,
    /// Offset into the storage, in elements.
    pub offset: u64,
    pub shape: Vec<usize>,
    pub stride: Vec<usize>,
}

impl TensorInfo {
    pub fn numel(&self) -> Option<u64> {
        self.shape.iter().try_fold(1u64, |a, &d| a.checked_mul(d as u64))
    }

    /// Elements spanned from the first to one past the last element (strided layout).
    pub fn span_elements(&self) -> Option<u64> {
        if self.shape.contains(&0) {
            return Some(0);
        }
        let last = self.shape.iter().zip(&self.stride).try_fold(0u64, |a, (&d, &s)| a.checked_add((d as u64 - 1).checked_mul(s as u64)?))?;
        last.checked_add(1)
    }

    /// Row-major contiguous (size-1 dimensions may have any stride).
    pub fn is_contiguous(&self) -> bool {
        let mut expect = 1usize;
        for (&d, &s) in self.shape.iter().zip(&self.stride).rev() {
            if d != 1 && s != expect {
                return false;
            }
            expect = expect.saturating_mul(d);
        }
        true
    }
}

#[derive(Clone, Debug)]
enum V {
    None,
    Bool(bool),
    Int(i64),
    Str(Rc<str>),
    Tuple(Rc<Vec<V>>),
    List(Rc<Vec<V>>),
    Dict(Rc<Vec<(V, V)>>),
    Global(Rc<str>),
    Storage(Rc<(String, DType)>),
    Tensor(Rc<TensorInfo>),
    Opaque,
}

fn bad(msg: impl Into<String>) -> SpeechError {
    SpeechError::Model(format!("checkpoint pickle: {}", msg.into()))
}

struct Reader<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SpeechError> {
        let e = self.p.checked_add(n).filter(|&e| e <= self.d.len()).ok_or_else(|| bad("truncated"))?;
        let s = self.d.get(self.p..e).ok_or_else(|| bad("truncated"))?;
        self.p = e;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, SpeechError> {
        Ok(*self.take(1)?.first().ok_or_else(|| bad("truncated"))?)
    }
    fn u16(&mut self) -> Result<u16, SpeechError> {
        Ok(u16::from_le_bytes(*self.take(2)?.first_chunk::<2>().ok_or_else(|| bad("truncated"))?))
    }
    fn u32(&mut self) -> Result<u32, SpeechError> {
        Ok(u32::from_le_bytes(*self.take(4)?.first_chunk::<4>().ok_or_else(|| bad("truncated"))?))
    }
    fn u64(&mut self) -> Result<u64, SpeechError> {
        Ok(u64::from_le_bytes(*self.take(8)?.first_chunk::<8>().ok_or_else(|| bad("truncated"))?))
    }
    fn line(&mut self) -> Result<&'a str, SpeechError> {
        let rest = self.d.get(self.p..).unwrap_or_default();
        let n = rest.iter().take(4096).position(|&b| b == b'\n').ok_or_else(|| bad("unterminated line"))?;
        let s = self.take(n + 1)?;
        std::str::from_utf8(s.get(..n).unwrap_or_default()).map_err(|_| bad("non-UTF-8 global name"))
    }
    fn str(&mut self, n: usize) -> Result<Rc<str>, SpeechError> {
        Ok(Rc::from(String::from_utf8_lossy(self.take(n)?).as_ref()))
    }
}

fn int(v: &V) -> Option<i64> {
    match v {
        V::Int(i) => Some(*i),
        V::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

fn dims(v: &V) -> Option<Vec<usize>> {
    let V::Tuple(t) = v else { return None };
    if t.len() > MAX_DIMS {
        return None;
    }
    t.iter().map(|x| int(x).and_then(|i| usize::try_from(i).ok())).collect()
}

/// Apply `callable` to `args` (REDUCE / NEWOBJ).
fn reduce(callable: &V, args: &V) -> V {
    let (V::Global(g), V::Tuple(a)) = (callable, args) else { return V::Opaque };
    match g.as_ref() {
        "collections.OrderedDict" | "builtins.dict" if a.is_empty() => V::Dict(Rc::new(Vec::new())),
        "torch._utils._rebuild_tensor_v2" | "torch._utils._rebuild_tensor" => {
            let (Some(V::Storage(s)), Some(off), Some(shape), Some(stride)) =
                (a.first(), a.get(1).and_then(int), a.get(2).and_then(dims), a.get(3).and_then(dims))
            else {
                return V::Opaque;
            };
            if shape.len() != stride.len() || off < 0 {
                return V::Opaque;
            }
            V::Tensor(Rc::new(TensorInfo { storage: s.0.clone(), dtype: s.1, offset: off as u64, shape, stride }))
        }
        "torch._utils._rebuild_parameter" | "torch._utils._rebuild_parameter_with_state" => match a.first() {
            Some(t @ V::Tensor(_)) => t.clone(),
            _ => V::Opaque,
        },
        _ => V::Opaque,
    }
}

/// `BINPERSID`: `('storage', <storage class>, key, location, numel)`.
fn persistent(pid: &V) -> V {
    let V::Tuple(t) = pid else { return V::Opaque };
    match (t.first(), t.get(1), t.get(2)) {
        (Some(V::Str(kind)), Some(V::Global(cls)), Some(V::Str(key))) if kind.as_ref() == "storage" => {
            let name = cls.rsplit('.').next().unwrap_or_default();
            match DType::from_storage(name) {
                Some(dt) => V::Storage(Rc::new((key.to_string(), dt))),
                None => V::Opaque,
            }
        }
        _ => V::Opaque,
    }
}

/// Run the pickle and return its value.
fn run(data: &[u8]) -> Result<V, SpeechError> {
    if data.len() as u64 > MAX_PICKLE {
        return Err(bad("too large"));
    }
    let mut r = Reader { d: data, p: 0 };
    let mut stack: Vec<V> = Vec::new();
    let mut marks: Vec<usize> = Vec::new();
    let mut memo: HashMap<u32, V> = HashMap::new();
    let mut copy_work = 0usize;
    let pop = |stack: &mut Vec<V>| stack.pop().ok_or_else(|| bad("stack underflow"));
    let pop_mark = |stack: &mut Vec<V>, marks: &mut Vec<usize>| -> Result<Vec<V>, SpeechError> {
        let m = marks.pop().ok_or_else(|| bad("missing mark"))?;
        if m > stack.len() {
            return Err(bad("bad mark"));
        }
        Ok(stack.split_off(m))
    };
    loop {
        if stack.len() > MAX_STACK || marks.len() > MAX_STACK {
            return Err(bad("stack too deep"));
        }
        let op = r.u8()?;
        match op {
            0x80 => {
                r.u8()?;
            }
            0x95 => {
                r.u64()?;
            }
            b'.' => return pop(&mut stack),
            b'(' => marks.push(stack.len()),
            b'N' => stack.push(V::None),
            0x88 => stack.push(V::Bool(true)),
            0x89 => stack.push(V::Bool(false)),
            b'J' => stack.push(V::Int(i64::from(r.u32()? as i32))),
            b'K' => stack.push(V::Int(i64::from(r.u8()?))),
            b'M' => stack.push(V::Int(i64::from(r.u16()?))),
            0x8a => {
                let n = usize::from(r.u8()?);
                let b = r.take(n)?;
                if n > 8 {
                    stack.push(V::Opaque);
                } else {
                    let mut v: i64 = 0;
                    for (i, &x) in b.iter().enumerate() {
                        v |= i64::from(x) << (8 * i);
                    }
                    if n > 0 && n < 8 && b.last().is_some_and(|&x| x & 0x80 != 0) {
                        v -= 1i64 << (8 * n);
                    }
                    stack.push(V::Int(v));
                }
            }
            b'G' => {
                // BINFLOAT: no float is needed to locate tensors
                r.take(8)?;
                stack.push(V::Opaque);
            }
            b'X' | b'T' | b'B' => {
                let n = r.u32()? as usize;
                stack.push(V::Str(r.str(n)?));
            }
            0x8c | b'U' | b'C' => {
                let n = usize::from(r.u8()?);
                stack.push(V::Str(r.str(n)?));
            }
            0x8d | 0x8e => {
                let n = usize::try_from(r.u64()?).map_err(|_| bad("length"))?;
                stack.push(V::Str(r.str(n)?));
            }
            b')' => stack.push(V::Tuple(Rc::new(Vec::new()))),
            b']' => stack.push(V::List(Rc::new(Vec::new()))),
            b'}' => stack.push(V::Dict(Rc::new(Vec::new()))),
            b't' => {
                let items = pop_mark(&mut stack, &mut marks)?;
                stack.push(V::Tuple(Rc::new(items)));
            }
            0x85..=0x87 => {
                let n = usize::from(op - 0x84);
                if stack.len() < n {
                    return Err(bad("stack underflow"));
                }
                let items = stack.split_off(stack.len() - n);
                stack.push(V::Tuple(Rc::new(items)));
            }
            b'q' | b'r' | 0x94 => {
                let i = match op {
                    b'q' => u32::from(r.u8()?),
                    b'r' => r.u32()?,
                    _ => memo.len() as u32,
                };
                if memo.len() >= MAX_MEMO {
                    return Err(bad("memo too large"));
                }
                let top = stack.last().ok_or_else(|| bad("stack underflow"))?.clone();
                memo.insert(i, top);
            }
            b'h' | b'j' => {
                let i = if op == b'h' { u32::from(r.u8()?) } else { r.u32()? };
                stack.push(memo.get(&i).ok_or_else(|| bad("unknown memo key"))?.clone());
            }
            b'c' => {
                let module = r.line()?;
                let name = r.line()?;
                stack.push(V::Global(Rc::from(format!("{module}.{name}").as_str())));
            }
            0x93 => {
                let name = pop(&mut stack)?;
                let module = pop(&mut stack)?;
                match (module, name) {
                    (V::Str(m), V::Str(n)) => stack.push(V::Global(Rc::from(format!("{m}.{n}").as_str()))),
                    _ => return Err(bad("STACK_GLOBAL needs strings")),
                }
            }
            b'Q' => {
                let pid = pop(&mut stack)?;
                stack.push(persistent(&pid));
            }
            b'R' | 0x81 => {
                let args = pop(&mut stack)?;
                let callable = pop(&mut stack)?;
                stack.push(reduce(&callable, &args));
            }
            b'b' => {
                // BUILD: set the object's state; state dicts carry only `_metadata` here
                pop(&mut stack)?;
                if stack.is_empty() {
                    return Err(bad("stack underflow"));
                }
            }
            b's' | b'u' => {
                let items = if op == b's' {
                    let v = pop(&mut stack)?;
                    let k = pop(&mut stack)?;
                    vec![k, v]
                } else {
                    pop_mark(&mut stack, &mut marks)?
                };
                if items.len() % 2 != 0 {
                    return Err(bad("odd SETITEMS"));
                }
                match stack.last_mut() {
                    Some(V::Dict(d)) => {
                        if Rc::strong_count(d) > 1 {
                            copy_work = copy_work.saturating_add(d.len());
                        }
                        if copy_work > MAX_COPY_WORK {
                            return Err(bad("too much copying"));
                        }
                        let d = Rc::make_mut(d);
                        let mut it = items.into_iter();
                        while let (Some(k), Some(v)) = (it.next(), it.next()) {
                            d.push((k, v));
                        }
                    }
                    Some(_) => {}
                    None => return Err(bad("stack underflow")),
                }
            }
            b'a' | b'e' => {
                let items = if op == b'a' { vec![pop(&mut stack)?] } else { pop_mark(&mut stack, &mut marks)? };
                match stack.last_mut() {
                    Some(V::List(l)) => {
                        if Rc::strong_count(l) > 1 {
                            copy_work = copy_work.saturating_add(l.len());
                        }
                        if copy_work > MAX_COPY_WORK {
                            return Err(bad("too much copying"));
                        }
                        Rc::make_mut(l).extend(items)
                    }
                    Some(_) => {}
                    None => return Err(bad("stack underflow")),
                }
            }
            b'0' => {
                pop(&mut stack)?;
            }
            b'2' => {
                let top = stack.last().ok_or_else(|| bad("stack underflow"))?.clone();
                stack.push(top);
            }
            b'1' => {
                pop_mark(&mut stack, &mut marks)?;
            }
            other => return Err(bad(format!("unsupported opcode 0x{other:02x} at byte {}", r.p - 1))),
        }
    }
}

/// The tensors of a pickled state dict, in order: `(name, layout)`. A Lightning-style
/// `{"state_dict": {...}}` wrapper is unwrapped; non-tensor entries are skipped.
pub fn state_dict(data: &[u8]) -> Result<Vec<(String, TensorInfo)>, SpeechError> {
    let mut v = run(data)?;
    for _ in 0..2 {
        let V::Dict(d) = &v else { break };
        let inner = d.iter().find(|(k, _)| matches!(k, V::Str(s) if s.as_ref() == "state_dict")).map(|(_, v)| v.clone());
        match inner {
            Some(i) => v = i,
            None => break,
        }
    }
    let V::Dict(d) = v else { return Err(bad("the checkpoint is not a state dict")) };
    let mut out = Vec::new();
    for (k, val) in d.iter() {
        if let (V::Str(k), V::Tensor(t)) = (k, val) {
            out.push((k.to_string(), (**t).clone()));
        }
    }
    if out.is_empty() {
        return Err(bad("the state dict holds no tensors"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nemo::testutil::state_dict_pickle;

    #[test]
    fn reads_a_torch_style_state_dict() {
        let p = state_dict_pickle(&[("enc.w", "0", 0, &[2, 3]), ("enc.b", "1", 4, &[3])]);
        let sd = state_dict(&p).unwrap();
        assert_eq!(sd.len(), 2);
        assert_eq!(sd[0].0, "enc.w");
        assert_eq!(sd[0].1, TensorInfo { storage: "0".into(), dtype: DType::F32, offset: 0, shape: vec![2, 3], stride: vec![3, 1] });
        assert!(sd[0].1.is_contiguous());
        assert_eq!(sd[1].1.offset, 4);
        assert_eq!(sd[1].1.span_elements(), Some(3));
    }

    #[test]
    fn long1_and_layouts() {
        // LONG1 of -2 (two bytes, little endian two's complement), then STOP
        let v = run(&[0x8a, 2, 0xfe, 0xff, b'.']).unwrap();
        assert!(matches!(v, V::Int(-2)));
        let t = TensorInfo { storage: "0".into(), dtype: DType::F32, offset: 0, shape: vec![2, 3], stride: vec![1, 2] };
        assert!(!t.is_contiguous());
        assert_eq!(t.span_elements(), Some(6));
    }

    #[test]
    fn hostile_pickles_fail_cleanly() {
        let good = state_dict_pickle(&[("a", "0", 0, &[4, 4]), ("b", "1", 0, &[4])]);
        assert!(state_dict(&[]).is_err());
        assert!(state_dict(b"(((((((").is_err());
        assert!(state_dict(b"h\x05.").is_err());
        assert!(state_dict(b"X\xff\xff\xff\xff").is_err());
        assert!(state_dict(b"}.").is_err());
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for i in 0..2000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let mut b = good.clone();
            if i % 3 == 0 {
                b.truncate((seed as usize) % b.len());
            } else {
                for k in 0..1 + i % 4 {
                    let at = ((seed >> (k * 8)) as usize) % b.len();
                    b[at] ^= (seed >> 40) as u8;
                }
            }
            let r = std::panic::catch_unwind(|| state_dict(&b));
            assert!(r.is_ok(), "case {i} panicked");
        }
    }
}
