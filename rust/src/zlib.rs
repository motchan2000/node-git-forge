//! zlib (RFC 1950) around DEFLATE (RFC 1951): a complete inflater that reports how much input it
//! consumed (pack entries are concatenated zlib streams), and a deflater that emits stored blocks.

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}

/// A zlib stream of stored DEFLATE blocks (valid for every inflater, git's included).
pub fn deflate_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 65535 * 5 + 16);
    out.extend_from_slice(&[0x78, 0x01]);
    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    } else {
        let mut chunks = data.chunks(65535).peekable();
        while let Some(chunk) = chunks.next() {
            let last = chunks.peek().is_none();
            out.push(if last { 1 } else { 0 });
            let len = chunk.len() as u16;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(chunk);
        }
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize, // byte position
    bit: u32,
    nbits: u32,
}

impl<'a> Bits<'a> {
    fn need(&mut self, n: u32) -> Result<(), String> {
        while self.nbits < n {
            if self.pos >= self.data.len() {
                return Err("zlib: unexpected end of stream".into());
            }
            self.bit |= (self.data[self.pos] as u32) << self.nbits;
            self.pos += 1;
            self.nbits += 8;
        }
        Ok(())
    }
    fn bits(&mut self, n: u32) -> Result<u32, String> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.nbits -= n;
        Ok(v)
    }
    fn align(&mut self) {
        // drop the partial byte; any whole bytes buffered go back to the stream
        let whole = self.nbits / 8;
        self.pos -= whole as usize;
        self.bit = 0;
        self.nbits = 0;
    }
}

struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Huffman, String> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut offs = [0u16; 16];
        for i in 1..16 {
            offs[i] = offs[i - 1] + counts[i - 1];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huffman { counts, symbols })
    }
    fn decode(&self, br: &mut Bits) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= br.bits(1)? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("zlib: bad huffman code".into())
    }
}

const LEN_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

fn inflate_codes(br: &mut Bits, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman, limit: usize) -> Result<(), String> {
    loop {
        let sym = lit.decode(br)?;
        if sym < 256 {
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let s = (sym - 257) as usize;
            if s >= 29 {
                return Err("zlib: bad length symbol".into());
            }
            let len = LEN_BASE[s] as usize + br.bits(LEN_EXTRA[s] as u32)? as usize;
            let ds = dist.decode(br)? as usize;
            if ds >= 30 {
                return Err("zlib: bad distance symbol".into());
            }
            let d = DIST_BASE[ds] as usize + br.bits(DIST_EXTRA[ds] as u32)? as usize;
            if d > out.len() {
                return Err("zlib: distance too far back".into());
            }
            let start = out.len() - d;
            for i in 0..len {
                let b = out[start + i];
                out.push(b);
            }
        }
        if out.len() > limit {
            return Err("zlib: output larger than expected".into());
        }
    }
}

/// Inflate the zlib stream at the start of `data`. Returns the bytes and the input consumed
/// (header, blocks and the Adler-32 trailer). `limit` caps the output size.
pub fn inflate(data: &[u8], limit: usize) -> Result<(Vec<u8>, usize), String> {
    if data.len() < 2 {
        return Err("zlib: stream too short".into());
    }
    let (cmf, flg) = (data[0], data[1]);
    if cmf & 0x0f != 8 || ((cmf as u16) << 8 | flg as u16) % 31 != 0 {
        return Err("zlib: bad header".into());
    }
    if flg & 0x20 != 0 {
        return Err("zlib: preset dictionaries are not supported".into());
    }
    let mut br = Bits { data, pos: 2, bit: 0, nbits: 0 };
    let mut out: Vec<u8> = Vec::with_capacity(limit.min(1 << 20));
    loop {
        let last = br.bits(1)?;
        let kind = br.bits(2)?;
        match kind {
            0 => {
                br.align();
                if br.pos + 4 > data.len() {
                    return Err("zlib: truncated stored block".into());
                }
                let len = u16::from_le_bytes([data[br.pos], data[br.pos + 1]]) as usize;
                let nlen = u16::from_le_bytes([data[br.pos + 2], data[br.pos + 3]]) as usize;
                if len != (!nlen & 0xffff) {
                    return Err("zlib: stored block length mismatch".into());
                }
                br.pos += 4;
                if br.pos + len > data.len() {
                    return Err("zlib: truncated stored block".into());
                }
                out.extend_from_slice(&data[br.pos..br.pos + len]);
                br.pos += len;
                if out.len() > limit {
                    return Err("zlib: output larger than expected".into());
                }
            }
            1 => {
                let mut lengths = [0u8; 288];
                for (i, l) in lengths.iter_mut().enumerate() {
                    *l = match i {
                        0..=143 => 8,
                        144..=255 => 9,
                        256..=279 => 7,
                        _ => 8,
                    };
                }
                let lit = Huffman::new(&lengths)?;
                let dist = Huffman::new(&[5u8; 30])?;
                inflate_codes(&mut br, &mut out, &lit, &dist, limit)?;
            }
            2 => {
                let hlit = br.bits(5)? as usize + 257;
                let hdist = br.bits(5)? as usize + 1;
                let hclen = br.bits(4)? as usize + 4;
                const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                let mut cl = [0u8; 19];
                for &idx in ORDER.iter().take(hclen) {
                    cl[idx] = br.bits(3)? as u8;
                }
                let clh = Huffman::new(&cl)?;
                let mut lengths = vec![0u8; hlit + hdist];
                let mut i = 0;
                while i < hlit + hdist {
                    let sym = clh.decode(&mut br)?;
                    match sym {
                        0..=15 => {
                            lengths[i] = sym as u8;
                            i += 1;
                        }
                        16 => {
                            if i == 0 {
                                return Err("zlib: repeat with no previous length".into());
                            }
                            let prev = lengths[i - 1];
                            let n = 3 + br.bits(2)? as usize;
                            if i + n > lengths.len() {
                                return Err("zlib: too many lengths".into());
                            }
                            for _ in 0..n {
                                lengths[i] = prev;
                                i += 1;
                            }
                        }
                        17 | 18 => {
                            let n = if sym == 17 { 3 + br.bits(3)? as usize } else { 11 + br.bits(7)? as usize };
                            if i + n > lengths.len() {
                                return Err("zlib: too many lengths".into());
                            }
                            i += n;
                        }
                        _ => return Err("zlib: bad code length symbol".into()),
                    }
                }
                let lit = Huffman::new(&lengths[..hlit])?;
                let dist = Huffman::new(&lengths[hlit..])?;
                inflate_codes(&mut br, &mut out, &lit, &dist, limit)?;
            }
            _ => return Err("zlib: reserved block type".into()),
        }
        if last == 1 {
            break;
        }
    }
    br.align();
    if br.pos + 4 > data.len() {
        return Err("zlib: missing adler-32 trailer".into());
    }
    let want = u32::from_be_bytes([data[br.pos], data[br.pos + 1], data[br.pos + 2], data[br.pos + 3]]);
    if want != adler32(&out) {
        return Err("zlib: adler-32 mismatch".into());
    }
    Ok((out, br.pos + 4))
}
