#![forbid(unsafe_code)]

//! Google Cloud Storage archive: an [`ArchiveStore`] that keeps each
//! retained item as one object in a bucket, its metadata as a second object
//! beside it, and restores the item by getting both back.
//!
//! A xmip-core-archive **technology** (repository-model.md): it depends on
//! the archive capability for the [`ArchiveStore`] trait and its item,
//! receipt and error types, and on the Cloud Storage transport technology
//! for the requests — the JSON API with a bearer token, one connection a
//! call. Obtaining the token is outside this crate, as it is outside the
//! transport's: the archive is configured with one. The same four fields
//! every archive technology carries — `data_type`, `identifier`, `bytes`,
//! `metadata` — are laid out as `object.rs` says: the bytes at
//! `<prefix>/<data_type>/<identifier>`, the metadata text at the same name
//! with `.meta` appended.
//!
//! An archive never deletes (ADR-0040): this one uploads and gets, nothing
//! else. The receipt is `gcs://<bucket>/<object>` with the SHA-256 of the
//! bytes as its checksum, and restoring checks the bytes that come back
//! against it.

pub mod object;

use std::time::Duration;

use archive::{ArchiveError, ArchiveItem, ArchiveReceipt, ArchiveStore};
use gcs::Client;

/// An archive that keeps items as objects under one prefix of one bucket.
pub struct GcsArchive {
    endpoint: String,
    token: String,
    bucket: String,
    prefix: String,
    timeout: Option<Duration>,
}

impl GcsArchive {
    /// An archive writing to `bucket` at `endpoint` —
    /// `https://storage.googleapis.com` in the cloud, `http://host:port` for
    /// an emulator — presenting `token` as the bearer, under no prefix.
    #[must_use]
    pub fn new(
        endpoint: impl Into<String>,
        token: impl Into<String>,
        bucket: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            token: token.into(),
            bucket: bucket.into(),
            prefix: String::new(),
            timeout: None,
        }
    }

    /// The prefix every object name is laid out under, `retained` say.
    #[must_use]
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Give up on an endpoint that stops answering.
    #[must_use]
    pub const fn timing_out_after(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    fn client(&self) -> Result<Client, ArchiveError> {
        let client = Client::new(&self.endpoint, &self.token).map_err(error)?;
        Ok(match self.timeout {
            Some(timeout) => client.timing_out_after(timeout),
            None => client,
        })
    }
}

impl ArchiveStore for GcsArchive {
    fn archive(&self, item: ArchiveItem) -> Result<ArchiveReceipt, ArchiveError> {
        let name = object::key(&self.prefix, &item.data_type, &item.identifier);
        let metadata = object::encode_metadata(&item.metadata);
        let client = self.client()?;
        client
            .put(&self.bucket, &name, &item.bytes)
            .map_err(error)?;
        client
            .put(&self.bucket, &object::meta_key(&name), metadata.as_bytes())
            .map_err(error)?;
        Ok(ArchiveReceipt {
            location: format!("gcs://{}/{name}", self.bucket),
            checksum: Some(object::sha256_hex(&item.bytes)),
        })
    }

    fn restore(&self, receipt: &ArchiveReceipt) -> Result<ArchiveItem, ArchiveError> {
        let (bucket, name) = parse_location(&receipt.location)?;
        let (data_type, identifier) =
            object::split_key(&self.prefix, name).ok_or_else(|| ArchiveError {
                message: format!(
                    "{name} is not laid out as {}/<data_type>/<identifier>",
                    self.prefix
                ),
            })?;
        let client = self.client()?;
        let bytes = client.get(bucket, name).map_err(error)?;
        if let Some(expected) = &receipt.checksum {
            let actual = object::sha256_hex(&bytes);
            if &actual != expected {
                return Err(ArchiveError {
                    message: format!(
                        "{}: the bytes came back with checksum {actual}, not {expected}",
                        receipt.location
                    ),
                });
            }
        }
        let metadata = client.get(bucket, &object::meta_key(name)).map_err(error)?;
        let metadata = String::from_utf8(metadata).map_err(error)?;
        Ok(ArchiveItem {
            data_type,
            identifier,
            bytes,
            metadata: object::decode_metadata(&metadata),
        })
    }
}

/// The bucket and object a receipt names: `gcs://<bucket>/<object>`.
fn parse_location(location: &str) -> Result<(&str, &str), ArchiveError> {
    location
        .strip_prefix("gcs://")
        .and_then(|rest| rest.split_once('/'))
        .filter(|(bucket, name)| !bucket.is_empty() && !name.is_empty())
        .ok_or_else(|| ArchiveError {
            message: format!("{location} is not gcs://bucket/object"),
        })
}

fn error(cause: impl std::fmt::Display) -> ArchiveError {
    ArchiveError {
        message: cause.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gcs::{Event, Session};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn item(id: &str) -> ArchiveItem {
        ArchiveItem {
            data_type: "json".to_string(),
            identifier: id.to_string(),
            bytes: b"{\"kept\":true}".to_vec(),
            metadata: vec![("source".to_string(), "playground".to_string())],
        }
    }

    /// A far end that answers `requests` bearing the token, one connection
    /// each, and then hands back what it holds and what it saw.
    fn far_end(requests: usize) -> (String, JoinHandle<(Session, Vec<Event>)>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address").to_string();
        let handle = std::thread::spawn(move || {
            let mut session = Session::new("ya29.token").timing_out_after(secs(2));
            let events = (0..requests)
                .map(|_| session.serve_one(&listener).expect("served"))
                .collect();
            (session, events)
        });
        (address, handle)
    }

    fn store(address: &str) -> GcsArchive {
        GcsArchive::new(format!("http://{address}"), "ya29.token", "orders-archive")
            .with_prefix("retained")
            .timing_out_after(secs(2))
    }

    #[test]
    fn an_archived_item_is_two_objects_and_the_receipt_names_the_first() {
        let (address, far_end) = far_end(2);
        let original = item("json#1");
        let receipt = store(&address).archive(original.clone()).expect("archive");
        assert_eq!(
            receipt.location,
            "gcs://orders-archive/retained/json/json#1"
        );
        assert_eq!(
            receipt.checksum.as_deref(),
            Some(object::sha256_hex(&original.bytes).as_str())
        );
        let (session, events) = far_end.join().expect("thread");
        let held = session.objects();
        assert_eq!(held.len(), 2);
        assert_eq!(
            held.get("orders-archive/retained/json/json#1"),
            Some(&original.bytes)
        );
        assert_eq!(
            held.get("orders-archive/retained/json/json#1.meta"),
            Some(&b"source\x1fplayground".to_vec())
        );
        assert!(matches!(&events[0], Event::Stored(arrived)
            if arrived.origin_uri == "gs://orders-archive/retained/json/json#1"));
    }

    #[test]
    fn the_objects_restore_the_item_through_the_same_session() {
        let (address, far_end) = far_end(4);
        let store = store(&address);
        let original = item("json#2");
        let receipt = store.archive(original.clone()).expect("archive");
        let restored = store.restore(&receipt).expect("restore");
        assert_eq!(restored, original, "both objects read back give the item");
        let (_, events) = far_end.join().expect("thread");
        assert_eq!(
            events[2],
            Event::Retrieved("gs://orders-archive/retained/json/json#2".to_string())
        );
        assert_eq!(
            events[3],
            Event::Retrieved("gs://orders-archive/retained/json/json#2.meta".to_string())
        );
    }

    #[test]
    fn a_missing_object_a_changed_checksum_a_wrong_token_and_a_foreign_receipt_are_refused() {
        let (address, far_end) = far_end(5);
        let store = store(&address);
        let mut receipt = store.archive(item("json#3")).expect("archive");
        receipt.checksum = Some("0".repeat(64));
        let changed = store.restore(&receipt).expect_err("checksum");
        assert!(changed.message.contains("not 0000"), "{changed}");
        receipt.location = "gcs://orders-archive/retained/json/none".to_string();
        receipt.checksum = None;
        let missing = store.restore(&receipt).expect_err("missing");
        assert!(missing.message.contains("404 No such object"), "{missing}");
        let other = GcsArchive::new(format!("http://{address}"), "other", "orders-archive")
            .timing_out_after(secs(2));
        let refused = other.archive(item("json#4")).expect_err("wrong token");
        assert!(refused.message.contains("401"), "{refused}");
        far_end.join().expect("thread");
        for location in [
            "s3://bucket/key",
            "gcs://bucket",
            "gcs://orders-archive/elsewhere/n",
        ] {
            receipt.location = location.to_string();
            assert!(store.restore(&receipt).is_err(), "{location}");
        }
    }
}
