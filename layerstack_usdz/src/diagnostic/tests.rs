// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{
    ImportDiagnostic, LayerReadError, PackageFile, UsdzError, UsdzResult, read_usdz, write_usdz,
};
use alloc::{sync::Arc, vec};
use layerstack::{
    AssetResolveError, AssetResolver, LayerId, PathInterner, ResolvedAsset, TokenInterner,
};
use layerstack_usdc::{
    UsdcError,
    writer::{Spec, SpecForm, Value, write_crate},
};

struct Outside(u64);
impl AssetResolver for Outside {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
    fn allocate_layer_id(&mut self) -> Option<LayerId> {
        self.0 += 1;
        Some(LayerId(self.0))
    }
}
fn read(files: &[PackageFile<'_>]) -> Result<UsdzResult, UsdzError> {
    read_usdz(
        &write_usdz(files).unwrap(),
        LayerId(1),
        &mut TokenInterner::default(),
        &mut PathInterner::default(),
        &mut Outside(1),
    )
}
const ROOT: &[u8] = b"#usda 1.0\ndef \"A\" (references = @bad.usdc@</Rock>) {}\n";

#[test]
fn hard_decoder_errors_keep_the_same_cause_at_root_and_in_a_member() {
    let mut bytes = write_crate(&[Spec::new("/", SpecForm::PseudoRoot)]).unwrap();
    bytes[9] = 255;
    for (files, member, id) in [
        (
            vec![PackageFile::new("root.usdc", &bytes)],
            "root.usdc",
            LayerId(1),
        ),
        (
            vec![
                PackageFile::new("root.usda", ROOT),
                PackageFile::new("bad.usdc", &bytes),
            ],
            "bad.usdc",
            LayerId(2),
        ),
    ] {
        let error = read(&files).unwrap_err();
        assert_eq!(
            error,
            UsdzError::LayerRead {
                member: Arc::from(member),
                layer_id: id,
                cause: LayerReadError::Usdc(UsdcError::UnsupportedVersion {
                    major: 0,
                    minor: 255,
                    patch: 0
                }),
            }
        );
        let source = core::error::Error::source(&error).unwrap();
        assert!(source.source().unwrap().is::<UsdcError>());
    }
}

#[test]
fn malformed_text_keeps_each_pipeline_phases_evidence() {
    for (text, phase) in [
        ("#usda 1.0\ndef \"Rock\" { float x = }", 0),
        ("#usda 1.0\ndef \"Rock\" { string label = \"\\xff\" }", 1),
        ("#usda 1.0\ndef \"Rock\" { float amount = \"wrong\" }", 2),
    ] {
        let cst = layerstack_usda::parser::parse_cst(text);
        let ast = layerstack_usda::lower::lower(&cst.tree, text);
        let emitted = layerstack_usda::emit::emit(
            &ast.layer,
            LayerId(1),
            &mut TokenInterner::default(),
            &mut PathInterner::default(),
            &mut Outside(1),
        );
        let expected = [&cst.diagnostics, &ast.diagnostics, &emitted.diagnostics][phase];
        assert!(
            !expected.is_empty(),
            "the intended phase reports the problem"
        );
        let result = read(&[PackageFile::new("root.usda", text.as_bytes())]).unwrap();
        assert!(result.has_errors());
        let actual: vec::Vec<_> = result
            .diagnostics
            .iter()
            .filter_map(|d| {
                assert_eq!(&*d.member, "root.usda");
                assert_eq!(d.layer_id, LayerId(1));
                match (&d.diagnostic, phase) {
                    (ImportDiagnostic::UsdaParse(d), 0)
                    | (ImportDiagnostic::UsdaLower(d), 1)
                    | (ImportDiagnostic::UsdaEmit(d), 2) => Some(d),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(actual.len(), expected.len());
        for (a, b) in actual.into_iter().zip(expected) {
            assert_eq!(a.span, b.span);
            assert_eq!(a.message, b.message);
            assert_eq!(a.severity, b.severity);
        }
    }
}

#[test]
fn descendant_diagnostics_are_attributed_once_even_with_repeated_references() {
    let root = b"#usda 1.0\ndef \"A\" (references = @wrapper.usda@</W>) {}\ndef \"B\" (references = @bad.usda@</Rock>) {}\n";
    let wrapper = b"#usda 1.0\ndef \"W\" (references = @bad.usda@</Rock>) {}\n";
    let bad = b"#usda 1.0\ndef \"Rock\" { float amount = \"wrong\" }\n";
    let result = read(&[
        PackageFile::new("root.usda", root),
        PackageFile::new("wrapper.usda", wrapper),
        PackageFile::new("bad.usda", bad),
    ])
    .unwrap();
    assert!(result.has_errors());
    assert_eq!(result.diagnostics.len(), 1);
    let d = &result.diagnostics[0];
    assert_eq!(&*d.member, "bad.usda");
    assert_eq!(result.member_paths.get(&d.layer_id).unwrap(), &d.member);
    assert!(matches!(d.diagnostic, ImportDiagnostic::UsdaEmit(_)));
}

#[test]
fn usdc_assembly_losses_survive_root_and_member_import() {
    let bytes = write_crate(&[
        Spec::new("/", SpecForm::PseudoRoot),
        Spec::new("/Rock", SpecForm::Prim).with_field("relocates", Value::String("legacy".into())),
    ])
    .unwrap();
    let direct = layerstack_usdc::read_usdc(
        &bytes,
        LayerId(1),
        &mut TokenInterner::default(),
        &mut PathInterner::default(),
        &mut Outside(1),
    )
    .unwrap();
    assert!(!direct.diagnostics.is_empty());
    for files in [
        vec![PackageFile::new("root.usdc", &bytes)],
        vec![
            PackageFile::new("root.usda", ROOT),
            PackageFile::new("bad.usdc", &bytes),
        ],
    ] {
        let result = read(&files).unwrap();
        assert!(result.has_errors());
        let losses: vec::Vec<_> = result
            .diagnostics
            .iter()
            .filter_map(|d| {
                assert_eq!(result.member_paths.get(&d.layer_id).unwrap(), &d.member);
                if let ImportDiagnostic::UsdcAssemble(d) = &d.diagnostic {
                    Some(d)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(losses, direct.diagnostics.iter().collect::<vec::Vec<_>>());
    }
}

#[test]
fn clean_reads_and_warning_policy_remain_distinct() {
    let result = read(&[PackageFile::new(
        "root.usda",
        b"#usda 1.0\ndef \"Rock\" {}\n",
    )])
    .unwrap();
    assert!(result.diagnostics.is_empty());
    assert!(!result.has_errors());
    let warning = ImportDiagnostic::UsdaEmit(layerstack_usda::diagnostic::Diagnostic::warning(
        layerstack_usda::Span::new(0, 0),
        "warning",
    ));
    assert!(!warning.is_error());
}

#[test]
fn invalid_utf8_preserves_the_offset_and_member() {
    let error = read(&[PackageFile::new("root.usda", b"#usda 1.0\n\xff")]).unwrap_err();
    let UsdzError::LayerRead {
        member,
        cause: LayerReadError::InvalidUtf8(error),
        ..
    } = error
    else {
        panic!("UTF-8 error")
    };
    assert_eq!(&*member, "root.usda");
    assert_eq!(error.valid_up_to(), 10);
}
