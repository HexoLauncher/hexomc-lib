use std::collections::HashMap;
use std::io::Read;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum Nbt {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    List(Vec<Nbt>),
    Compound(HashMap<String, Nbt>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Nbt {
    pub fn get(&self, key: &str) -> Option<&Nbt> {
        match self {
            Nbt::Compound(map) => map.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Nbt::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Nbt::Byte(v) => Some(*v as i64),
            Nbt::Short(v) => Some(*v as i64),
            Nbt::Int(v) => Some(*v as i64),
            Nbt::Long(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Nbt]> {
        match self {
            Nbt::List(items) => Some(items),
            _ => None,
        }
    }
}

const MAX_NBT_BYTES: usize = 16 * 1024 * 1024;
const MAX_NBT_DEPTH: usize = 64;
const MAX_NBT_NODES: usize = 100_000;

/// Read compressed or plain NBT without allocating an unbounded input buffer.
pub(super) fn read_file(path: &std::path::Path) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?
        .take((MAX_NBT_BYTES + 1) as u64)
        .read_to_end(&mut bytes).ok()?;
    (bytes.len() <= MAX_NBT_BYTES).then_some(bytes)
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    nodes_left: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn i16(&mut self) -> Option<i16> {
        Some(i16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<String> {
        let len = u16::from_be_bytes(self.take(2)?.try_into().ok()?) as usize;

        Some(String::from_utf8_lossy(self.take(len)?).to_string())
    }

    fn value(&mut self, tag: u8, depth: usize) -> Option<Nbt> {
        if depth >= MAX_NBT_DEPTH {
            return None;
        }
        self.nodes_left = self.nodes_left.checked_sub(1)?;
        Some(match tag {
            1 => Nbt::Byte(self.u8()? as i8),
            2 => Nbt::Short(self.i16()?),
            3 => Nbt::Int(self.i32()?),
            4 => Nbt::Long(self.i64()?),
            5 => Nbt::Float(f32::from_bits(self.i32()? as u32)),
            6 => Nbt::Double(f64::from_bits(self.i64()? as u64)),
            7 => {
                let len = usize::try_from(self.i32()?).ok()?;
                Nbt::ByteArray(self.take(len)?.to_vec())
            }
            8 => Nbt::String(self.string()?),
            9 => {
                let item_tag = self.u8()?;
                let len = usize::try_from(self.i32()?).ok()?;
                let mut items = Vec::with_capacity(len.min(1024));
                for _ in 0..len {
                    items.push(self.value(item_tag, depth + 1)?);
                }
                Nbt::List(items)
            }
            10 => {
                let mut map = HashMap::new();
                loop {
                    let child = self.u8()?;
                    if child == 0 {
                        break;
                    }
                    let name = self.string()?;
                    map.insert(name, self.value(child, depth + 1)?);
                }
                Nbt::Compound(map)
            }
            11 => {
                let len = usize::try_from(self.i32()?).ok()?;
                let mut items = Vec::with_capacity(len.min(1024));
                for _ in 0..len {
                    items.push(self.i32()?);
                }
                Nbt::IntArray(items)
            }
            12 => {
                let len = usize::try_from(self.i32()?).ok()?;
                let mut items = Vec::with_capacity(len.min(1024));
                for _ in 0..len {
                    items.push(self.i64()?);
                }
                Nbt::LongArray(items)
            }
            _ => return None,
        })
    }
}

pub fn parse(bytes: &[u8]) -> Option<Nbt> {
    if bytes.len() > MAX_NBT_BYTES {
        return None;
    }
    let owned;
    let data = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .take((MAX_NBT_BYTES + 1) as u64)
            .read_to_end(&mut out)
            .ok()?;
        if out.len() > MAX_NBT_BYTES {
            return None;
        }
        owned = out;
        owned.as_slice()
    } else {
        bytes
    };

    let mut reader = Reader { data, pos: 0, nodes_left: MAX_NBT_NODES };
    let tag = reader.u8()?;
    if tag != 10 {
        return None;
    }
    reader.string()?; // Root node name, unused
    reader.value(10, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut out = vec![10, 0, 0]; // TAG_Compound, empty name
        out.extend([9]); // TAG_List
        out.extend((b"servers".len() as i16).to_be_bytes());
        out.extend(b"servers");
        out.extend([10]); // Element type TAG_Compound
        out.extend(1i32.to_be_bytes());

        for (key, value) in [("name", "Home"), ("ip", "127.0.0.1")] {
            out.extend([8]); // TAG_String
            out.extend((key.len() as i16).to_be_bytes());
            out.extend(key.as_bytes());
            out.extend((value.len() as i16).to_be_bytes());
            out.extend(value.as_bytes());
        }
        out.extend([0]); // End of element
        out.extend([0]); // End of root node
        out
    }

    #[test]
    fn reads_server_entries() {
        let root = parse(&sample()).expect("should parse");
        let servers = root.get("servers").and_then(Nbt::as_list).expect("servers");

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].get("name").and_then(Nbt::as_str), Some("Home"));
        assert_eq!(
            servers[0].get("ip").and_then(Nbt::as_str),
            Some("127.0.0.1")
        );
    }

    #[test]
    fn broken_data_returns_none() {
        assert!(parse(b"not nbt at all").is_none());
        assert!(parse(&sample()[..8]).is_none());
    }

    #[test]
    fn deeply_nested_compounds_and_lists_are_rejected() {
        let mut compounds = vec![10, 0, 0];
        for _ in 0..20_000 { compounds.extend_from_slice(&[10, 0, 0]); }
        compounds.extend(vec![0; 20_001]);
        assert!(parse(&compounds).is_none());

        let mut lists = vec![10, 0, 0, 9, 0, 0];
        for _ in 0..20_000 { lists.extend_from_slice(&[9, 0, 0, 0, 1]); }
        assert!(parse(&lists).is_none());
    }

    #[test]
    fn decompression_output_is_bounded() {
        use std::io::Write;
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(&sample()).unwrap();
        std::io::copy(&mut std::io::repeat(0).take(MAX_NBT_BYTES as u64), &mut gzip).unwrap();
        assert!(parse(&gzip.finish().unwrap()).is_none());
    }

    #[test]
    fn node_budget_and_negative_lengths_are_rejected() {
        let mut reader = Reader { data: &[1], pos: 0, nodes_left: 0 };
        assert!(reader.value(1, 0).is_none());
        let negative_list = [10, 0, 0, 9, 0, 0, 1, 255, 255, 255, 255, 0];
        assert!(parse(&negative_list).is_none());
    }
}
