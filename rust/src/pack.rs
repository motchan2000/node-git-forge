//! Packfiles and pkt-lines: reading packs (OFS_DELTA / REF_DELTA, trailer check) and writing them,
//! plus pkt-line and side-band framing.

use crate::objects::{hash_object, Kind, Obj};
use crate::util::{hex, sha1, Sha1};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

const OFS_DELTA: u8 = 6;
const REF_DELTA: u8 = 7;

pub fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, String> {
    let mut i = 0;
    let mut varint = |i: &mut usize| -> Result<usize, String> {
        let (mut v, mut shift) = (0usize, 0);
        loop {
            let b = *delta.get(*i).ok_or("truncated delta header")?;
            *i += 1;
            v |= ((b & 0x7f) as usize) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
    };
    let base_size = varint(&mut i)?;
    let result_size = varint(&mut i)?;
    if base_size != base.len() {
        return Err("delta base size mismatch".into());
    }
    let mut out = Vec::with_capacity(result_size);
    while i < delta.len() {
        let op = delta[i];
        i += 1;
        if op & 0x80 != 0 {
            let mut offset = 0usize;
            let mut size = 0usize;
            for bit in 0..4 {
                if op & (1 << bit) != 0 {
                    offset |= (*delta.get(i).ok_or("truncated delta")? as usize) << (8 * bit);
                    i += 1;
                }
            }
            for bit in 0..3 {
                if op & (0x10 << bit) != 0 {
                    size |= (*delta.get(i).ok_or("truncated delta")? as usize) << (8 * bit);
                    i += 1;
                }
            }
            if size == 0 {
                size = 0x10000;
            }
            if offset + size > base.len() {
                return Err("delta copy out of range".into());
            }
            out.extend_from_slice(&base[offset..offset + size]);
        } else if op != 0 {
            let n = op as usize;
            if i + n > delta.len() {
                return Err("delta insert out of range".into());
            }
            out.extend_from_slice(&delta[i..i + n]);
            i += n;
        } else {
            return Err("delta opcode 0 is reserved".into());
        }
        if out.len() > result_size {
            return Err("delta output too large".into());
        }
    }
    if out.len() != result_size {
        return Err("delta result size mismatch".into());
    }
    Ok(out)
}

struct Raw {
    offset: usize,
    code: u8,
    data: Vec<u8>,
    base_offset: Option<usize>,
    base_oid: Option<String>,
}

/// Parse and resolve a whole pack. `external` supplies REF_DELTA bases outside the pack.
pub fn read_pack(buf: &[u8], external: &dyn Fn(&str) -> Option<Obj>) -> Result<Vec<(String, Obj)>, String> {
    if buf.len() < 32 {
        return Err("pack is too short".into());
    }
    if &buf[0..4] != b"PACK" {
        return Err("missing PACK signature".into());
    }
    let version = u32::from_be_bytes(buf[4..8].try_into().unwrap());
    if version != 2 && version != 3 {
        return Err(format!("unsupported pack version {}", version));
    }
    let count = u32::from_be_bytes(buf[8..12].try_into().unwrap()) as usize;
    let trailer = buf.len() - 20;
    if sha1(&buf[..trailer])[..] != buf[trailer..] {
        return Err("pack trailer checksum mismatch".into());
    }
    let mut entries: Vec<Raw> = Vec::with_capacity(count);
    let mut by_offset: HashMap<usize, usize> = HashMap::new();
    let mut i = 12;
    for _ in 0..count {
        if i >= trailer {
            return Err("pack ends before its declared object count".into());
        }
        let offset = i;
        let mut b = buf[i];
        i += 1;
        let code = (b >> 4) & 7;
        let mut size = (b & 0x0f) as usize;
        let mut shift = 4;
        while b & 0x80 != 0 {
            if i >= trailer {
                return Err("truncated object header".into());
            }
            b = buf[i];
            i += 1;
            size |= ((b & 0x7f) as usize) << shift;
            shift += 7;
        }
        let (mut base_offset, mut base_oid) = (None, None);
        if code == OFS_DELTA {
            let mut c = buf[i];
            i += 1;
            let mut ofs = (c & 0x7f) as usize;
            while c & 0x80 != 0 {
                c = buf[i];
                i += 1;
                ofs = ((ofs + 1) << 7) | (c & 0x7f) as usize;
            }
            if ofs > offset || offset - ofs < 12 {
                return Err("ofs-delta base before the pack start".into());
            }
            base_offset = Some(offset - ofs);
        } else if code == REF_DELTA {
            if i + 20 > trailer {
                return Err("truncated ref-delta".into());
            }
            base_oid = Some(hex(&buf[i..i + 20]));
            i += 20;
        } else if Kind::from_code(code).is_none() {
            return Err(format!("bad object type {} at {}", code, offset));
        }
        let (data, used) = crate::zlib::inflate(&buf[i..trailer], size)?;
        if data.len() != size {
            return Err(format!("object at {} inflated to {} bytes, header says {}", offset, data.len(), size));
        }
        i += used;
        by_offset.insert(offset, entries.len());
        entries.push(Raw { offset, code, data, base_offset, base_oid });
    }
    let mut resolved: Vec<Option<Obj>> = vec![None; entries.len()];
    let mut objects: HashMap<String, Obj> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for (idx, e) in entries.iter_mut().enumerate() {
        if let Some(kind) = Kind::from_code(e.code) {
            let data = std::mem::take(&mut e.data);
            let oid = hash_object(kind, &data);
            let obj = Obj { kind, data: Arc::new(data) };
            resolved[idx] = Some(obj.clone());
            if !objects.contains_key(&oid) {
                order.push(oid.clone());
            }
            objects.insert(oid, obj);
        }
    }
    let mut pending: Vec<usize> = (0..entries.len()).filter(|&i| resolved[i].is_none()).collect();
    while !pending.is_empty() {
        let mut next = Vec::new();
        for &idx in &pending {
            let e = &entries[idx];
            let base = if let Some(bo) = e.base_offset {
                let bi = *by_offset.get(&bo).ok_or("ofs-delta base is not an object start")?;
                resolved[bi].clone()
            } else if let Some(bo) = &e.base_oid {
                objects.get(bo).cloned().or_else(|| external(bo))
            } else {
                None
            };
            let Some(base) = base else {
                next.push(idx);
                continue;
            };
            let data = apply_delta(&base.data, &e.data)?;
            let oid = hash_object(base.kind, &data);
            let obj = Obj { kind: base.kind, data: Arc::new(data) };
            resolved[idx] = Some(obj.clone());
            if !objects.contains_key(&oid) {
                order.push(oid.clone());
            }
            objects.insert(oid, obj);
        }
        if next.len() == pending.len() {
            return Err(format!("{} delta object(s) have no base", next.len()));
        }
        pending = next;
    }
    Ok(order.into_iter().map(|oid| {
        let o = objects.remove(&oid).unwrap();
        (oid, o)
    }).collect())
}

fn entry_header(kind: Kind, size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut byte = (kind.code() << 4) | (size & 0x0f) as u8;
    let mut rest = size >> 4;
    while rest > 0 {
        out.push(byte | 0x80);
        byte = (rest & 0x7f) as u8;
        rest >>= 7;
    }
    out.push(byte);
    out
}

fn entry_cache() -> &'static Mutex<HashMap<String, Arc<Vec<u8>>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Vec<u8>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One pack entry (header + zlib body), cached by oid.
pub fn pack_entry(oid: &str, obj: &Obj) -> Arc<Vec<u8>> {
    if let Some(e) = entry_cache().lock().unwrap().get(oid) {
        return e.clone();
    }
    let mut e = entry_header(obj.kind, obj.data.len());
    e.extend_from_slice(&crate::zlib::deflate_stored(&obj.data));
    let e = Arc::new(e);
    entry_cache().lock().unwrap().insert(oid.to_string(), e.clone());
    e
}

pub fn write_pack(list: &[(String, Obj)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"PACK");
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&(list.len() as u32).to_be_bytes());
    for (oid, obj) in list {
        out.extend_from_slice(&pack_entry(oid, obj));
    }
    let mut h = Sha1::new();
    h.update(&out);
    let sum = h.finish();
    out.extend_from_slice(&sum);
    out
}

// ---------------------------------------------------------------------------------------------
// pkt-line

pub enum Packet<'a> {
    Data(&'a [u8]),
    Flush,
    Delim,
    End,
}

pub fn pkt(payload: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", payload.len() + 4).into_bytes();
    out.extend_from_slice(payload);
    out
}
pub fn pkt_str(s: &str) -> Vec<u8> {
    pkt(s.as_bytes())
}
pub const FLUSH: &[u8] = b"0000";
pub const DELIM: &[u8] = b"0001";

/// Parse packets from `start`, stopping after `stop_after_flushes` flushes. Returns packets and the
/// offset after the last one read.
pub fn parse_packets(buf: &[u8], start: usize, stop_after_flushes: usize) -> Result<(Vec<Packet<'_>>, usize), String> {
    let mut out = Vec::new();
    let mut i = start;
    let mut flushes = 0;
    while i < buf.len() && flushes < stop_after_flushes {
        if i + 4 > buf.len() {
            return Err("truncated pkt-line header".into());
        }
        let hexs = std::str::from_utf8(&buf[i..i + 4]).map_err(|_| "bad pkt-line length")?;
        let len = usize::from_str_radix(hexs, 16).map_err(|_| format!("bad pkt-line length {:?}", hexs))?;
        match len {
            0 => {
                out.push(Packet::Flush);
                flushes += 1;
                i += 4;
            }
            1 => {
                out.push(Packet::Delim);
                i += 4;
            }
            2 => {
                out.push(Packet::End);
                i += 4;
            }
            3 => return Err("bad pkt-line length 3".into()),
            _ => {
                if i + len > buf.len() {
                    return Err("truncated pkt-line payload".into());
                }
                out.push(Packet::Data(&buf[i + 4..i + len]));
                i += len;
            }
        }
    }
    Ok((out, i))
}

pub fn line_of(p: &Packet) -> Option<String> {
    match p {
        Packet::Data(d) => {
            let s = String::from_utf8_lossy(d);
            Some(s.strip_suffix('\n').unwrap_or(&s).to_string())
        }
        _ => None,
    }
}

/// Side-band framing of `data` on `band`, each packet within the pkt-line limit.
pub fn sideband(band: u8, data: &[u8], chunk: usize, out: &mut Vec<u8>) {
    for part in data.chunks(chunk) {
        out.extend_from_slice(format!("{:04x}", part.len() + 5).as_bytes());
        out.push(band);
        out.extend_from_slice(part);
    }
}
