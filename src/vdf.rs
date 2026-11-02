//! Byte-exact binary KeyValues codec for Steam's shortcuts.vdf.
//!
//! A file is a sequence of typed entries: a type byte, a NUL-terminated key, then a
//! value whose shape the type byte decides. A map holds entries until an `0x08` byte.
//! Nothing is normalised on the way in: keys keep their bytes and case, strings stay
//! raw bytes, numbers stay the little-endian bytes Steam wrote. `serialize(parse(b))`
//! therefore reproduces `b` exactly, which is what lets `register` rewrite a user's
//! file without disturbing entries it does not own.

use anyhow::{bail, Context, Result};

const TYPE_MAP: u8 = 0x00;
const TYPE_STRING: u8 = 0x01;
const TYPE_INT32: u8 = 0x02;
const TYPE_FLOAT: u8 = 0x03;
const TYPE_POINTER: u8 = 0x04;
const TYPE_WIDE_STRING: u8 = 0x05;
const TYPE_COLOR: u8 = 0x06;
const TYPE_UINT64: u8 = 0x07;
const TYPE_END: u8 = 0x08;
const TYPE_INT64: u8 = 0x0A;

/// One KeyValues value. Numeric variants keep the raw little-endian bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// Ordered entries; keys are raw bytes and may repeat.
    Map(Vec<(Vec<u8>, Value)>),
    /// NUL-terminated string, stored without the terminator; not necessarily UTF-8.
    Str(Vec<u8>),
    Int32([u8; 4]),
    Float([u8; 4]),
    Pointer([u8; 4]),
    /// UTF-16LE code units as raw bytes, stored without the two-byte terminator.
    WideStr(Vec<u8>),
    Color([u8; 4]),
    Uint64([u8; 8]),
    Int64([u8; 8]),
}

impl Value {
    /// An empty map.
    pub fn map() -> Value {
        Value::Map(Vec::new())
    }

    /// A string value.
    pub fn string(s: &str) -> Value {
        Value::Str(s.as_bytes().to_vec())
    }

    /// A 32-bit integer value.
    pub fn int32(n: i32) -> Value {
        Value::Int32(n.to_le_bytes())
    }

    /// The entries of a map, or None for any other variant.
    pub fn entries(&self) -> Option<&Vec<(Vec<u8>, Value)>> {
        match self {
            Value::Map(entries) => Some(entries),
            _ => None,
        }
    }

    /// Mutable entries of a map, or None for any other variant.
    pub fn entries_mut(&mut self) -> Option<&mut Vec<(Vec<u8>, Value)>> {
        match self {
            Value::Map(entries) => Some(entries),
            _ => None,
        }
    }

    /// First entry whose key matches `key` ignoring ASCII case.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries()?
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key.as_bytes()))
            .map(|(_, v)| v)
    }

    /// Mutable form of [`get`](Value::get).
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.entries_mut()?
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(key.as_bytes()))
            .map(|(_, v)| v)
    }

    /// Replace the value under `key` keeping the key's existing spelling, or append
    /// `key` as given when absent. Does nothing on a non-map.
    pub fn insert(&mut self, key: &str, value: Value) {
        let Some(entries) = self.entries_mut() else {
            return;
        };
        match entries
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(key.as_bytes()))
        {
            Some((_, slot)) => *slot = value,
            None => entries.push((key.as_bytes().to_vec(), value)),
        }
    }

    /// Lossy UTF-8 view of a string (narrow or wide); None for other variants.
    #[cfg(test)]
    pub fn as_str(&self) -> Option<String> {
        match self {
            Value::Str(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            Value::WideStr(bytes) => {
                let units: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .collect();
                Some(String::from_utf16_lossy(&units))
            }
            _ => None,
        }
    }

    /// The raw bytes of a narrow string; None for other variants.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Str(bytes) => Some(bytes),
            _ => None,
        }
    }

    /// The integer of an `Int32`; None for other variants.
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Value::Int32(bytes) => Some(i32::from_le_bytes(*bytes)),
            _ => None,
        }
    }

    fn type_byte(&self) -> u8 {
        match self {
            Value::Map(_) => TYPE_MAP,
            Value::Str(_) => TYPE_STRING,
            Value::Int32(_) => TYPE_INT32,
            Value::Float(_) => TYPE_FLOAT,
            Value::Pointer(_) => TYPE_POINTER,
            Value::WideStr(_) => TYPE_WIDE_STRING,
            Value::Color(_) => TYPE_COLOR,
            Value::Uint64(_) => TYPE_UINT64,
            Value::Int64(_) => TYPE_INT64,
        }
    }
}

/// Parse a whole file. The file is one map: entries up to a closing `0x08` byte,
/// and nothing after it.
pub fn parse(bytes: &[u8]) -> Result<Value> {
    let mut reader = Reader { bytes, pos: 0 };
    let root = reader.map_body()?;
    if reader.pos != bytes.len() {
        bail!(
            "{} trailing bytes after the closing map at byte {}",
            bytes.len() - reader.pos,
            reader.pos
        );
    }
    Ok(root)
}

/// Serialize a whole file; the inverse of [`parse`]. A map root is written as its
/// entries followed by the closing `0x08`. Any other root is written as a single entry
/// with an empty key, then the terminator.
pub fn serialize(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    match value {
        Value::Map(entries) => write_map_body(&mut out, entries),
        other => {
            write_entry(&mut out, b"", other);
            out.push(TYPE_END);
        }
    }
    out
}

fn write_map_body(out: &mut Vec<u8>, entries: &[(Vec<u8>, Value)]) {
    for (key, value) in entries {
        write_entry(out, key, value);
    }
    out.push(TYPE_END);
}

fn write_entry(out: &mut Vec<u8>, key: &[u8], value: &Value) {
    out.push(value.type_byte());
    out.extend_from_slice(key);
    out.push(0);
    match value {
        Value::Map(entries) => write_map_body(out, entries),
        Value::Str(bytes) => {
            out.extend_from_slice(bytes);
            out.push(0);
        }
        Value::WideStr(bytes) => {
            out.extend_from_slice(bytes);
            out.extend_from_slice(&[0, 0]);
        }
        Value::Int32(b) | Value::Float(b) | Value::Pointer(b) | Value::Color(b) => {
            out.extend_from_slice(b);
        }
        Value::Uint64(b) | Value::Int64(b) => out.extend_from_slice(b),
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8> {
        let b = *self
            .bytes
            .get(self.pos)
            .with_context(|| format!("unexpected end of data at byte {}", self.pos))?;
        self.pos += 1;
        Ok(b)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self.pos + N;
        let slice = self
            .bytes
            .get(self.pos..end)
            .with_context(|| format!("unexpected end of data at byte {}", self.pos))?;
        let mut out = [0; N];
        out.copy_from_slice(slice);
        self.pos = end;
        Ok(out)
    }

    /// Bytes up to a NUL, which is consumed and not returned.
    fn cstring(&mut self) -> Result<Vec<u8>> {
        let rest = &self.bytes[self.pos..];
        let len = rest
            .iter()
            .position(|&b| b == 0)
            .with_context(|| format!("unterminated string starting at byte {}", self.pos))?;
        let s = rest[..len].to_vec();
        self.pos += len + 1;
        Ok(s)
    }

    /// UTF-16LE code units up to an aligned 0x0000 unit, which is consumed and not returned.
    fn wide_cstring(&mut self) -> Result<Vec<u8>> {
        let start = self.pos;
        loop {
            let unit = self
                .array::<2>()
                .with_context(|| format!("unterminated wide string starting at byte {start}"))?;
            if unit == [0, 0] {
                return Ok(self.bytes[start..self.pos - 2].to_vec());
            }
        }
    }

    fn map_body(&mut self) -> Result<Value> {
        let mut entries = Vec::new();
        loop {
            let at = self.pos;
            let type_byte = self.byte()?;
            if type_byte == TYPE_END {
                return Ok(Value::Map(entries));
            }
            let key = self.cstring()?;
            let value = self.value(type_byte).with_context(|| {
                format!("in entry {:?} at byte {at}", String::from_utf8_lossy(&key))
            })?;
            entries.push((key, value));
        }
    }

    fn value(&mut self, type_byte: u8) -> Result<Value> {
        Ok(match type_byte {
            TYPE_MAP => self.map_body()?,
            TYPE_STRING => Value::Str(self.cstring()?),
            TYPE_INT32 => Value::Int32(self.array()?),
            TYPE_FLOAT => Value::Float(self.array()?),
            TYPE_POINTER => Value::Pointer(self.array()?),
            TYPE_WIDE_STRING => Value::WideStr(self.wide_cstring()?),
            TYPE_COLOR => Value::Color(self.array()?),
            TYPE_UINT64 => Value::Uint64(self.array()?),
            TYPE_INT64 => Value::Int64(self.array()?),
            other => bail!("unknown value type 0x{other:02x}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    /// `(name, bytes)` of every `*.vdf` under tests/fixtures, sorted by name.
    pub fn fixtures() -> Vec<(String, Vec<u8>)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut out: Vec<(String, Vec<u8>)> = fs::read_dir(&dir)
            .expect("tests/fixtures exists")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "vdf"))
            .map(|p| {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                (name, fs::read(&p).unwrap())
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn every_fixture_round_trips_byte_exactly() {
        let fixtures = fixtures();
        assert!(
            fixtures.len() >= 8,
            "expected the Steam fixtures, found {}",
            fixtures.len()
        );
        for (name, bytes) in fixtures {
            let parsed = parse(&bytes).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            let again = serialize(&parsed);
            assert!(again == bytes, "{name}: serialize(parse(bytes)) differs");
            assert_eq!(parse(&again).unwrap(), parsed, "{name}: reparse differs");
        }
    }

    #[test]
    fn minimal_file_parses_to_the_expected_tree() {
        let bytes: &[u8] = b"\x00shortcuts\x00\
            \x000\x00\
                \x02appid\x00\x78\x56\x34\x12\
                \x01AppName\x00Example\x00\
                \x00tags\x00\x010\x00favorite\x00\x08\
            \x08\
        \x08\
        \x08";
        let expected = Value::Map(vec![(
            b"shortcuts".to_vec(),
            Value::Map(vec![(
                b"0".to_vec(),
                Value::Map(vec![
                    (b"appid".to_vec(), Value::int32(0x1234_5678)),
                    (b"AppName".to_vec(), Value::string("Example")),
                    (
                        b"tags".to_vec(),
                        Value::Map(vec![(b"0".to_vec(), Value::string("favorite"))]),
                    ),
                ]),
            )]),
        )]);
        let parsed = parse(bytes).unwrap();
        assert_eq!(parsed, expected);
        assert_eq!(serialize(&parsed), bytes);
    }

    #[test]
    fn every_value_type_round_trips() {
        let mut bytes = vec![TYPE_FLOAT];
        bytes.extend_from_slice(b"f\x00\x00\x00\x80\x3f");
        bytes.extend_from_slice(b"\x04p\x00\x01\x02\x03\x04");
        bytes.extend_from_slice(b"\x05w\x00h\x00i\x00\x00\x00");
        bytes.extend_from_slice(b"\x06c\x00\xff\x00\xff\x00");
        bytes.extend_from_slice(b"\x07u\x00\x01\x00\x00\x00\x00\x00\x00\x00");
        bytes.extend_from_slice(b"\x0ai\x00\xff\xff\xff\xff\xff\xff\xff\xff");
        bytes.push(TYPE_END);
        let parsed = parse(&bytes).unwrap();
        assert_eq!(parsed.get("w").unwrap().as_str().as_deref(), Some("hi"));
        assert_eq!(parsed.get("i"), Some(&Value::Int64([0xff; 8])));
        assert_eq!(serialize(&parsed), bytes);
    }

    #[test]
    fn truncated_input_errors_instead_of_panicking() {
        let (_, full) = fixtures().into_iter().next().unwrap();
        for len in 0..full.len() {
            let err = parse(&full[..len]).expect_err("truncated file must not parse");
            assert!(!err.to_string().is_empty());
        }
        assert!(parse(b"\x01key\x00no terminator").is_err());
        assert!(parse(b"\x05w\x00a\x00").is_err());
        assert!(parse(b"\x09bad\x00\x08").is_err());
    }

    #[test]
    fn trailing_bytes_are_an_error() {
        assert!(parse(b"\x08\x00").is_err());
        assert_eq!(parse(b"\x08").unwrap(), Value::map());
    }

    #[test]
    fn get_ignores_ascii_case_and_insert_keeps_spelling() {
        let mut map = Value::Map(vec![
            (b"appname".to_vec(), Value::string("old")),
            (b"OpenVR".to_vec(), Value::int32(1)),
        ]);
        assert_eq!(map.get("AppName").unwrap().as_str().as_deref(), Some("old"));
        assert_eq!(map.get("openvr").unwrap().as_i32(), Some(1));
        assert!(map.get("Exe").is_none());

        map.insert("AppName", Value::string("new"));
        map.insert("Exe", Value::string("\"/bin/true\""));
        let keys: Vec<&[u8]> = map
            .entries()
            .unwrap()
            .iter()
            .map(|(k, _)| k.as_slice())
            .collect();
        assert_eq!(keys, [&b"appname"[..], b"OpenVR", b"Exe"]);
        assert_eq!(map.get("appname").unwrap().as_str().as_deref(), Some("new"));

        *map.get_mut("openvr").unwrap() = Value::int32(0);
        assert_eq!(map.get("OpenVR").unwrap().as_i32(), Some(0));
        assert!(Value::string("x").get("anything").is_none());
    }

    #[test]
    fn as_str_is_lossy_on_bad_utf8() {
        let v = Value::Str(b"caf\xe9".to_vec());
        assert_eq!(v.as_str().as_deref(), Some("caf\u{fffd}"));
        assert_eq!(v.as_bytes(), Some(&b"caf\xe9"[..]));
        assert_eq!(v.as_i32(), None);
    }
}
