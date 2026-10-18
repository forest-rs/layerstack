// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A composed stage bound to the store it was composed from: the canonical
//! way to read a stage.

use alloc::{string::String, sync::Arc, vec::Vec};

use super::{Attribute, Object, Prim, PrimPredicate, Relationship, Stage};
use crate::{
    Value,
    doc::LayerStore,
    interner::{TokenId, TokenInterner},
    path::{PathId, PathInterner, PropertyPath, TargetPath},
};

/// A composed [`Stage`] bound to the [`LayerStore`] it was composed from.
///
/// Every query of a view takes paths as text (`"/World/Arm"`) or as ids,
/// and hands out handles ([`Prim`], [`Attribute`], [`Relationship`]) bound
/// to the same stage and store, so no call needs the store passed again. A
/// view borrows both and is `Copy`; make one wherever a stage and its store
/// are at hand ([`crate::LiveStage::view`], or `StageDocument::view` in
/// `layerstack_io`).
///
/// A path names nothing when the stage does not compose it, including when
/// its text is not a well-formed absolute path; queries then return
/// `None`. Looking a path up never interns it.
///
/// OpenUSD: a `UsdStage`, whose `GetPrimAtPath`, `GetAttributeAtPath`,
/// `GetRelationshipAtPath` and `Traverse` these mirror.
///
/// ```
/// use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, Stage, StageOptions};
///
/// let mut store = InMemoryStore::default();
/// let world = store.path("/World");
/// let mut layer = Layer::new(LayerId(1));
/// layer.insert_prim(world, PrimSpec::def());
/// store.insert_layer(layer);
/// let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
///
/// let view = stage.view(&store);
/// assert_eq!(view.prim("/World").map(|prim| prim.path()), Some(world));
/// assert!(view.prim("/Missing").is_none());
/// assert_eq!(view.traverse().count(), 1);
/// ```
#[derive(Clone, Copy)]
#[doc(alias = "UsdStage")]
pub struct StageView<'a> {
    stage: &'a Stage,
    store: &'a dyn LayerStore,
}

impl core::fmt::Debug for StageView<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StageView").finish_non_exhaustive()
    }
}

impl Stage {
    /// This stage bound to `store`, which holds the layers it was composed
    /// from (see [`StageView`]).
    #[must_use]
    pub fn view<'a>(&'a self, store: &'a dyn LayerStore) -> StageView<'a> {
        StageView { stage: self, store }
    }
}

/// A prim path given to a [`StageView`] query: text (`"/World/Arm"`) or an
/// interned id.
pub trait PrimPathArg {
    /// The interned path this names, if the store has interned it.
    fn prim_path(&self, paths: &PathInterner, tokens: &TokenInterner) -> Option<PathId>;
}

impl PrimPathArg for PathId {
    fn prim_path(&self, _: &PathInterner, _: &TokenInterner) -> Option<PathId> {
        Some(*self)
    }
}

impl PrimPathArg for &str {
    fn prim_path(&self, paths: &PathInterner, tokens: &TokenInterner) -> Option<PathId> {
        lookup_prim(self, paths, tokens)
    }
}

impl PrimPathArg for &String {
    fn prim_path(&self, paths: &PathInterner, tokens: &TokenInterner) -> Option<PathId> {
        lookup_prim(self, paths, tokens)
    }
}

/// A property path given to a [`StageView`] query: text
/// (`"/World/Arm.points"`) or an interned property path.
pub trait PropertyPathArg {
    /// The property path this names, if the store has interned its parts.
    fn property_path(&self, paths: &PathInterner, tokens: &TokenInterner) -> Option<PropertyPath>;
}

impl PropertyPathArg for PropertyPath {
    fn property_path(&self, _: &PathInterner, _: &TokenInterner) -> Option<PropertyPath> {
        Some(*self)
    }
}

impl PropertyPathArg for &str {
    fn property_path(&self, paths: &PathInterner, tokens: &TokenInterner) -> Option<PropertyPath> {
        lookup_property(self, paths, tokens)
    }
}

impl PropertyPathArg for &String {
    fn property_path(&self, paths: &PathInterner, tokens: &TokenInterner) -> Option<PropertyPath> {
        lookup_property(self, paths, tokens)
    }
}

/// The interned absolute prim path `text`, without interning anything.
fn lookup_prim(text: &str, paths: &PathInterner, tokens: &TokenInterner) -> Option<PathId> {
    let rest = text.strip_prefix('/')?;
    let mut names: Vec<TokenId> = Vec::new();
    if !rest.is_empty() {
        for name in rest.split('/') {
            if name.is_empty() {
                return None;
            }
            names.push(tokens.lookup(name)?);
        }
    }
    paths.lookup_segments(&names)
}

/// The interned property path `text` (`/Prim.name`, the name possibly
/// namespaced with `:`), without interning anything.
fn lookup_property(
    text: &str,
    paths: &PathInterner,
    tokens: &TokenInterner,
) -> Option<PropertyPath> {
    let last_slash = text.rfind('/')?;
    let dot = last_slash + text[last_slash..].find('.')?;
    let prim = lookup_prim(&text[..dot.max(1)], paths, tokens)?;
    let name = &text[dot + 1..];
    if name.is_empty() {
        return None;
    }
    Some(PropertyPath::new(prim, tokens.lookup(name)?))
}

impl<'a> StageView<'a> {
    /// The composed stage.
    #[must_use]
    pub fn stage(self) -> &'a Stage {
        self.stage
    }

    /// The store the stage was composed from.
    #[must_use]
    pub fn store(self) -> &'a dyn LayerStore {
        self.store
    }

    /// The pseudo-root, the parent of the stage's root prims.
    #[doc(alias = "UsdStage::GetPseudoRoot")]
    pub fn pseudo_root(self) -> Option<Prim<'a>> {
        self.prim("/")
    }

    /// The composed prim at `path`, active or not.
    #[doc(alias = "UsdStage::GetPrimAtPath")]
    pub fn prim(self, path: impl PrimPathArg) -> Option<Prim<'a>> {
        let id = path.prim_path(self.store.paths(), self.store.tokens())?;
        self.stage.prim(id, self.store)
    }

    /// The composed attribute at `path` (`"/World/Arm.points"`).
    #[doc(alias = "UsdStage::GetAttributeAtPath")]
    pub fn attribute(self, path: impl PropertyPathArg) -> Option<Attribute<'a>> {
        match self.property(path)? {
            Object::Attribute(attribute) => Some(attribute),
            _ => None,
        }
    }

    /// The composed relationship at `path`.
    #[doc(alias = "UsdStage::GetRelationshipAtPath")]
    pub fn relationship(self, path: impl PropertyPathArg) -> Option<Relationship<'a>> {
        match self.property(path)? {
            Object::Relationship(relationship) => Some(relationship),
            _ => None,
        }
    }

    /// The composed attribute or relationship at `path`.
    #[doc(alias = "UsdStage::GetPropertyAtPath")]
    pub fn property(self, path: impl PropertyPathArg) -> Option<Object<'a>> {
        let path = path.property_path(self.store.paths(), self.store.tokens())?;
        self.stage.object(TargetPath::Property(path), self.store)
    }

    /// The composed prim the root layer's `defaultPrim` names (a root
    /// prim's name, or an absolute path).
    ///
    /// Spec: AOUSD Core §7.6.1 (`defaultPrim`).
    #[doc(alias = "UsdStage::GetDefaultPrim")]
    pub fn default_prim(self) -> Option<Prim<'a>> {
        let tokens = self.store.tokens();
        let token = self.store.layer(self.stage.root_layer()?)?.default_prim()?;
        let text = tokens.resolve(token);
        let id = if text.starts_with('/') {
            lookup_prim(text, self.store.paths(), tokens)
        } else {
            lookup_prim(&alloc::format!("/{text}"), self.store.paths(), tokens)
        }?;
        self.stage.prim(id, self.store)
    }

    /// The stage's prims in depth-first namespace order, beneath the
    /// pseudo-root: active, loaded, defined and non-abstract prims, as
    /// OpenUSD's default traversal visits them.
    #[doc(alias = "UsdStage::Traverse")]
    pub fn traverse(self) -> impl Iterator<Item = Prim<'a>> + 'a {
        self.traverse_with(PrimPredicate::default())
    }

    /// The prims `predicate` admits, in depth-first namespace order beneath
    /// the pseudo-root; a prim it rejects is skipped with its descendants.
    #[doc(alias = "UsdStage::Traverse")]
    pub fn traverse_with(self, predicate: PrimPredicate) -> impl Iterator<Item = Prim<'a>> + 'a {
        let root = self.store.paths().lookup(&crate::Path::root());
        let (stage, store) = (self.stage, self.store);
        root.into_iter().flat_map(move |root| {
            stage
                .prim_range(root, store, predicate)
                .filter(move |path| *path != root)
                .filter_map(move |path| stage.prim(path, store))
        })
    }
}

/// A Rust value an attribute value converts to: the target of
/// [`Attribute::value`].
///
/// Conversion is strict, as OpenUSD's typed `UsdAttribute::Get<T>` is: a
/// value of another type does not convert (an `f32` attribute does not read
/// as `f64`). Which opinion is converted depends on the time, as in OpenUSD
/// 26.08: at [`Time::Default`](crate::Time::Default) a default that does
/// not convert is skipped for a weaker one (or the schema fallback) that
/// does, while at a time code the strongest source is selected first and a
/// value that does not convert reads as nothing. The
/// `layerstack_conformance` `typed_reads` oracle records both from a
/// program built against OpenUSD. To see the opinion a read skipped, use
/// [`Stage::explain_property_value`](crate::Stage::explain_property_value),
/// which marks it incompatible.
///
/// Spec: AOUSD Core §6 (value types), §12.3 (value resolution).
pub trait FromValue: Sized {
    /// Converts `value`, or returns `None` when it is of another type.
    /// `tokens` resolves token values.
    fn from_value(value: &Value, tokens: &TokenInterner) -> Option<Self>;
}

macro_rules! scalar_from_value {
    ($($ty:ty => $variant:ident),* $(,)?) => {$(
        impl FromValue for $ty {
            fn from_value(value: &Value, _: &TokenInterner) -> Option<Self> {
                match value {
                    Value::$variant(v) => Some(*v),
                    _ => None,
                }
            }
        }
    )*};
}

scalar_from_value! {
    bool => Bool,
    u8 => UChar,
    i32 => Int,
    u32 => UInt,
    i64 => Int64,
    u64 => UInt64,
    f32 => Float,
    f64 => Double,
    [f32; 2] => Vec2f,
    [f32; 3] => Vec3f,
    [f32; 4] => Vec4f,
    [f64; 2] => Vec2d,
    [f64; 3] => Vec3d,
    [f64; 4] => Vec4d,
    [i32; 2] => Vec2i,
    [i32; 3] => Vec3i,
    [i32; 4] => Vec4i,
}

impl FromValue for [f64; 16] {
    fn from_value(value: &Value, _: &TokenInterner) -> Option<Self> {
        match value {
            Value::Matrix4d(m) => Some(**m),
            _ => None,
        }
    }
}

/// A `string` value. Tokens and asset paths are other types: read them as
/// [`TokenId`] or with [`Attribute::get`].
impl FromValue for String {
    fn from_value(value: &Value, _: &TokenInterner) -> Option<Self> {
        match value {
            Value::String(text) => Some(Self::from(&**text)),
            _ => None,
        }
    }
}

/// A `token` value, as an id; resolve it with the store's tokens.
impl FromValue for TokenId {
    fn from_value(value: &Value, _: &TokenInterner) -> Option<Self> {
        match value {
            Value::Token(token) => Some(*token),
            _ => None,
        }
    }
}

/// An array of any element type that converts: a typed array or a
/// generic one, every element converting.
impl<T: FromValue> FromValue for Vec<T> {
    fn from_value(value: &Value, tokens: &TokenInterner) -> Option<Self> {
        match value {
            Value::Array(items) => items.iter().map(|v| T::from_value(v, tokens)).collect(),
            Value::TypedArray(array) => array.values().map(|v| T::from_value(&v, tokens)).collect(),
            _ => None,
        }
    }
}

/// Shared text, as `string` values are held.
impl FromValue for Arc<str> {
    fn from_value(value: &Value, _: &TokenInterner) -> Option<Self> {
        match value {
            Value::String(text) => Some(text.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, vec, vec::Vec};

    use crate::{
        InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, PropertyType, Stage, StageOptions,
        Time, TokenId, Value, spec_path::VariantSelectionSite,
    };

    fn double(value: f64) -> PropertySpec {
        PropertySpec::typed_attribute(PropertyType::parse("double").unwrap())
            .with_default(Value::Double(value))
    }

    /// `/World` (the default prim) with `/World/Arm` and `/World/Leg`,
    /// `/World.radius = 2.5`, `/World.name = "w"` and `/World.kind:tag =
    /// token "x"`.
    fn scene() -> (InMemoryStore, Stage) {
        let mut store = InMemoryStore::default();
        let (world, arm, leg) = (
            store.path("/World"),
            store.path("/World/Arm"),
            store.path("/World/Leg"),
        );
        let names = vec![store.tokens.intern("Arm"), store.tokens.intern("Leg")];
        let (radius, name, tag, x) = (
            store.tokens.intern("radius"),
            store.tokens.intern("name"),
            store.tokens.intern("kind:tag"),
            store.tokens.intern("x"),
        );
        let string = PropertyType::parse("string").unwrap();
        let token = PropertyType::parse("token").unwrap();
        let mut layer = Layer::new(LayerId(1));
        layer.set_default_prim(Some(store.tokens.intern("World")));
        layer.insert_prim(
            world,
            PrimSpec::def()
                .with_children(names)
                .with_property(radius, double(2.5))
                .with_property(
                    name,
                    PropertySpec::typed_attribute(string).with_default(Value::string("w")),
                )
                .with_property(
                    tag,
                    PropertySpec::typed_attribute(token).with_default(Value::Token(x)),
                ),
        );
        layer.insert_prim(arm, PrimSpec::def());
        layer.insert_prim(leg, PrimSpec::def());
        store.insert_layer(layer);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        (store, stage)
    }

    #[test]
    fn paths_are_looked_up_without_interning() {
        let (store, stage) = scene();
        let view = stage.view(&store);
        let before = (store.paths.len(), store.tokens.len());
        assert!(view.prim("/World/Arm").is_some());
        assert!(view.pseudo_root().is_some());
        for missing in [
            "/World/Hand",
            "World",
            "",
            "//World",
            "/World/",
            "/Nowhere/Deep",
        ] {
            assert!(view.prim(missing).is_none(), "{missing:?}");
        }
        assert!(view.attribute("/World.radius").is_some());
        assert!(view.attribute("/World.kind:tag").is_some());
        for missing in [
            "/World.",
            "/World.missing",
            "/World/Arm.radius",
            ".radius",
            "/World",
        ] {
            assert!(view.attribute(missing).is_none(), "{missing:?}");
        }
        assert_eq!(
            (store.paths.len(), store.tokens.len()),
            before,
            "lookups intern nothing"
        );
    }

    #[test]
    fn traversal_and_default_prim() {
        let (store, stage) = scene();
        let view = stage.view(&store);
        let names: Vec<String> = view
            .traverse()
            .map(|prim| store.paths.display(prim.path(), &store.tokens))
            .collect();
        assert_eq!(names, ["/World", "/World/Arm", "/World/Leg"]);
        assert_eq!(
            view.default_prim().map(|prim| prim.path()),
            store
                .paths
                .lookup(&crate::Path::parse_absolute("/World", &store.tokens).unwrap())
        );
    }

    #[test]
    fn values_convert_strictly() {
        let (store, stage) = scene();
        let view = stage.view(&store);
        let radius = view.attribute("/World.radius").unwrap();
        assert_eq!(radius.value::<f64>(Time::Default), Some(2.5));
        assert_eq!(radius.value::<f32>(Time::Default), None);
        assert_eq!(radius.value::<String>(Time::Default), None);
        let name = view.attribute("/World.name").unwrap();
        assert_eq!(name.value::<String>(Time::Default).as_deref(), Some("w"));
        assert_eq!(name.value::<TokenId>(Time::Default), None);
        let tag = view.attribute("/World.kind:tag").unwrap();
        let token = tag.value::<TokenId>(Time::Default).unwrap();
        assert_eq!(store.tokens.resolve(token), "x");
    }

    #[test]
    fn arrays_convert_element_by_element() {
        let tokens = crate::TokenInterner::default();
        let ints = Value::Array(vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(
            super::FromValue::from_value(&ints, &tokens),
            Some(vec![1_i32, 2])
        );
        assert_eq!(
            <Vec<i64> as super::FromValue>::from_value(&ints, &tokens),
            None,
            "an array of another element type does not convert"
        );
    }

    /// `/Item.price` in a stack of `overlay` (2) over `basis` (3), with a
    /// session-like `top` (1) above both: `top` authors nothing for the
    /// price, `overlay` a default of 12 and a sample at 10 of 14, `basis` a
    /// default of 10 and samples at 0 and 20.
    fn overlays() -> InMemoryStore {
        let mut store = InMemoryStore::default();
        let item = store.path("/Item");
        let (price, note) = (store.tokens.intern("price"), store.tokens.intern("note"));
        let typed = |value: Option<f64>, samples: Vec<(f64, f64)>| {
            let mut spec = PropertySpec::typed_attribute(PropertyType::parse("double").unwrap());
            spec.default = value.map(Value::Double);
            if !samples.is_empty() {
                spec = spec.with_time_samples(
                    samples
                        .into_iter()
                        .map(|(t, v)| (t, Value::Double(v)))
                        .collect(),
                );
            }
            spec
        };
        let mut top = Layer::new(LayerId(1));
        top.sublayers_mut().extend([
            crate::SublayerEntry::new(LayerId(2)),
            crate::SublayerEntry::new(LayerId(3)),
        ]);
        top.insert_prim(item, PrimSpec::over().with_property(note, double(1.0)));
        let mut overlay = Layer::new(LayerId(2));
        overlay.insert_prim(
            item,
            PrimSpec::over().with_property(price, typed(Some(12.0), vec![(10.0, 14.0)])),
        );
        let mut basis = Layer::new(LayerId(3));
        basis.insert_prim(
            item,
            PrimSpec::def().with_property(price, typed(Some(10.0), vec![(0.0, 0.0), (20.0, 20.0)])),
        );
        for layer in [top, overlay, basis] {
            store.insert_layer(layer);
        }
        store
    }

    #[test]
    fn values_beneath_a_layer_are_those_without_it() {
        let mut store = overlays();
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let mut muted = StageOptions::default();
        muted.muted_layers.insert(LayerId(2));
        let without = Stage::compose(&mut store, LayerId(1), muted);
        let view = stage.view(&store);
        let price = view.attribute("/Item.price").unwrap();
        let unmuted = without.view(&store).attribute("/Item.price").unwrap();
        for time in [
            Time::Default,
            Time::at(5.0),
            Time::at(10.0),
            Time::held(15.0),
        ] {
            let below = price.value_below::<f64>(LayerId(2), time).unwrap();
            assert_eq!(Some(below.value), unmuted.value::<f64>(time), "{time:?}");
            assert_eq!(
                below.provenance.map(|p| p.layer),
                Some(LayerId(3)),
                "the basis supplies it"
            );
        }
        assert_eq!(price.value::<f64>(Time::Default), Some(12.0));
        // `top` authors no price: beneath it is everything weaker, so the
        // overlay's own value.
        let below_top = price.value_below::<f64>(LayerId(1), Time::Default).unwrap();
        assert_eq!(below_top.value, 12.0);
        assert_eq!(below_top.provenance.map(|p| p.layer), Some(LayerId(2)));
        // Beneath the basis there is only the fallback, which has none.
        assert!(
            price
                .value_below::<f64>(LayerId(3), Time::Default)
                .is_none()
        );
    }

    #[test]
    fn layer_specs_iterate_in_namespace_order() {
        let mut store = InMemoryStore::default();
        let (b, a, a_child) = (store.path("/B"), store.path("/A"), store.path("/A/Child"));
        let (set, x) = (store.tokens.intern("v"), store.tokens.intern("x"));
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(b, PrimSpec::def());
        layer.insert_prim(a_child, PrimSpec::def());
        let mut branch = PrimSpec::def();
        branch.outer_variant_sites = vec![VariantSelectionSite {
            host_path: a,
            set,
            variant: x,
        }];
        layer.insert_prim(a_child, branch);
        layer.insert_prim(a, PrimSpec::def());
        let order: Vec<(String, usize)> = layer
            .prim_specs_in_order(&store.paths, &store.tokens)
            .into_iter()
            .map(|(path, spec)| {
                (
                    store.paths.display(path, &store.tokens),
                    spec.outer_variant_sites.len(),
                )
            })
            .collect();
        assert_eq!(
            order,
            [
                ("/A".into(), 0),
                ("/A/Child".into(), 0),
                ("/A/Child".into(), 1),
                ("/B".into(), 0),
            ]
        );
    }
}
