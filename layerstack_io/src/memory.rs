// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! In-memory byte transport for a document's sources.

use alloc::{borrow::Cow, collections::BTreeMap, format, string::String, vec::Vec};
use layerstack::{
    asset::{create_identifier, normalize_asset_path},
    encoded_bytes::EncodedBytes,
};

use crate::{ByteSink, IoError, IoErrorKind, Storage};

/// A byte-backed [`Storage`] available without the `std` feature.
///
/// Sources stay resident until replaced or the storage is dropped. An encoded
/// read or a clone of the storage shares immutable bytes. Later writes replace
/// the source in only the storage written to, so earlier readers and clones
/// can keep using the previous bytes.
///
/// Relative source names are rooted in this storage. Explicitly relative
/// references (`./` or `../`) resolve beside their authoring layer. Bare search
/// paths try that directory first, then the storage root. Sources that are
/// absent report [`IoErrorKind::NotFound`].
///
/// ```
/// use layerstack::{StageOptions, Transaction, Value, edit::EditTarget};
/// use layerstack_io::{MemoryStorage, StageDocument};
///
/// let mut storage = MemoryStorage::new();
/// storage.insert("scene.usda", b"#usda 1.0\ndef \"Root\" {\n int value = 1\n}\n".to_vec())?;
/// let mut document = StageDocument::open(storage, "scene.usda", StageOptions::default())?;
/// let root = document.root_layer();
/// let path = document.store().property_path("/Root.value");
/// let mut transaction = Transaction::new();
/// transaction.set_default(EditTarget::for_layer(root).property(path), Value::Int(2));
/// document.apply(&transaction).unwrap();
/// assert_eq!(document.save()?.saved, [root]);
/// let reopened = StageDocument::open(
///     document.storage().clone(), "scene.usda", StageOptions::default(),
/// )?;
/// let view = reopened.stage().stage().view(reopened.store());
/// let value = view.attribute("/Root.value").unwrap().get(layerstack::Time::Default).unwrap();
/// assert_eq!(value.value, Value::Int(2));
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
#[derive(Clone, Debug, Default)]
pub struct MemoryStorage {
    sources: BTreeMap<String, EncodedBytes>,
}

impl MemoryStorage {
    /// Creates empty storage.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Supplies bytes under `identifier`, replacing any earlier source there.
    /// An owned vector transfers its allocation. An [`EncodedBytes`] view
    /// retains its existing owner and byte range.
    ///
    /// # Errors
    /// Rejects an identifier that cannot name a source, such as an empty path
    /// or an unevaluated asset expression.
    pub fn insert(
        &mut self,
        identifier: &str,
        bytes: impl Into<EncodedBytes>,
    ) -> Result<(), IoError> {
        let identifier = self.identify(identifier, None)?;
        self.sources.insert(identifier, bytes.into());
        Ok(())
    }

    fn source(&self, identifier: &str) -> Result<&EncodedBytes, IoError> {
        self.sources.get(identifier).ok_or_else(|| {
            IoError::new(
                IoErrorKind::NotFound,
                "source is absent from memory storage",
            )
            .at(identifier)
        })
    }
}

impl Storage for MemoryStorage {
    fn identify(&self, asset: &str, anchor: Option<&str>) -> Result<String, IoError> {
        // Use OpenUSD's anchoring and existing-neighbor search rules (AOUSD
        // Core §9.4–§9.5). With no authoring layer, resolve from the map root.
        create_identifier(asset, anchor.unwrap_or("."), |beside| {
            self.sources.contains_key(storage_key(beside).as_ref())
        })
        .map(|identifier| storage_key(&identifier).into_owned())
        .filter(|identifier| !identifier.is_empty())
        .ok_or_else(|| {
            IoError::new(IoErrorKind::Rejected, "invalid memory source identifier").at(asset)
        })
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
        Ok(self.sources.contains_key(identifier))
    }

    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.insert(identifier, bytes.to_vec())
    }

    fn write_streamed(
        &mut self,
        identifier: &str,
        produce: &mut dyn FnMut(&mut dyn ByteSink) -> Result<(), IoError>,
    ) -> Result<(), IoError> {
        let identifier = self.identify(identifier, None)?;
        let mut bytes = Vec::new();
        produce(&mut bytes)?;
        self.sources.insert(identifier, bytes.into());
        Ok(())
    }
}

fn storage_key(identifier: &str) -> Cow<'_, str> {
    // The resolver keeps the distinction between an authored file-relative
    // path and a search path. Once resolved, both can name the same source.
    let path = identifier.strip_prefix("./").unwrap_or(identifier);
    // The asset helpers leave URIs opaque. In the source map, normalize the
    // path part only so a file-relative join applies to a URI directory too.
    if let Some((scheme, rest)) = path.split_once("://") {
        let (rest, suffix) = rest
            .find(['?', '#'])
            .map_or((rest, ""), |at| rest.split_at(at));
        if let Some((authority, uri_path)) = rest.split_once('/') {
            let normalized = normalize_asset_path(&format!("/{uri_path}"));
            return Cow::Owned(format!("{scheme}://{authority}{normalized}{suffix}"));
        }
    }
    Cow::Borrowed(path)
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use layerstack::{EditTarget, StageOptions, Time, Transaction, Value};

    use super::*;
    use crate::StageDocument;

    #[test]
    fn encoded_reads_share_a_bounded_view_across_replacement() {
        let mut storage = MemoryStorage::new();
        let owner = EncodedBytes::from(vec![0, 1, 2, 3, 4]);
        storage
            .insert("./dir/resource.bin", owner.slice(1..4).unwrap())
            .unwrap();
        let identifier = "dir/resource.bin";
        assert!(storage.exists(identifier).unwrap());
        assert!(!storage.exists("absent.bin").unwrap());
        let bytes = storage.read_encoded_bounded(identifier, 3).unwrap();
        assert!(bytes.shares_backing(&owner));
        assert_eq!(&*bytes, &[1, 2, 3]);
        assert_eq!(storage.read_bounded(identifier, 3).unwrap(), [1, 2, 3]);
        assert_eq!(
            storage
                .read_encoded_bounded(identifier, 2)
                .unwrap_err()
                .kind,
            IoErrorKind::Rejected,
        );
        assert_eq!(
            storage.read_bounded(identifier, 0).unwrap_err().kind,
            IoErrorKind::Rejected,
        );
        assert_eq!(
            storage.read_encoded("absent.bin").unwrap_err().kind,
            IoErrorKind::NotFound,
        );

        let snapshot = storage.clone();
        storage.write(identifier, &[8]).unwrap();
        assert_eq!(storage.read(identifier).unwrap(), [8]);
        assert!(
            snapshot
                .read_encoded(identifier)
                .unwrap()
                .shares_backing(&owner)
        );
        assert_eq!(&*bytes, &[1, 2, 3]);
    }

    #[test]
    fn streamed_writes_publish_only_when_the_producer_succeeds() {
        let mut storage = MemoryStorage::new();
        storage.insert("original", b"old".to_vec()).unwrap();
        let original = storage.read_encoded("original").unwrap();
        for identifier in ["original", "absent"] {
            let error = storage
                .write_streamed(identifier, &mut |sink| {
                    sink.write_bytes(b"partial")?;
                    Err(IoError::new(IoErrorKind::Rejected, "producer stopped"))
                })
                .unwrap_err();
            assert_eq!(error.message, "producer stopped");
        }
        assert!(
            storage
                .read_encoded("original")
                .unwrap()
                .shares_backing(&original)
        );
        assert!(!storage.exists("absent").unwrap());

        storage
            .write_streamed("original", &mut |sink| {
                sink.write_bytes(b"first ")?;
                sink.write_bytes(b"second")
            })
            .unwrap();
        assert_eq!(storage.read("original").unwrap(), b"first second");
        assert_eq!(&*original, b"old");
    }

    #[test]
    fn resolves_explicit_and_search_paths_using_the_source_namespace() {
        let mut storage = MemoryStorage::new();
        for identifier in [
            "scene/adjacent.usda",
            "global.usda",
            "mem://host/scene/adjacent.usda",
        ] {
            storage.insert(identifier, Vec::new()).unwrap();
        }
        let root = Some("scene/root.usda");
        for (authored, expected) in [
            ("adjacent.usda", "scene/adjacent.usda"),
            ("global.usda", "global.usda"),
            ("./global.usda", "scene/global.usda"),
            ("../extra.usda", "extra.usda"),
        ] {
            assert_eq!(storage.identify(authored, root).unwrap(), expected);
        }
        assert_eq!(
            storage
                .identify("adjacent.usda", Some("mem://host/scene/root.usda"))
                .unwrap(),
            "mem://host/scene/adjacent.usda",
        );
        assert_eq!(
            storage.identify("", root).unwrap_err().kind,
            IoErrorKind::Rejected
        );
    }

    #[test]
    fn a_stage_document_saves_and_reopens_from_its_memory_storage() {
        let mut storage = MemoryStorage::new();
        storage
            .insert(
                "scene/root.usda",
                b"#usda 1.0\n(subLayers = [@parts.usda@])\ndef \"Root\" {\n int value = 1\n}\n"
                    .to_vec(),
            )
            .unwrap();
        storage
            .insert(
                "scene/parts.usda",
                b"#usda 1.0\ndef \"Adjacent\" {}\n".to_vec(),
            )
            .unwrap();

        let mut document =
            StageDocument::open(storage, "scene/root.usda", StageOptions::default()).unwrap();
        let root = document.root_layer();
        let path = document.store().property_path("/Root.value");
        let mut transaction = Transaction::new();
        transaction.set_default(EditTarget::for_layer(root).property(path), Value::Int(2));
        document.apply(&transaction).unwrap();
        assert_eq!(document.save().unwrap().saved, [root]);
        assert!(!document.is_dirty(root));

        let reopened = StageDocument::open(
            document.storage().clone(),
            "scene/root.usda",
            StageOptions::default(),
        )
        .unwrap();
        let view = reopened.stage().stage().view(reopened.store());
        assert!(view.prim("/Adjacent").is_some());
        let value = view
            .attribute("/Root.value")
            .unwrap()
            .get(Time::Default)
            .unwrap();
        assert_eq!(value.value, Value::Int(2));
    }
}
