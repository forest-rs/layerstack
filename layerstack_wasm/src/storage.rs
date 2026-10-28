// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The byte map a host supplies sources to.

use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    vec::Vec,
};

use layerstack::encoded_bytes::EncodedBytes;
use layerstack_io::{IoError, IoErrorKind, Storage};

/// What the host said about one identifier.
#[derive(Clone, Debug)]
enum Source {
    /// The host supplied these bytes.
    Supplied(EncodedBytes),
    /// The host confirmed there is no such source.
    Absent,
}

/// A [`Storage`] the host fills: sources are bytes the host supplied under
/// an identifier, and nothing is fetched.
///
/// A read of an identifier the host has not answered yet fails with
/// [`IoErrorKind::Unavailable`], which leaves the arc or sublayer waiting
/// rather than missing ([`layerstack_io::StageDocument::unavailable_assets`]
/// lists it); one the host confirmed absent ([`Self::confirm_absent`])
/// fails with [`IoErrorKind::NotFound`], which composition reports as
/// missing. A USDZ package is one supplied source; its members resolve
/// inside it.
///
/// Identifiers are text: [`Storage::identify`] anchors a relative asset
/// path (`./a.usda`, `../b.usda`, `c.usda`) to its anchor's directory and
/// removes `.` and `..` segments, and keeps an absolute one (`/a.usda`, or
/// a URL with `://`) as written. There are no search paths. The host maps
/// identifiers to URLs or files however it likes.
#[derive(Clone, Debug, Default)]
pub struct HostStorage {
    sources: BTreeMap<String, Source>,
    supplied_bytes: u64,
}

impl HostStorage {
    /// Empty storage: every identifier is unavailable.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Supplies `bytes` as the source `identifier`, replacing any earlier
    /// answer. A stage that wants it reads it at its next load
    /// ([`layerstack_io::StageDocument::load_requested_assets`]); a layer
    /// already read from it stays as it was read.
    pub fn supply(&mut self, identifier: &str, bytes: Vec<u8>) {
        self.supplied_bytes += bytes.len() as u64;
        self.sources
            .insert(identifier.to_string(), Source::Supplied(bytes.into()));
    }

    /// Records that `identifier` names no source: reads of it fail with
    /// [`IoErrorKind::NotFound`] from now on.
    pub fn confirm_absent(&mut self, identifier: &str) {
        self.sources.insert(identifier.to_string(), Source::Absent);
    }

    /// Whether the host answered `identifier`, with bytes or as absent.
    #[must_use]
    pub fn is_answered(&self, identifier: &str) -> bool {
        self.sources.contains_key(identifier)
    }

    /// The bytes supplied for `identifier`, if any.
    #[must_use]
    pub fn bytes(&self, identifier: &str) -> Option<&[u8]> {
        match self.sources.get(identifier)? {
            Source::Supplied(bytes) => Some(bytes),
            Source::Absent => None,
        }
    }

    /// The bytes supplied so far, summed over every [`Self::supply`]
    /// (a replaced source counts each time).
    #[must_use]
    pub fn supplied_bytes(&self) -> u64 {
        self.supplied_bytes
    }

    fn source(&self, identifier: &str) -> Result<&EncodedBytes, IoError> {
        match self.sources.get(identifier) {
            Some(Source::Supplied(bytes)) => Ok(bytes),
            Some(Source::Absent) => Err(IoError::new(
                IoErrorKind::NotFound,
                "the host confirmed this source absent",
            )
            .at(identifier)),
            None => Err(
                IoError::new(IoErrorKind::Unavailable, "the host has not supplied it yet")
                    .at(identifier),
            ),
        }
    }
}

impl Storage for HostStorage {
    fn identify(&self, asset: &str, anchor: Option<&str>) -> Result<String, IoError> {
        Ok(identify(asset, anchor))
    }

    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.source(identifier).map(|bytes| bytes.to_vec())
    }

    fn read_encoded(&self, identifier: &str) -> Result<EncodedBytes, IoError> {
        self.source(identifier).cloned()
    }

    fn read_bounded(&self, identifier: &str, max_bytes: u64) -> Result<Vec<u8>, IoError> {
        self.read_encoded_bounded(identifier, max_bytes)
            .map(|bytes| bytes.to_vec())
    }

    fn read_encoded_bounded(
        &self,
        identifier: &str,
        max_bytes: u64,
    ) -> Result<EncodedBytes, IoError> {
        let bytes = self.source(identifier)?;
        if bytes.len() as u64 > max_bytes {
            return Err(IoError::new(
                IoErrorKind::Rejected,
                "resource exceeds encoded byte budget",
            )
            .at(identifier));
        }
        Ok(bytes.clone())
    }

    fn exists(&self, identifier: &str) -> Result<bool, IoError> {
        match self.source(identifier) {
            Ok(_) => Ok(true),
            Err(error) if error.kind == IoErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.sources.insert(
            identifier.to_string(),
            Source::Supplied(bytes.to_vec().into()),
        );
        Ok(())
    }
}

/// `asset` as an identifier, anchored to the directory of `anchor`.
fn identify(asset: &str, anchor: Option<&str>) -> String {
    if asset.starts_with('/') || asset.contains("://") {
        return normalize(asset);
    }
    match anchor.and_then(|anchor| anchor.rfind('/').map(|end| &anchor[..=end])) {
        Some(directory) => normalize(&[directory, asset].concat()),
        None => normalize(asset),
    }
}

/// `identifier` without `.` segments, and with each `..` segment removing
/// the segment before it (never the scheme and authority of a URL, nor a
/// leading `..` of a relative identifier).
fn normalize(identifier: &str) -> String {
    let (prefix, path) = match identifier.find("://") {
        Some(scheme_end) => {
            let authority_end = identifier[scheme_end + 3..]
                .find('/')
                .map_or(identifier.len(), |end| scheme_end + 3 + end);
            identifier.split_at(authority_end)
        }
        None => ("", identifier),
    };
    let absolute = path.starts_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." if segments.last().is_some_and(|last| *last != "..") => {
                segments.pop();
            }
            ".." if absolute => {}
            segment => segments.push(segment),
        }
    }
    let mut out = String::with_capacity(identifier.len());
    out.push_str(prefix);
    if absolute {
        out.push('/');
    }
    out.push_str(&segments.join("/"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_assets_anchor_to_the_anchor_directory() {
        assert_eq!(identify("./sub.usda", Some("scene.usda")), "sub.usda");
        assert_eq!(identify("sub.usda", Some("set/scene.usda")), "set/sub.usda");
        assert_eq!(identify("../a/b.usda", Some("set/scene.usda")), "a/b.usda");
        assert_eq!(identify("../b.usda", Some("scene.usda")), "../b.usda");
        assert_eq!(identify("./x/./y.usda", None), "x/y.usda");
        assert_eq!(
            identify("/abs/./c.usda", Some("set/scene.usda")),
            "/abs/c.usda"
        );
        assert_eq!(identify("../../c.usda", Some("/a/scene.usda")), "/c.usda");
        assert_eq!(
            identify("../tex/d.usda", Some("https://host/set/scene.usda")),
            "https://host/tex/d.usda"
        );
        assert_eq!(
            identify("../../../d.usda", Some("https://host/set/scene.usda")),
            "https://host/d.usda"
        );
    }

    #[test]
    fn reads_distinguish_unavailable_from_absent() {
        let mut storage = HostStorage::new();
        let unavailable = storage.read("a.usda").unwrap_err();
        assert_eq!(unavailable.kind, IoErrorKind::Unavailable);
        assert_eq!(unavailable.identifier.as_deref(), Some("a.usda"));
        storage.confirm_absent("a.usda");
        assert_eq!(
            storage.read("a.usda").unwrap_err().kind,
            IoErrorKind::NotFound
        );
        assert_eq!(storage.exists("a.usda").ok(), Some(false));
        storage.supply("a.usda", b"#usda 1.0\n".to_vec());
        assert_eq!(storage.read("a.usda").unwrap(), b"#usda 1.0\n");
        assert_eq!(storage.exists("a.usda").ok(), Some(true));
        assert_eq!(storage.supplied_bytes(), 10);
        assert_eq!(
            storage.read_bounded("a.usda", 4).unwrap_err().kind,
            IoErrorKind::Rejected
        );
        assert!(storage.is_answered("a.usda"));
        assert!(!storage.is_answered("b.usda"));
    }
}
