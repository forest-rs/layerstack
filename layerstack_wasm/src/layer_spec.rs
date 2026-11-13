// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What one layer authors at one path: the spec there, read from the
//! layer's own data, with no composition and no export.
//!
//! [`Stage::layer_spec`] reads the pseudo-root (`/`), a prim spec, a
//! variant spec (`/Robot{finish=painted}`), a prim spec inside a variant
//! (`/Robot{finish=painted}Trim`) or a property spec (`/Robot.height`,
//! `/Robot{finish=painted}.color`), as OpenUSD's `SdfLayer::GetPseudoRoot`,
//! `GetPrimAtPath` and `GetPropertyAtPath` find them. Its record names the
//! fields the spec authors by their OpenUSD keys (`SdfFieldKeys`, the names
//! a change's `primFields` uses), and gives the fields an editor acts on as
//! data: list ops with each of their lists apart, variant selections,
//! variant sets, children, sublayers, time sample times. Values are not
//! read: an attribute's default says only whether it is a value or a
//! block, so a large or retained value is never decoded here.
//!
//! Spec: AOUSD Core §7.3 (specs and their paths), §7.6 (spec fields),
//! §12.4 (list ops).

use alloc::{string::String, vec, vec::Vec};

use layerstack::{
    FieldEntry, FieldValue, InMemoryStore, Layer, LayerId, ListOp, Path, PathId, PrimSpec,
    PropertyKind, PropertySpec, Reference, ReferenceSource, ReferenceTarget, SparseListOp,
    Specifier, TokenId, TokenInterner, Value, Variability, VariantSelections, VariantSets,
    VariantSpec, get_field,
};

use crate::{error::Error, stage::Stage, wire::Wire};

/// A prim spec or a variant spec: the two hold the same kinds of fields
/// (AOUSD Core §7.6.7: a variant spec contributes prim spec fields).
#[derive(Clone, Copy)]
enum PrimLike<'a> {
    Prim(&'a PrimSpec),
    Variant(&'a VariantSpec),
}

impl<'a> PrimLike<'a> {
    fn fields(self) -> &'a [FieldEntry] {
        match self {
            Self::Prim(spec) => &spec.fields,
            Self::Variant(spec) => &spec.fields,
        }
    }

    fn references(self) -> Option<&'a ListOp<Reference>> {
        match self {
            Self::Prim(spec) => authored(&spec.references),
            Self::Variant(spec) => authored(&spec.references),
        }
    }

    fn payloads(self) -> Option<&'a ListOp<Reference>> {
        match self {
            Self::Prim(spec) => authored(&spec.payloads),
            Self::Variant(spec) => authored(&spec.payloads),
        }
    }

    fn inherits(self) -> Option<&'a ListOp<PathId>> {
        match self {
            Self::Prim(spec) => authored(&spec.rare()?.inherits),
            Self::Variant(spec) => authored(&spec.inherits),
        }
    }

    fn specializes(self) -> Option<&'a ListOp<PathId>> {
        match self {
            Self::Prim(spec) => authored(&spec.rare()?.specializes),
            Self::Variant(spec) => authored(&spec.specializes),
        }
    }

    fn variant_set_names(self) -> Option<&'a ListOp<TokenId>> {
        match self {
            Self::Prim(spec) => authored(&spec.rare()?.variant_set_names),
            Self::Variant(spec) => authored(&spec.variant_set_names),
        }
    }

    fn variant_selections(self) -> Option<&'a VariantSelections> {
        let selections = match self {
            Self::Prim(spec) => spec.variant_selections()?,
            Self::Variant(spec) => &spec.variant_selections,
        };
        (!selections.is_empty()).then_some(selections)
    }

    fn variant_sets(self) -> &'a VariantSets {
        match self {
            Self::Prim(spec) => spec.variant_sets(),
            Self::Variant(spec) => &spec.variant_sets,
        }
    }

    fn children(self) -> &'a [TokenId] {
        match self {
            Self::Prim(spec) => &spec.authored_children,
            Self::Variant(spec) => &spec.authored_children,
        }
    }

    fn property_names(self) -> impl Iterator<Item = TokenId> + 'a {
        let properties = match self {
            Self::Prim(spec) => &spec.properties,
            Self::Variant(spec) => &spec.properties,
        };
        properties.iter().map(|entry| entry.name)
    }

    fn has_prim_order(self) -> bool {
        match self {
            Self::Prim(spec) => spec.prim_order().is_some(),
            Self::Variant(spec) => spec.prim_order.is_some(),
        }
    }

    fn has_property_order(self) -> bool {
        match self {
            Self::Prim(spec) => spec.property_order().is_some(),
            Self::Variant(spec) => spec.property_order.is_some(),
        }
    }

    /// `active` or `instanceable`: a prim spec's own member, else a field
    /// a variant spec holds by that name.
    fn flag(self, name: &str, tokens: &TokenInterner) -> Option<bool> {
        let own = match self {
            Self::Prim(spec) if name == "active" => spec.active,
            Self::Prim(spec) => spec.instanceable,
            Self::Variant(_) => None,
        };
        own.or_else(|| match field(self.fields(), name, tokens)? {
            Value::Bool(value) => Some(*value),
            _ => None,
        })
    }
}

/// The list op `op` holds when one is authored, even if emptied since,
/// as OpenUSD keeps a list op field whose items an edit removed.
fn authored<T: 'static>(op: &SparseListOp<T>) -> Option<&ListOp<T>> {
    op.is_authored().then(|| &**op)
}

/// The plain value of the field `name` among `fields`.
fn field<'a>(fields: &'a [FieldEntry], name: &str, tokens: &TokenInterner) -> Option<&'a Value> {
    get_field(fields, &tokens.lookup(name)?)?.as_value()
}

/// The text of a field holding a token or a string (`kind`,
/// `documentation`), or `null`.
fn text_field(fields: &[FieldEntry], name: &str, tokens: &TokenInterner) -> Wire {
    match field(fields, name, tokens) {
        Some(Value::Token(token)) => Wire::text(tokens.resolve(*token)),
        Some(Value::String(text)) => Wire::text(&**text),
        _ => Wire::Null,
    }
}

/// The record of a list op, `[explicit, prepended, appended, deleted,
/// added, ordered]`, each list its items as `item` encodes them and
/// `explicit` `null` when the op is not explicit.
fn list_op<T>(op: &ListOp<T>, item: impl Fn(&T) -> Wire) -> Wire {
    let list = |items: &[T]| Wire::List(items.iter().map(&item).collect());
    Wire::List(vec![
        op.explicit.as_deref().map_or(Wire::Null, list),
        list(&op.prepend),
        list(&op.append),
        list(&op.delete),
        list(&op.add),
        list(&op.reorder),
    ])
}

/// The record of a list op, or `null` for none.
fn list_op_or_null<T>(op: Option<&ListOp<T>>, item: impl Fn(&T) -> Wire) -> Wire {
    op.map_or(Wire::Null, |op| list_op(op, item))
}

/// Field names, sorted, each once.
fn sorted_names(mut names: Vec<String>) -> Wire {
    names.sort_unstable();
    names.dedup();
    Wire::List(names.into_iter().map(Wire::Text).collect())
}

/// The names of `tokens`, in order.
fn names(ids: impl Iterator<Item = TokenId>, tokens: &TokenInterner) -> Wire {
    Wire::List(ids.map(|id| Wire::text(tokens.resolve(id))).collect())
}

impl Stage {
    /// The spec layer `id` authors at `path`, a path in the layer's own
    /// namespace, or `null` when it authors none there. `path` is the
    /// pseudo-root (`/`), a prim path, a variant path
    /// (`/Robot{finish=painted}`, nested `/Robot{a=x}{b=y}`), a prim path
    /// inside variants (`/Robot{finish=painted}Trim`) or a property path on
    /// any of those but the pseudo-root. The pseudo-root is never `null`:
    /// every layer has one (`SdfLayer::GetPseudoRoot`).
    ///
    /// The record is `[type, path, fields, ...]`: `type` is `"pseudoRoot"`,
    /// `"prim"`, `"variant"`, `"attribute"` or `"relationship"`, `path` the
    /// spec path as the core spells it, `fields` the OpenUSD keys of the
    /// fields the spec authors, sorted, children lists (`primChildren`,
    /// `properties`, `variantSetChildren`) left out. Then, by type:
    ///
    /// - pseudo-root: `defaultPrim`, `documentation`, sublayers (as
    ///   [`Self::layer_sublayers`] encodes them), root prim names.
    /// - prim or variant: `specifier`, `typeName`, `kind`, `active`,
    ///   `instanceable`, `documentation`, then the list ops `references`,
    ///   `payload`, `inheritPaths`, `specializes`, `apiSchemas`,
    ///   `variantSetNames` (each `null` when not authored; a reference or
    ///   payload item is `[asset, primPath, offset, scale]`), the variant
    ///   selections `[[set, variant], ...]` sorted by set (`null` for
    ///   none), the variant sets `[[set, [variant, ...]], ...]` in
    ///   authored order, child prim names and property names in authored
    ///   order. A variant has no `specifier` or `typeName`.
    /// - attribute: `typeName`, `custom`, `variability`, the default
    ///   (`"value"`, `"block"`, `"animationBlock"`, or `null` for none),
    ///   the time sample times in layer time (`null` when no `timeSamples`
    ///   field is authored), `connectionPaths`.
    /// - relationship: `custom`, `targetPaths`.
    ///
    /// A list op record is `[explicit, prepended, appended, deleted,
    /// added, ordered]`, `explicit` `null` when the op is not explicit.
    /// Nothing is composed, exported or walked: the read looks the spec up
    /// and reads its fields. Values are not decoded, so a retained
    /// (lazily read) value stays encoded.
    ///
    /// Spec: AOUSD Core §7.3.5–§7.3.7 (prim, variant and property specs
    /// and their paths), §7.6.1 (layer fields: `subLayers`, `defaultPrim`
    /// §7.6.1.2.3, `layerRelocates` §7.6.1.2.4), §7.6.2 (prim fields:
    /// `variantSelection` §7.6.2.3.4, `variantSetNames` §7.6.2.3.5),
    /// §7.6.3–§7.6.5 (property fields: `custom`, `typeName`,
    /// `variability`, `default`, `timeSamples`, `connectionPaths`,
    /// `targetPaths`), §7.6.7 (variant specs), §12.4 (list ops). OpenUSD:
    /// `SdfLayer::GetPseudoRoot`, `GetPrimAtPath`, `GetPropertyAtPath`,
    /// `SdfSpec::ListFields`, `SdfLayer::ListTimeSamplesForPath`.
    ///
    /// # Errors
    ///
    /// `layer.unknown`; `path.invalid` when `path` is not a spec path.
    pub(crate) fn layer_spec(&self, id: u64, path: &str) -> Result<Wire, Error> {
        let store = self.document().store();
        let layer = store
            .layer(LayerId(id))
            .ok_or_else(|| Error::new("layer.unknown", "the stage has no layer with this id"))?;
        let spec = store.try_spec_path(path).map_err(|_| {
            Error::new(
                "path.invalid",
                "not a spec path, such as /World/Rock, /World/Rock{shape=round}Pebble or /World/Rock.size",
            )
            .at_path(path)
        })?;
        let shown = Wire::Text(spec.display(store.paths(), store.tokens()));
        if spec.property().is_some() {
            return Ok(layer.property_at(&spec).map_or(Wire::Null, |property| {
                property_record(store, shown, property)
            }));
        }
        if !spec.variant_chain().is_empty() {
            return Ok(layer
                .variant_spec_at(&spec, store.paths())
                .map_or(Wire::Null, |variant| {
                    self.prim_record("variant", shown, layer, PrimLike::Variant(variant))
                }));
        }
        let root = store.paths().intern(Path::root());
        if spec.prim_path() == root {
            return Ok(self.pseudo_root_record(shown, layer, root));
        }
        Ok(layer.prim_at(&spec).map_or(Wire::Null, |prim| {
            self.prim_record("prim", shown, layer, PrimLike::Prim(prim))
        }))
    }

    /// The record of `layer`'s pseudo-root: its layer metadata, sublayers
    /// and root prims.
    fn pseudo_root_record(&self, shown: Wire, layer: &Layer, root: PathId) -> Wire {
        let tokens = self.document().store().tokens();
        let spec = layer.prim(root);
        let mut fields: Vec<String> = layer
            .metadata_fields()
            .iter()
            .map(|entry| String::from(tokens.resolve(entry.name)))
            .collect();
        let mut add = |authored: bool, name: &str| {
            if authored {
                fields.push(String::from(name));
            }
        };
        add(layer.default_prim().is_some(), "defaultPrim");
        add(!layer.sublayers().is_empty(), "subLayers");
        add(
            layer
                .sublayers()
                .iter()
                .any(|entry| !entry.offset.is_identity()),
            "subLayerOffsets",
        );
        add(layer.has_relocates(), "layerRelocates");
        add(
            spec.is_some_and(|spec| spec.prim_order().is_some()),
            "primOrder",
        );
        Wire::List(vec![
            Wire::text("pseudoRoot"),
            shown,
            sorted_names(fields),
            layer
                .default_prim()
                .map_or(Wire::Null, |token| Wire::text(tokens.resolve(token))),
            text_field(layer.metadata_fields(), "documentation", tokens),
            self.sublayer_rows(layer),
            names(
                spec.map(|spec| spec.authored_children.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .copied(),
                tokens,
            ),
        ])
    }

    /// The record of a prim or variant spec of `layer`.
    fn prim_record(&self, kind: &str, shown: Wire, layer: &Layer, spec: PrimLike<'_>) -> Wire {
        let store = self.document().store();
        let tokens = store.tokens();
        let paths = store.paths();
        let (specifier, type_name) = match spec {
            PrimLike::Prim(prim) => (prim.specifier, prim.type_name),
            PrimLike::Variant(_) => (None, None),
        };
        let active = spec.flag("active", tokens);
        let instanceable = spec.flag("instanceable", tokens);
        let api_schemas = tokens
            .lookup("apiSchemas")
            .and_then(|key| get_field(spec.fields(), &key))
            .and_then(|value| match value {
                FieldValue::TokenListOp(op) => Some(&**op),
                _ => None,
            });
        let mut fields: Vec<String> = spec
            .fields()
            .iter()
            .map(|entry| String::from(tokens.resolve(entry.name)))
            .collect();
        for (authored, name) in [
            (specifier.is_some(), "specifier"),
            (type_name.is_some(), "typeName"),
            (
                matches!(spec, PrimLike::Prim(prim) if prim.active.is_some()),
                "active",
            ),
            (
                matches!(spec, PrimLike::Prim(prim) if prim.instanceable.is_some()),
                "instanceable",
            ),
            (spec.references().is_some(), "references"),
            (spec.payloads().is_some(), "payload"),
            (spec.inherits().is_some(), "inheritPaths"),
            (spec.specializes().is_some(), "specializes"),
            (spec.variant_set_names().is_some(), "variantSetNames"),
            (spec.variant_selections().is_some(), "variantSelection"),
            (spec.has_prim_order(), "primOrder"),
            (spec.has_property_order(), "propertyOrder"),
        ] {
            if authored {
                fields.push(String::from(name));
            }
        }
        let reference = |item: &Reference| self.reference_item(layer.id(), item);
        let path = |item: &PathId| Wire::Text(paths.display(*item, tokens));
        let token = |item: &TokenId| Wire::text(tokens.resolve(*item));
        let selections = spec.variant_selections().map_or(Wire::Null, |selections| {
            let mut pairs: Vec<(&str, &str)> = selections
                .iter()
                .map(|(set, variant)| (tokens.resolve(set), tokens.resolve(variant)))
                .collect();
            pairs.sort_unstable();
            Wire::List(
                pairs
                    .into_iter()
                    .map(|(set, variant)| Wire::List(vec![Wire::text(set), Wire::text(variant)]))
                    .collect(),
            )
        });
        let variant_sets = Wire::List(
            spec.variant_sets()
                .iter()
                .map(|(name, set)| {
                    Wire::List(vec![
                        Wire::text(tokens.resolve(*name)),
                        names(set.variants.keys().copied(), tokens),
                    ])
                })
                .collect(),
        );
        Wire::List(vec![
            Wire::text(kind),
            shown,
            sorted_names(fields),
            specifier.map_or(Wire::Null, |specifier| {
                Wire::text(match specifier {
                    Specifier::Def => "def",
                    Specifier::Over => "over",
                    Specifier::Class => "class",
                })
            }),
            type_name.map_or(Wire::Null, |name| Wire::text(tokens.resolve(name))),
            text_field(spec.fields(), "kind", tokens),
            active.map_or(Wire::Null, Wire::Bool),
            instanceable.map_or(Wire::Null, Wire::Bool),
            text_field(spec.fields(), "documentation", tokens),
            list_op_or_null(spec.references(), reference),
            list_op_or_null(spec.payloads(), reference),
            list_op_or_null(spec.inherits(), path),
            list_op_or_null(spec.specializes(), path),
            list_op_or_null(api_schemas, token),
            list_op_or_null(spec.variant_set_names(), token),
            selections,
            variant_sets,
            names(spec.children().iter().copied(), tokens),
            names(spec.property_names(), tokens),
        ])
    }

    /// A reference or payload item authored in the layer `authoring`:
    /// `[asset, primPath, offset, scale]`, `asset` as authored (`null` for
    /// an internal arc; the identifier of the layer for an arc built
    /// natively by layer alone) and `primPath` `null` for the default prim.
    fn reference_item(&self, authoring: LayerId, item: &Reference) -> Wire {
        let store = self.document().store();
        let target_layer = match item.source() {
            ReferenceSource::Layer { layer, .. } => Some(layer),
            _ => None,
        };
        let asset = item.asset_path().or_else(|| {
            target_layer
                .filter(|layer| *layer != authoring)
                .and_then(|layer| self.document().identifier(layer))
        });
        let prim_path = match item.target {
            ReferenceTarget::Prim(path) => Wire::Text(store.paths().display(path, store.tokens())),
            ReferenceTarget::DefaultPrim => Wire::Null,
        };
        Wire::List(vec![
            Wire::text_or_null(asset),
            prim_path,
            Wire::Number(item.layer_offset.offset),
            Wire::Number(item.layer_offset.scale),
        ])
    }
}

/// The record of an attribute or relationship spec.
fn property_record(store: &InMemoryStore, shown: Wire, spec: &PropertySpec) -> Wire {
    let tokens = store.tokens();
    let mut fields: Vec<String> = spec
        .metadata
        .iter()
        .map(|entry| String::from(tokens.resolve(entry.name)))
        .collect();
    let attribute = spec.kind == PropertyKind::Attribute;
    for (authored, name) in [
        (spec.type_name.is_some(), "typeName"),
        (spec.custom, "custom"),
        // A relationship is uniform without saying so.
        (
            attribute && spec.variability == Variability::Uniform,
            "variability",
        ),
        (spec.default.is_some(), "default"),
        (spec.time_samples.is_authored(), "timeSamples"),
        (spec.spline.is_some(), "spline"),
        (
            spec.targets.is_some(),
            if attribute {
                "connectionPaths"
            } else {
                "targetPaths"
            },
        ),
    ] {
        if authored {
            fields.push(String::from(name));
        }
    }
    let targets = list_op_or_null(spec.targets.as_deref(), |target| {
        Wire::Text(target.display(store.paths(), tokens))
    });
    if !attribute {
        return Wire::List(vec![
            Wire::text("relationship"),
            shown,
            sorted_names(fields),
            Wire::Bool(spec.custom),
            targets,
        ]);
    }
    let default = spec.default.as_ref().map_or(Wire::Null, |value| {
        Wire::text(match value {
            Value::Blocked => "block",
            Value::AnimationBlock => "animationBlock",
            _ => "value",
        })
    });
    let times = spec.time_samples.get().map_or(Wire::Null, |samples| {
        Wire::Floats(samples.iter().map(|sample| sample.0).collect())
    });
    Wire::List(vec![
        Wire::text("attribute"),
        shown,
        sorted_names(fields),
        spec.type_name
            .as_ref()
            .map_or(Wire::Null, |name| Wire::text(name.type_name())),
        Wire::Bool(spec.custom),
        Wire::text(match spec.variability {
            Variability::Varying => "varying",
            Variability::Uniform => "uniform",
        }),
        default,
        times,
        targets,
    ])
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, vec, vec::Vec};

    use crate::{
        edit::EditBatch,
        stage::{BROWSER_LIMITS, Stage},
        wire::Wire,
    };

    const SCENE: &str = r#"#usda 1.0
(
    defaultPrim = "World"
    doc = "A scene"
    upAxis = "Z"
    subLayers = [
        @./shot.usda@ (offset = 10),
        @./base.usda@
    ]
)
def Xform "World" (
    kind = "assembly"
) {
    def Xform "Robot" (
        active = true
        instanceable = false
        doc = "A robot"
        prepend apiSchemas = ["CollectionAPI:parts"]
        delete references = @./old.usda@
        prepend references = [@./robot.usda@</Robot> (offset = 5; scale = 2), </World/Spare>]
        append references = @./trim.usda@
        add inherits = </Base>
        reorder inherits = [</Base>, </Other>]
        specializes = None
        variants = {
            string finish = "painted"
            string size = "big"
        }
        prepend variantSets = ["finish", "size"]
    ) {
        double height = 2
        double speed.timeSamples = {
            4: 1,
            1: 2,
        }
        uniform token mode = None
        custom rel parts = [</World/Spare>]
        color3f out.connect = </World/Spare.color>
        variantSet "finish" = {
            "chrome" {
            }
            "painted" (
                prepend references = @./paint.usda@
            ) {
                color3f color = (0.8, 0.2, 0.1)
                def "Decal" {
                    double size = 1
                }
            }
        }
        variantSet "size" = {
            "big" {
            }
        }
        def "Arm" {
        }
    }
    def Xform "Spare" {
    }
}
"#;

    fn open() -> Stage {
        Stage::open(
            "scene.usda",
            SCENE.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap()
    }

    fn root(stage: &Stage) -> u64 {
        stage.document().stage().stage().root_layer().unwrap().0
    }

    fn list(wire: &Wire) -> &[Wire] {
        wire.as_list()
            .unwrap_or_else(|| panic!("not a list: {wire:?}"))
    }

    fn texts(wire: &Wire) -> Vec<&str> {
        list(wire)
            .iter()
            .map(|item| item.as_text().unwrap())
            .collect()
    }

    fn text(value: &str) -> Wire {
        Wire::text(value)
    }

    fn strings(items: &[&str]) -> Wire {
        Wire::List(items.iter().map(|item| Wire::text(*item)).collect())
    }

    fn reference(asset: Option<&str>, prim: Option<&str>, offset: f64, scale: f64) -> Wire {
        Wire::List(vec![
            Wire::text_or_null(asset),
            Wire::text_or_null(prim),
            Wire::Number(offset),
            Wire::Number(scale),
        ])
    }

    /// The lists of a list op record after `explicit`.
    struct Lists {
        prepended: Vec<Wire>,
        appended: Vec<Wire>,
        deleted: Vec<Wire>,
        added: Vec<Wire>,
        ordered: Vec<Wire>,
    }

    impl Lists {
        fn none() -> Self {
            Self {
                prepended: vec![],
                appended: vec![],
                deleted: vec![],
                added: vec![],
                ordered: vec![],
            }
        }
    }

    /// A list op record: explicit, prepended, appended, deleted, added, ordered.
    fn op(explicit: Option<Vec<Wire>>, lists: Lists) -> Wire {
        Wire::List(vec![
            explicit.map_or(Wire::Null, Wire::List),
            Wire::List(lists.prepended),
            Wire::List(lists.appended),
            Wire::List(lists.deleted),
            Wire::List(lists.added),
            Wire::List(lists.ordered),
        ])
    }

    #[test]
    fn the_pseudo_root_holds_layer_metadata_sublayers_and_root_prims() {
        let stage = open();
        let spec = stage.layer_spec(root(&stage), "/").unwrap();
        let fields = list(&spec);
        assert_eq!(fields[0], text("pseudoRoot"));
        assert_eq!(fields[1], text("/"));
        assert_eq!(
            texts(&fields[2]),
            [
                "defaultPrim",
                "documentation",
                "subLayerOffsets",
                "subLayers",
                "upAxis"
            ]
        );
        assert_eq!(fields[3], text("World"));
        assert_eq!(fields[4], text("A scene"));
        assert_eq!(
            fields[5],
            Wire::List(vec![
                Wire::List(vec![
                    text("./shot.usda"),
                    Wire::Number(10.0),
                    Wire::Number(1.0)
                ]),
                Wire::List(vec![
                    text("./base.usda"),
                    Wire::Number(0.0),
                    Wire::Number(1.0)
                ]),
            ])
        );
        assert_eq!(fields[6], strings(&["World"]));
    }

    #[test]
    fn a_prim_spec_gives_its_fields_and_each_list_of_its_list_ops() {
        let stage = open();
        let spec = stage.layer_spec(root(&stage), "/World/Robot").unwrap();
        let fields = list(&spec);
        assert_eq!(fields[0], text("prim"));
        assert_eq!(fields[1], text("/World/Robot"));
        assert_eq!(
            texts(&fields[2]),
            [
                "active",
                "apiSchemas",
                "documentation",
                "inheritPaths",
                "instanceable",
                "references",
                "specializes",
                "specifier",
                "typeName",
                "variantSelection",
                "variantSetNames",
            ]
        );
        assert_eq!(fields[3], text("def"));
        assert_eq!(fields[4], text("Xform"));
        assert_eq!(fields[5], Wire::Null, "no kind of its own");
        assert_eq!(fields[6], Wire::Bool(true));
        assert_eq!(fields[7], Wire::Bool(false));
        assert_eq!(fields[8], text("A robot"));
        assert_eq!(
            fields[9],
            op(
                None,
                Lists {
                    prepended: vec![
                        reference(Some("./robot.usda"), Some("/Robot"), 5.0, 2.0),
                        reference(None, Some("/World/Spare"), 0.0, 1.0),
                    ],
                    appended: vec![reference(Some("./trim.usda"), None, 0.0, 1.0)],
                    deleted: vec![reference(Some("./old.usda"), None, 0.0, 1.0)],
                    ..Lists::none()
                }
            ),
            "references, an internal one included"
        );
        assert_eq!(fields[10], Wire::Null, "no payload");
        assert_eq!(
            fields[11],
            op(
                None,
                Lists {
                    added: vec![text("/Base")],
                    ordered: vec![text("/Base"), text("/Other")],
                    ..Lists::none()
                }
            ),
            "inherits: added and reordered"
        );
        assert_eq!(
            fields[12],
            op(Some(vec![]), Lists::none()),
            "`specializes = None` is an explicit empty list"
        );
        assert_eq!(
            fields[13],
            op(
                None,
                Lists {
                    prepended: vec![text("CollectionAPI:parts")],
                    ..Lists::none()
                }
            )
        );
        assert_eq!(
            fields[14],
            op(
                None,
                Lists {
                    prepended: vec![text("finish"), text("size")],
                    ..Lists::none()
                }
            )
        );
        assert_eq!(
            fields[15],
            Wire::List(vec![
                strings(&["finish", "painted"]),
                strings(&["size", "big"])
            ])
        );
        assert_eq!(
            fields[16],
            Wire::List(vec![
                Wire::List(vec![text("finish"), strings(&["chrome", "painted"])]),
                Wire::List(vec![text("size"), strings(&["big"])]),
            ])
        );
        assert_eq!(fields[17], strings(&["Arm"]));
        assert_eq!(
            fields[18],
            strings(&["height", "speed", "mode", "parts", "out"])
        );

        let world = stage.layer_spec(root(&stage), "/World").unwrap();
        assert_eq!(list(&world)[5], text("assembly"));
        assert_eq!(list(&world)[17], strings(&["Robot", "Spare"]));
    }

    #[test]
    fn specs_inside_a_variant_are_reached_by_their_variant_paths() {
        let stage = open();
        let layer = root(&stage);
        let variant = stage
            .layer_spec(layer, "/World/Robot{finish=painted}")
            .unwrap();
        let fields = list(&variant);
        assert_eq!(fields[0], text("variant"));
        assert_eq!(fields[1], text("/World/Robot{finish=painted}"));
        assert_eq!(texts(&fields[2]), ["references"]);
        assert_eq!(fields[3], Wire::Null, "a variant has no specifier");
        assert_eq!(
            fields[9],
            op(
                None,
                Lists {
                    prepended: vec![reference(Some("./paint.usda"), None, 0.0, 1.0)],
                    ..Lists::none()
                }
            )
        );
        assert_eq!(fields[17], strings(&["Decal"]));
        assert_eq!(fields[18], strings(&["color"]));

        let decal = stage
            .layer_spec(layer, "/World/Robot{finish=painted}Decal")
            .unwrap();
        assert_eq!(list(&decal)[0], text("prim"));
        assert_eq!(list(&decal)[3], text("def"));
        let size = stage
            .layer_spec(layer, "/World/Robot{finish=painted}Decal.size")
            .unwrap();
        assert_eq!(list(&size)[0], text("attribute"));
        let color = stage
            .layer_spec(layer, "/World/Robot{finish=painted}.color")
            .unwrap();
        assert_eq!(list(&color)[3], text("color3f"));

        for missing in [
            "/World/Robot{finish=chrome}Decal",
            "/World/Robot{finish=matte}",
            "/World/Robot/Decal",
            "/World/Robot.color",
        ] {
            assert_eq!(
                stage.layer_spec(layer, missing).unwrap(),
                Wire::Null,
                "{missing}"
            );
        }
    }

    #[test]
    fn property_specs_say_what_they_author_without_reading_values() {
        let stage = open();
        let layer = root(&stage);
        let height = stage.layer_spec(layer, "/World/Robot.height").unwrap();
        assert_eq!(
            height,
            Wire::List(vec![
                text("attribute"),
                text("/World/Robot.height"),
                strings(&["default", "typeName"]),
                text("double"),
                Wire::Bool(false),
                text("varying"),
                text("value"),
                Wire::Null,
                Wire::Null,
            ])
        );
        let speed = stage.layer_spec(layer, "/World/Robot.speed").unwrap();
        assert_eq!(texts(&list(&speed)[2]), ["timeSamples", "typeName"]);
        assert_eq!(list(&speed)[6], Wire::Null, "no default");
        assert_eq!(
            list(&speed)[7],
            Wire::Floats(vec![1.0, 4.0]),
            "sorted layer times"
        );
        let mode = stage.layer_spec(layer, "/World/Robot.mode").unwrap();
        assert_eq!(
            texts(&list(&mode)[2]),
            ["default", "typeName", "variability"]
        );
        assert_eq!(list(&mode)[5], text("uniform"));
        assert_eq!(list(&mode)[6], text("block"));
        let out = stage.layer_spec(layer, "/World/Robot.out").unwrap();
        assert_eq!(texts(&list(&out)[2]), ["connectionPaths", "typeName"]);
        assert_eq!(
            list(&out)[8],
            op(Some(vec![text("/World/Spare.color")]), Lists::none())
        );
        let parts = stage.layer_spec(layer, "/World/Robot.parts").unwrap();
        assert_eq!(
            parts,
            Wire::List(vec![
                text("relationship"),
                text("/World/Robot.parts"),
                strings(&["custom", "targetPaths"]),
                Wire::Bool(true),
                op(Some(vec![text("/World/Spare")]), Lists::none()),
            ])
        );
    }

    #[test]
    fn a_layer_without_a_spec_there_reads_null_and_bad_input_is_refused() {
        let stage = open();
        let layer = root(&stage);
        assert_eq!(stage.layer_spec(layer, "/Nowhere").unwrap(), Wire::Null);
        assert_eq!(
            stage.layer_spec(layer, "/World.nothing").unwrap(),
            Wire::Null
        );
        for invalid in ["World", "/World/", "/World{finish}", "/.size", ""] {
            let error = stage.layer_spec(layer, invalid).unwrap_err();
            assert_eq!(error.code, "path.invalid", "{invalid}");
        }
        let error = stage.layer_spec(u64::MAX, "/").unwrap_err();
        assert_eq!(error.code, "layer.unknown");
    }

    #[test]
    fn an_edit_shows_in_the_spec_and_its_undo_takes_it_away() {
        let mut stage = open();
        let layer = root(&stage);
        let mut batch = EditBatch::new();
        batch.set_kind(String::from("/World/Spare"), String::from("component"));
        let mut receipt = stage.apply(layer, &mut batch).unwrap();
        let spare = stage.layer_spec(layer, "/World/Spare").unwrap();
        assert_eq!(texts(&list(&spare)[2]), ["kind", "specifier", "typeName"]);
        assert_eq!(list(&spare)[5], text("component"));
        stage.undo(&mut receipt).unwrap();
        let spare = stage.layer_spec(layer, "/World/Spare").unwrap();
        assert_eq!(texts(&list(&spare)[2]), ["specifier", "typeName"]);
        assert_eq!(list(&spare)[5], Wire::Null);
    }
}
