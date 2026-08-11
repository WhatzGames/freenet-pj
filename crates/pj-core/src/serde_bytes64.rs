//! Serde support for `[u8; 64]`.
//!
//! Serde's built-in array impls stop at 32 elements, and an ed25519 signature is
//! 64 bytes, so signatures need a hand-written pair. Kept private to the crate.

use core::fmt;

use serde::de::{Error as _, SeqAccess, Visitor};
use serde::{Deserializer, Serializer};

pub(crate) fn serialize<S: Serializer>(bytes: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_bytes(bytes)
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
    d.deserialize_bytes(Bytes64)
}

struct Bytes64;

impl<'de> Visitor<'de> for Bytes64 {
    type Value = [u8; 64];

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("64 bytes")
    }

    fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
        v.try_into()
            .map_err(|_| E::invalid_length(v.len(), &"64 bytes"))
    }

    // Some formats (and bincode's own array path) hand bytes over as a sequence.
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut out = [0u8; 64];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = seq
                .next_element::<u8>()?
                .ok_or_else(|| A::Error::invalid_length(i, &"64 bytes"))?;
        }
        Ok(out)
    }
}
