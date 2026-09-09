//! The layout of one item in the bucket: the name its bytes are stored
//! under, the name its metadata sits beside them under, the text the
//! metadata is kept as, and the checksum the receipt carries.
//!
//! `<prefix>/<data_type>/<identifier>` holds the bytes and the same key
//! with `.meta` appended holds the metadata, so an operator listing the
//! prefix sees the archive laid out by type and can open either object
//! with any Cloud Storage tool. The data type is made slash-free so the key splits
//! back into its two parts; the identifier is kept as it is.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

/// Record and unit separators encode the metadata pairs into the one text
/// object: pairs split on record, key from value on unit.
const PAIR: char = '\u{1e}';
const KV: char = '\u{1f}';

/// What follows an item's key to name the object holding its metadata.
pub const META_SUFFIX: &str = ".meta";

/// The key for an item's bytes: `<prefix>/<data_type>/<identifier>`, or
/// `<data_type>/<identifier>` under an empty prefix.
#[must_use]
pub fn key(prefix: &str, data_type: &str, identifier: &str) -> String {
    let data_type = segment(data_type);
    match prefix.trim_matches('/') {
        "" => format!("{data_type}/{identifier}"),
        prefix => format!("{prefix}/{data_type}/{identifier}"),
    }
}

/// The key for the metadata beside the bytes at `key`.
#[must_use]
pub fn meta_key(key: &str) -> String {
    format!("{key}{META_SUFFIX}")
}

/// The data type and identifier a key under `prefix` names, or `None` when
/// the key is not laid out that way.
#[must_use]
pub fn split_key(prefix: &str, key: &str) -> Option<(String, String)> {
    let rest = match prefix.trim_matches('/') {
        "" => key,
        prefix => key.strip_prefix(prefix)?.strip_prefix('/')?,
    };
    let (data_type, identifier) = rest.split_once('/')?;
    (!data_type.is_empty() && !identifier.is_empty())
        .then(|| (data_type.to_string(), identifier.to_string()))
}

/// The metadata pairs as the one text the `.meta` object holds.
#[must_use]
pub fn encode_metadata(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{key}{KV}{value}"))
        .collect::<Vec<_>>()
        .join(&PAIR.to_string())
}

/// The pairs a `.meta` object holds.
#[must_use]
pub fn decode_metadata(encoded: &str) -> Vec<(String, String)> {
    if encoded.is_empty() {
        return Vec::new();
    }
    encoded
        .split(PAIR)
        .filter_map(|pair| pair.split_once(KV))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// SHA-256 of `bytes` as lower-case hex, the checksum a receipt carries.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// One key segment made slash-free: a data type like `text/plain` becomes
/// `text_plain`, so the key still splits into type and identifier.
fn segment(data_type: &str) -> String {
    data_type.replace('/', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_prefix_type_and_identifier_and_splits_back() {
        assert_eq!(key("retained", "json", "a#1"), "retained/json/a#1");
        assert_eq!(key("/retained/", "json", "a/b"), "retained/json/a/b");
        assert_eq!(key("", "text/plain", "n"), "text_plain/n");
        assert_eq!(meta_key("retained/json/a#1"), "retained/json/a#1.meta");
        assert_eq!(
            split_key("retained", "retained/json/a/b"),
            Some(("json".to_string(), "a/b".to_string()))
        );
        assert_eq!(
            split_key("", "json/n"),
            Some(("json".to_string(), "n".to_string()))
        );
        assert_eq!(split_key("retained", "elsewhere/json/n"), None);
        assert_eq!(split_key("retained", "retained/json"), None);
        assert_eq!(split_key("retained", "retained//n"), None);
    }

    #[test]
    fn metadata_pairs_survive_the_one_text() {
        let pairs = vec![
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "two words".to_string()),
        ];
        assert_eq!(decode_metadata(&encode_metadata(&pairs)), pairs);
        assert_eq!(encode_metadata(&[]), "");
        assert!(decode_metadata("").is_empty());
    }

    #[test]
    fn the_checksum_is_the_known_sha256() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
