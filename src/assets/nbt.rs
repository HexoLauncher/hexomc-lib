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

    pub fn as_list(&self) -> Option<&[Nbt]> {
        match self {
            Nbt::List(items) => Some(items),
            _ => None,
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
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
        let len = self.i16()? as usize;

        Some(String::from_utf8_lossy(self.take(len)?).to_string())
    }

    fn value(&mut self, tag: u8) -> Option<Nbt> {
        Some(match tag {
            1 => Nbt::Byte(self.u8()? as i8),
            2 => Nbt::Short(self.i16()?),
            3 => Nbt::Int(self.i32()?),
            4 => Nbt::Long(self.i64()?),
            5 => Nbt::Float(f32::from_bits(self.i32()? as u32)),
            6 => Nbt::Double(f64::from_bits(self.i64()? as u64)),
            7 => {
                let len = self.i32()?.max(0) as usize;
                Nbt::ByteArray(self.take(len)?.to_vec())
            }
            8 => Nbt::String(self.string()?),
            9 => {
                let item_tag = self.u8()?;
                let len = self.i32()?.max(0) as usize;
                let mut items = Vec::with_capacity(len.min(1024));
                for _ in 0..len {
                    items.push(self.value(item_tag)?);
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
                    map.insert(name, self.value(child)?);
                }
                Nbt::Compound(map)
            }
            11 => {
                let len = self.i32()?.max(0) as usize;
                let mut items = Vec::with_capacity(len.min(1024));
                for _ in 0..len {
                    items.push(self.i32()?);
                }
                Nbt::IntArray(items)
            }
            12 => {
                let len = self.i32()?.max(0) as usize;
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
    let owned;
    let data = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut out)
            .ok()?;
        owned = out;
        owned.as_slice()
    } else {
        bytes
    };

    let mut reader = Reader { data, pos: 0 };
    let tag = reader.u8()?;
    if tag != 10 {
        return None;
    }
    reader.string()?; // Root node name, unused
    reader.value(10)
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
}