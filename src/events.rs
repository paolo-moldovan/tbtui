//! Minimal TFRecord + protobuf decoding for TensorBoard event files.
//!
//! Only the parts needed for scalar summaries are decoded, so there is no
//! protobuf codegen dependency. Both legacy `simple_value` scalars (PyTorch,
//! TF1) and tensor-based scalars (TF2 `tf.summary.scalar`, torch `new_style`)
//! are supported.

use std::fs::File;
use std::io::{BufReader, ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;

// ---------------------------------------------------------------- protobuf

enum Wire<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

struct Pb<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Pb<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Pb { buf, pos: 0 }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut out = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.buf.get(self.pos)?;
            self.pos += 1;
            out |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Some(out);
            }
        }
        None
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    fn next(&mut self) -> Option<(u32, Wire<'a>)> {
        if self.pos >= self.buf.len() {
            return None;
        }
        let key = self.varint()?;
        let field = (key >> 3) as u32;
        let wire = match key & 7 {
            0 => Wire::Varint(self.varint()?),
            1 => Wire::Fixed64(u64::from_le_bytes(self.take(8)?.try_into().ok()?)),
            2 => {
                let n = self.varint()? as usize;
                Wire::Bytes(self.take(n)?)
            }
            5 => Wire::Fixed32(u32::from_le_bytes(self.take(4)?.try_into().ok()?)),
            _ => return None, // groups (3/4) are not used by TensorBoard
        };
        Some((field, wire))
    }
}

// ---------------------------------------------------------------- events

pub struct Event {
    pub wall_time: f64,
    pub step: i64,
    pub scalars: Vec<(String, f64)>,
}

pub fn parse_event(buf: &[u8]) -> Option<Event> {
    let mut ev = Event { wall_time: 0.0, step: 0, scalars: Vec::new() };
    let mut pb = Pb::new(buf);
    while let Some((field, w)) = pb.next() {
        match (field, w) {
            (1, Wire::Fixed64(v)) => ev.wall_time = f64::from_bits(v),
            (2, Wire::Varint(v)) => ev.step = v as i64,
            (5, Wire::Bytes(b)) => parse_summary(b, &mut ev.scalars),
            _ => {}
        }
    }
    Some(ev)
}

fn parse_summary(buf: &[u8], out: &mut Vec<(String, f64)>) {
    let mut pb = Pb::new(buf);
    while let Some((field, w)) = pb.next() {
        if let (1, Wire::Bytes(b)) = (field, w)
            && let Some(s) = parse_value(b) {
                out.push(s);
            }
    }
}

fn parse_value(buf: &[u8]) -> Option<(String, f64)> {
    let mut tag = None;
    let mut simple = None;
    let mut tensor = None;
    let mut plugin_scalars = false;
    let mut pb = Pb::new(buf);
    while let Some((field, w)) = pb.next() {
        match (field, w) {
            (1, Wire::Bytes(b)) => tag = Some(String::from_utf8_lossy(b).into_owned()),
            (2, Wire::Fixed32(v)) => simple = Some(f32::from_bits(v) as f64),
            (8, Wire::Bytes(b)) => tensor = Some(b),
            (9, Wire::Bytes(b)) => plugin_scalars = plugin_name(b).as_deref() == Some("scalars"),
            _ => {}
        }
    }
    let tag = tag?;
    if let Some(v) = simple {
        return Some((tag, v));
    }
    let t = parse_tensor(tensor?)?;
    // Accept rank-0 numeric tensors, or anything the scalars plugin claims.
    if t.rank == 0 || plugin_scalars {
        return t.first.map(|v| (tag, v));
    }
    None
}

fn plugin_name(meta: &[u8]) -> Option<String> {
    let mut pb = Pb::new(meta);
    while let Some((field, w)) = pb.next() {
        if let (1, Wire::Bytes(pd)) = (field, w) {
            let mut pb2 = Pb::new(pd);
            while let Some((f2, w2)) = pb2.next() {
                if let (1, Wire::Bytes(n)) = (f2, w2) {
                    return Some(String::from_utf8_lossy(n).into_owned());
                }
            }
        }
    }
    None
}

struct Tensor {
    rank: usize,
    first: Option<f64>,
}

// TensorFlow DataType enum values we understand.
const DT_FLOAT: u64 = 1;
const DT_DOUBLE: u64 = 2;
const DT_INT32: u64 = 3;
const DT_INT64: u64 = 9;
const DT_BOOL: u64 = 10;
const DT_HALF: u64 = 19;

fn parse_tensor(buf: &[u8]) -> Option<Tensor> {
    let mut dtype = 0;
    let mut rank = 0;
    let mut content: Option<&[u8]> = None;
    let mut first: Option<f64> = None;
    let mut pb = Pb::new(buf);
    while let Some((field, w)) = pb.next() {
        match (field, w) {
            (1, Wire::Varint(v)) => dtype = v,
            (2, Wire::Bytes(shape)) => {
                let mut s = Pb::new(shape);
                while let Some((f, _)) = s.next() {
                    if f == 2 {
                        rank += 1;
                    }
                }
            }
            (4, Wire::Bytes(b)) => content = Some(b),
            // float_val (packed or not)
            (5, Wire::Bytes(b)) if first.is_none() && b.len() >= 4 => {
                first = Some(f32::from_le_bytes(b[..4].try_into().ok()?) as f64)
            }
            (5, Wire::Fixed32(v)) if first.is_none() => first = Some(f32::from_bits(v) as f64),
            // double_val
            (6, Wire::Bytes(b)) if first.is_none() && b.len() >= 8 => {
                first = Some(f64::from_le_bytes(b[..8].try_into().ok()?))
            }
            (6, Wire::Fixed64(v)) if first.is_none() => first = Some(f64::from_bits(v)),
            // int_val (also carries half/bool), int64_val
            (7 | 10 | 11, Wire::Bytes(b)) if first.is_none() => {
                first = Pb::new(b).varint().map(|v| int_like(dtype, v))
            }
            (7 | 10 | 11, Wire::Varint(v)) if first.is_none() => first = Some(int_like(dtype, v)),
            // half_val
            (13, Wire::Bytes(b)) if first.is_none() => {
                first = Pb::new(b).varint().map(|v| f16_to_f64(v as u16))
            }
            (13, Wire::Varint(v)) if first.is_none() => first = Some(f16_to_f64(v as u16)),
            _ => {}
        }
    }
    if let Some(c) = content {
        first = match dtype {
            DT_FLOAT if c.len() >= 4 => Some(f32::from_le_bytes(c[..4].try_into().ok()?) as f64),
            DT_DOUBLE if c.len() >= 8 => Some(f64::from_le_bytes(c[..8].try_into().ok()?)),
            DT_INT32 if c.len() >= 4 => Some(i32::from_le_bytes(c[..4].try_into().ok()?) as f64),
            DT_INT64 if c.len() >= 8 => Some(i64::from_le_bytes(c[..8].try_into().ok()?) as f64),
            DT_HALF if c.len() >= 2 => Some(f16_to_f64(u16::from_le_bytes([c[0], c[1]]))),
            DT_BOOL if !c.is_empty() => Some(c[0] as f64),
            _ => None,
        };
    }
    if !matches!(dtype, DT_FLOAT | DT_DOUBLE | DT_INT32 | DT_INT64 | DT_HALF | DT_BOOL) {
        return None;
    }
    Some(Tensor { rank, first })
}

fn int_like(dtype: u64, v: u64) -> f64 {
    match dtype {
        DT_HALF => f16_to_f64(v as u16),
        DT_INT32 => v as i32 as f64,
        _ => v as i64 as f64,
    }
}

fn f16_to_f64(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let frac = (h & 0x3ff) as f64;
    sign * match exp {
        0 => frac * 2f64.powi(-24),
        31 => {
            if frac == 0.0 {
                f64::INFINITY
            } else {
                f64::NAN
            }
        }
        _ => (1.0 + frac / 1024.0) * 2f64.powi(exp - 15),
    }
}

// ---------------------------------------------------------------- tfrecord

/// Read complete records starting at `*offset`, advancing it past each one.
/// A trailing partial record (file still being written) is left for later.
pub fn read_records(path: &Path, offset: &mut u64, mut f: impl FnMut(&[u8])) -> std::io::Result<()> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len < *offset {
        *offset = 0; // file was truncated / replaced
    }
    if len == *offset {
        return Ok(());
    }
    file.seek(SeekFrom::Start(*offset))?;
    let mut r = BufReader::with_capacity(1 << 20, file);
    let mut header = [0u8; 12];
    let mut data = Vec::new();
    loop {
        match r.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        let n = u64::from_le_bytes(header[..8].try_into().unwrap());
        if *offset + 12 + n + 4 > len {
            break; // incomplete (or corrupt) record
        }
        data.resize(n as usize, 0);
        let mut crc = [0u8; 4];
        match r.read_exact(&mut data).and_then(|_| r.read_exact(&mut crc)) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        *offset += 12 + n + 4;
        f(&data);
    }
    Ok(())
}

/// Parse complete records from an in-memory buffer (remote streams).
/// Returns the number of bytes consumed; a trailing partial record is left.
pub fn split_records(buf: &[u8], mut f: impl FnMut(&[u8])) -> usize {
    let mut pos = 0;
    while buf.len() - pos >= 12 {
        let n = u64::from_le_bytes(buf[pos..pos + 8].try_into().unwrap()) as usize;
        let Some(end) = n.checked_add(16).and_then(|l| l.checked_add(pos)) else { break };
        if end > buf.len() {
            break;
        }
        f(&buf[pos + 12..pos + 12 + n]);
        pos = end;
    }
    pos
}

// ---------------------------------------------------------------- writing (demo)

fn crc32c(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0x82F6_3B78 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    });
    !data.iter().fold(!0u32, |c, &b| t[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8))
}

fn masked_crc(data: &[u8]) -> u32 {
    let c = crc32c(data);
    (c.rotate_right(15)).wrapping_add(0xa282_ead8)
}

pub fn frame_record(data: &[u8]) -> Vec<u8> {
    let len = (data.len() as u64).to_le_bytes();
    let mut out = Vec::with_capacity(data.len() + 16);
    out.extend_from_slice(&len);
    out.extend_from_slice(&masked_crc(&len).to_le_bytes());
    out.extend_from_slice(data);
    out.extend_from_slice(&masked_crc(data).to_le_bytes());
    out
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_bytes(out: &mut Vec<u8>, field: u32, b: &[u8]) {
    put_varint(out, ((field as u64) << 3) | 2);
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

pub fn encode_version_event(wall: f64) -> Vec<u8> {
    let mut ev = vec![0x09];
    ev.extend_from_slice(&wall.to_bits().to_le_bytes());
    put_bytes(&mut ev, 3, b"brain.Event:2");
    ev
}

pub fn encode_scalar_event(wall: f64, step: i64, scalars: &[(&str, f32)]) -> Vec<u8> {
    let mut summary = Vec::new();
    for (tag, v) in scalars {
        let mut val = Vec::new();
        put_bytes(&mut val, 1, tag.as_bytes());
        val.push(0x15); // field 2, fixed32
        val.extend_from_slice(&v.to_bits().to_le_bytes());
        put_bytes(&mut summary, 1, &val);
    }
    let mut ev = vec![0x09]; // field 1, fixed64
    ev.extend_from_slice(&wall.to_bits().to_le_bytes());
    ev.push(0x10); // field 2, varint
    put_varint(&mut ev, step as u64);
    put_bytes(&mut ev, 5, &summary);
    ev
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_value() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    #[test]
    fn roundtrip_scalar() {
        let ev = encode_scalar_event(1.5, 42, &[("loss", 0.25), ("acc", 0.9)]);
        let e = parse_event(&ev).unwrap();
        assert_eq!(e.step, 42);
        assert_eq!(e.wall_time, 1.5);
        assert_eq!(e.scalars[0], ("loss".into(), 0.25));
        assert_eq!(e.scalars[1].0, "acc");
    }

    #[test]
    fn half_floats() {
        assert_eq!(f16_to_f64(0x3c00), 1.0);
        assert_eq!(f16_to_f64(0xc000), -2.0);
    }
}
