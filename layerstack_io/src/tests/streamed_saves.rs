// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDA saves stream through [`Storage::write_streamed`]: the storage
//! receives the text `save_usda` writes, in pieces, and keeps an existing
//! source when a piece fails.

use super::*;

/// A storage committing a streamed write only when it completes, its
/// sink refusing the bytes past `fail_after`.
#[derive(Default)]
struct Streaming {
    files: BTreeMap<String, Vec<u8>>,
    fail_after: Option<usize>,
    streamed_writes: usize,
    pieces: usize,
}

/// The sink of one [`Streaming`] write.
struct Staged<'s> {
    bytes: Vec<u8>,
    fail_after: Option<usize>,
    pieces: &'s mut usize,
}

impl ByteSink for Staged<'_> {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), IoError> {
        *self.pieces += 1;
        if self
            .fail_after
            .is_some_and(|limit| self.bytes.len() + bytes.len() > limit)
        {
            return Err(IoError::new(IoErrorKind::Storage, "injected sink failure"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

impl Storage for Streaming {
    fn identify(&self, asset: &str, _: Option<&str>) -> Result<String, IoError> {
        Ok(asset.into())
    }
    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.files
            .get(identifier)
            .cloned()
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, identifier))
    }
    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.files.insert(identifier.into(), bytes.to_vec());
        Ok(())
    }
    fn write_streamed(
        &mut self,
        identifier: &str,
        produce: &mut dyn FnMut(&mut dyn ByteSink) -> Result<(), IoError>,
    ) -> Result<(), IoError> {
        self.streamed_writes += 1;
        let mut staged = Staged {
            bytes: Vec::new(),
            fail_after: self.fail_after,
            pieces: &mut self.pieces,
        };
        produce(&mut staged)?;
        let bytes = staged.bytes;
        self.files.insert(identifier.into(), bytes);
        Ok(())
    }
}

/// A root layer whose text is several streaming pieces long.
fn large_root() -> String {
    let mut source = String::from("#usda 1.0\n\ndef \"Root\"\n{\n");
    for i in 0..3_000 {
        source.push_str(&format!(
            "    def Xform \"Child{i}\"\n    {{\n        double3 xformOp:translate = ({i}, 0.5, -1)\n        uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    }}\n"
        ));
    }
    source.push_str("}\n");
    source
}

fn open(storage: Streaming) -> StageDocument<Streaming> {
    StageDocument::open(storage, "root.usda", StageOptions::default()).unwrap()
}

#[test]
fn usda_saves_stream_the_text_save_usda_writes() {
    let mut storage = Streaming::default();
    storage
        .files
        .insert("root.usda".into(), large_root().into_bytes());
    let mut document = open(storage);
    let root = document.stage().stage().root_layer().unwrap();
    let expected = layerstack_usda::save::save_usda(
        document.store().layer(root).unwrap(),
        document.store().tokens(),
        document.store().paths(),
    )
    .unwrap();
    document.export_layer(root, "copy.usda").unwrap();
    let storage = document.storage();
    assert_eq!(storage.streamed_writes, 1);
    assert!(storage.pieces > 2, "{} pieces", storage.pieces);
    assert_eq!(storage.files["copy.usda"], expected.as_bytes());
}

#[test]
fn a_failing_sink_keeps_the_existing_source_and_reports_its_error() {
    let mut storage = Streaming::default();
    storage
        .files
        .insert("root.usda".into(), large_root().into_bytes());
    storage
        .files
        .insert("copy.usda".into(), b"previous text".to_vec());
    storage.fail_after = Some(100_000);
    let mut document = open(storage);
    let root = document.stage().stage().root_layer().unwrap();
    let error = document.export_layer(root, "copy.usda").unwrap_err();
    assert_eq!(error.kind, IoErrorKind::Storage);
    assert_eq!(error.message, "injected sink failure");
    assert_eq!(error.identifier.as_deref(), Some("copy.usda"));
    let storage = document.storage();
    assert_eq!(storage.files["copy.usda"], b"previous text");
    // The sink was handed nothing after the piece it refused.
    assert!(
        storage.pieces >= 2 && storage.pieces < 10,
        "{}",
        storage.pieces
    );
}

#[test]
fn a_layer_usda_cannot_hold_is_refused_before_the_storage_writes() {
    let mut storage = Streaming::default();
    // An empty array in a dictionary records no element type to write.
    storage.files.insert(
        "root.usda".into(),
        b"#usda 1.0\ndef \"A\" (\n    customData = {\n        asset[] e = []\n    }\n)\n{\n}\n"
            .to_vec(),
    );
    let mut document = open(storage);
    let root = document.stage().stage().root_layer().unwrap();
    let error = document.export_layer(root, "copy.usda").unwrap_err();
    assert_eq!(error.kind, IoErrorKind::Unsupported, "{error}");
    assert_eq!(document.storage().streamed_writes, 0);
    assert!(!document.storage().files.contains_key("copy.usda"));
}
