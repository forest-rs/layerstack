// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The byte map a host supplies sources to.

use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use layerstack::encoded_bytes::EncodedBytes;
use layerstack_io::{IoError, IoErrorKind, Storage};

/// What the host said about one identifier.
#[derive(Clone, Debug)]
enum Source {
    /// The host supplied these bytes.
    Supplied(Arc<Vec<u8>>),
    /// A save wrote these bytes ([`Storage::write`]); never released.
    Written(Arc<Vec<u8>>),
    /// The host supplied bytes, the stage read them and the storage
    /// released them ([`Storage::release_source`]): reads fail as for a
    /// source not supplied yet.
    Released,
    /// The host confirmed there is no such source.
    Absent,
}

/// What a [`HostStorage`] released of the sources the host supplied
/// ([`HostStorage::released`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Released {
    /// The sources released.
    pub sources: u64,
    /// Their bytes, which nothing else held, so they were freed.
    pub bytes: u64,
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
/// # Lifetime of supplied bytes
///
/// The storage holds what the host supplies ([`Self::supply`]) until the
/// stage has read a layer from it: then, as the document tells it
/// ([`Storage::release_source`]), it drops its handle unless something
/// else holds the bytes. A layer that keeps views of its source (a lazily
/// read USDC layer, a USDZ package whose members are read later) shares
/// them, so the storage keeps them too, at no cost; a layer that decoded
/// its source into specs (USDA, an eagerly read USDC layer) does not, so
/// the bytes are freed as soon as the layer is read. Bytes the host
/// supplied but no read took yet, a source whose read failed, and bytes
/// [`StageDocument::read_asset_bytes`] read stay.
///
/// A released source reads as one the host has not supplied
/// ([`IoErrorKind::Unavailable`]): an arc that asks for it again (another
/// layer of the same source, under other format arguments) waits in
/// [`layerstack_io::StageDocument::unavailable_assets`] until the host
/// supplies it again; a reload that selects it fails with
/// [`IoErrorKind::Unavailable`] naming it and changes nothing, and a
/// dependency a reload of the root reaches but cannot read again keeps
/// the layer read before. [`Self::released`] counts what was released.
///
/// [`StageDocument::read_asset_bytes`]: layerstack_io::StageDocument::read_asset_bytes
///
/// A save writes into the same map ([`Storage::write`]): the saved bytes
/// become the source a later read sees, as a file would, are never
/// released, and are kept as written ([`Self::take_written`]) for the
/// host to persist.
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
    released: Released,
    /// What saves wrote since the host last took it: the last bytes
    /// written to each identifier. A save writes a few layers, so a vector,
    /// not another map's code.
    written: Vec<(String, EncodedBytes)>,
    written_bytes: u64,
}

impl HostStorage {
    /// Empty storage: every identifier is unavailable.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Supplies `bytes` as the source `identifier`, replacing any earlier
    /// answer, a released one included. A stage that wants it reads it at
    /// its next load ([`layerstack_io::StageDocument::load_requested_assets`]);
    /// a layer already read from it stays as it was read.
    pub fn supply(&mut self, identifier: &str, bytes: Vec<u8>) {
        self.supplied_bytes += bytes.len() as u64;
        self.sources
            .insert(identifier.to_string(), Source::Supplied(Arc::new(bytes)));
    }

    /// Records that `identifier` names no source: reads of it fail with
    /// [`IoErrorKind::NotFound`] from now on.
    pub fn confirm_absent(&mut self, identifier: &str) {
        self.sources.insert(identifier.to_string(), Source::Absent);
    }

    /// Whether the host answered `identifier`, with bytes the storage
    /// still holds or as absent: whether a read of it does not fail with
    /// [`IoErrorKind::Unavailable`]. A released source is not answered.
    #[must_use]
    pub fn is_answered(&self, identifier: &str) -> bool {
        self.sources
            .get(identifier)
            .is_some_and(|source| !matches!(source, Source::Released))
    }

    /// Whether the storage released the bytes supplied for `identifier`
    /// after the stage read them ([`Storage::release_source`]), and the
    /// host has not supplied it again since.
    #[must_use]
    pub fn is_released(&self, identifier: &str) -> bool {
        matches!(self.sources.get(identifier), Some(Source::Released))
    }

    /// The bytes the storage holds for `identifier`: supplied and not
    /// released, or written.
    #[must_use]
    pub fn bytes(&self, identifier: &str) -> Option<&[u8]> {
        match self.sources.get(identifier)? {
            Source::Supplied(bytes) | Source::Written(bytes) => Some(bytes),
            Source::Released | Source::Absent => None,
        }
    }

    /// The bytes supplied so far, summed over every [`Self::supply`]
    /// (a replaced or released source counts each time it is supplied).
    #[must_use]
    pub fn supplied_bytes(&self) -> u64 {
        self.supplied_bytes
    }

    /// What the storage released so far: the supplied sources the stage
    /// read and nothing else held, and their bytes.
    #[must_use]
    pub fn released(&self) -> Released {
        self.released
    }

    /// What saves wrote since the last call, in identifier order: the last
    /// bytes written to each identifier. They stay the identifiers' sources.
    pub fn take_written(&mut self) -> Vec<(String, EncodedBytes)> {
        let mut written = core::mem::take(&mut self.written);
        layerstack::sort::sort_unstable_by(&mut written, |a, b| a.0.cmp(&b.0));
        written
    }

    /// The bytes saves wrote so far, summed over every write.
    #[must_use]
    pub fn written_bytes(&self) -> u64 {
        self.written_bytes
    }

    fn source(&self, identifier: &str) -> Result<&Arc<Vec<u8>>, IoError> {
        match self.sources.get(identifier) {
            Some(Source::Supplied(bytes) | Source::Written(bytes)) => Ok(bytes),
            Some(Source::Absent) => Err(IoError::new(
                IoErrorKind::NotFound,
                "the host confirmed this source absent",
            )
            .at(identifier)),
            Some(Source::Released) => Err(IoError::new(
                IoErrorKind::Unavailable,
                "the host's bytes were released after the stage read them; supply them again",
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
        self.source(identifier)
            .map(|bytes| Arc::clone(bytes).into())
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
        Ok(Arc::clone(bytes).into())
    }

    /// Whether the host supplied `identifier`: `true` for a released
    /// source too, which exists though a read of it waits for the host to
    /// supply it again.
    fn exists(&self, identifier: &str) -> Result<bool, IoError> {
        match self.sources.get(identifier) {
            Some(Source::Supplied(_) | Source::Written(_) | Source::Released) => Ok(true),
            Some(Source::Absent) => Ok(false),
            None => Err(
                IoError::new(IoErrorKind::Unavailable, "the host has not supplied it yet")
                    .at(identifier),
            ),
        }
    }

    /// Drops the bytes supplied for `identifier` when nothing else holds
    /// them, recording it released ([`Self::released`]); keeps bytes a
    /// layer shares and written ones.
    fn release_source(&mut self, identifier: &str) {
        let Some(source) = self.sources.get_mut(identifier) else {
            return;
        };
        if let Source::Supplied(bytes) = source
            && Arc::strong_count(bytes) == 1
        {
            self.released.sources += 1;
            self.released.bytes += bytes.len() as u64;
            *source = Source::Released;
        }
    }

    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        let bytes = Arc::new(bytes.to_vec());
        self.written_bytes += bytes.len() as u64;
        let encoded = EncodedBytes::from(Arc::clone(&bytes));
        match self.written.iter_mut().find(|(at, _)| at == identifier) {
            Some((_, last)) => *last = encoded,
            None => self.written.push((identifier.to_string(), encoded)),
        }
        self.sources
            .insert(identifier.to_string(), Source::Written(bytes));
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

    #[test]
    fn writes_become_sources_and_are_kept_for_the_host() {
        let mut storage = HostStorage::new();
        storage.write("a.usda", b"one").unwrap();
        storage.write("a.usda", b"two").unwrap();
        assert_eq!(storage.read("a.usda").unwrap(), b"two");
        storage.write("b.usda", b"three").unwrap();
        let written = storage.take_written();
        assert_eq!(written.len(), 2);
        assert_eq!(
            (written[0].0.as_str(), &*written[0].1),
            ("a.usda", &b"two"[..])
        );
        assert_eq!(
            (written[1].0.as_str(), &*written[1].1),
            ("b.usda", &b"three"[..])
        );
        assert_eq!(storage.written_bytes(), 11);
        assert!(storage.take_written().is_empty(), "taken once");
        assert_eq!(storage.read("a.usda").unwrap(), b"two", "still the source");
    }

    /// A read source is released once nothing else holds its bytes, and
    /// reads as one not supplied until the host supplies it again; bytes a
    /// layer shares (a lazily read USDC layer's), and written ones, stay.
    #[test]
    fn read_sources_are_released_unless_shared() {
        let mut storage = HostStorage::new();
        storage.supply("a.usda", b"#usda 1.0\n".to_vec());
        storage.supply("b.usdc", b"PXR-USDC".to_vec());
        storage.write("c.usda", b"saved").unwrap();
        drop(storage.read_encoded("a.usda").unwrap());
        let shared = storage.read_encoded("b.usdc").unwrap();
        for identifier in ["a.usda", "b.usdc", "c.usda", "unknown.usda"] {
            storage.release_source(identifier);
        }
        assert_eq!(
            storage.released(),
            Released {
                sources: 1,
                bytes: 10
            }
        );

        assert!(storage.is_released("a.usda"));
        assert!(!storage.is_answered("a.usda"));
        assert_eq!(storage.bytes("a.usda"), None);
        let reread = storage.read_encoded("a.usda").unwrap_err();
        assert_eq!(reread.kind, IoErrorKind::Unavailable);
        assert_eq!(reread.identifier.as_deref(), Some("a.usda"));
        assert_eq!(storage.exists("a.usda").ok(), Some(true), "still exists");

        assert!(!storage.is_released("b.usdc"));
        assert!(
            storage
                .read_encoded("b.usdc")
                .unwrap()
                .shares_backing(&shared)
        );
        drop(shared);
        assert_eq!(storage.read("c.usda").unwrap(), b"saved");
        storage.release_source("c.usda");
        assert!(!storage.is_released("c.usda"), "written bytes stay");

        storage.supply("a.usda", b"#usda 1.0\n".to_vec());
        assert!(storage.is_answered("a.usda"));
        assert!(!storage.is_released("a.usda"));
        assert_eq!(storage.read("a.usda").unwrap(), b"#usda 1.0\n");
        assert_eq!(storage.supplied_bytes(), 28);
    }
}
