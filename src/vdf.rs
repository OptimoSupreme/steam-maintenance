//! Byte-exact binary KeyValues (shortcuts.vdf) codec. STUB: to be implemented.

use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Map(Vec<(Vec<u8>, Value)>),
    Str(Vec<u8>),
    Int32([u8; 4]),
}

pub fn parse(_bytes: &[u8]) -> Result<Value> {
    anyhow::bail!("vdf::parse not implemented")
}

pub fn serialize(_value: &Value) -> Vec<u8> {
    Vec::new()
}
