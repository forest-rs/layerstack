// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A model's cached per-purpose bounds, `extentsHint`, as OpenUSD's
//! `UsdGeomModelAPI` reads and authors it. `BoundsCache::compute_extents_hint`
//! computes the value to author.

use crate::{
    SchemaEdit, Time,
    bounds::Extent,
    usd_geom::{GeomModelApi, GeomModelApiEdit},
};
use alloc::{string::String, vec::Vec};
use layerstack::{PropertyType, Value};

const EXTENTS_HINT: &str = "extentsHint";

/// Why an `extentsHint` edit was refused. Nothing is authored.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExtentsHintError {
    /// A hint holds one to four extents, one per purpose in default, render,
    /// proxy, guide order (OpenUSD's "invalid extents size").
    Count {
        /// The number of extents given.
        count: usize,
    },
    /// `extentsHint` exists with a type other than `float3[]`, which
    /// OpenUSD cannot set a hint on.
    IncompatibleAttribute {
        /// Its existing USD type name.
        type_name: String,
    },
}
impl core::fmt::Display for ExtentsHintError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Count { count } => {
                write!(f, "an extents hint holds 1 to 4 extents, not {count}")
            }
            Self::IncompatibleAttribute { type_name } => {
                write!(f, "extentsHint has type {type_name}, not float3[]")
            }
        }
    }
}
impl core::error::Error for ExtentsHintError {}

impl GeomModelApi<'_> {
    /// The prim's authored `extentsHint` at `time`: one extent per purpose,
    /// in default, render, proxy, guide order. `None` when it has none, or
    /// one that is not a `float3[]` of an even length. A prim need not have
    /// the schema applied: read any prim through
    /// [`GeomModelApi::from_prim_unchecked`], as `UsdGeomModelAPI(prim)` reads it.
    #[must_use]
    #[doc(alias = "UsdGeomModelAPI::GetExtentsHint")]
    #[doc(alias = "GetExtentsHint")]
    pub fn extents_hint(&self, time: Time) -> Option<Vec<Extent>> {
        let points = self.read_value_at(EXTENTS_HINT, time, crate::value::read_float3_array)?;
        let (pairs, rest) = points.as_chunks::<2>();
        rest.is_empty().then(|| pairs.to_vec())
    }
}

impl GeomModelApiEdit {
    /// Authors `extents` as the prim's default `extentsHint`, creating the
    /// `float3[]` attribute (not custom) when the prim lacks it.
    ///
    /// OpenUSD: `UsdGeomModelAPI::SetExtentsHint` at the default time; the
    /// hint is the model's bounds per purpose, as
    /// `BoundsCache::compute_extents_hint` computes them, and bounds caches
    /// with `use_extents_hint` read it instead of the model's descendants.
    ///
    /// # Errors
    /// [`ExtentsHintError`] for no extents or more than four, or an
    /// `extentsHint` of another type; nothing is authored.
    #[doc(alias = "UsdGeomModelAPI::SetExtentsHint")]
    #[doc(alias = "SetExtentsHint")]
    pub fn set_extents_hint(
        &self,
        edit: &mut SchemaEdit<'_>,
        extents: &[Extent],
    ) -> Result<&Self, ExtentsHintError> {
        self.write_extents_hint(edit, None, extents)
    }

    /// [`Self::set_extents_hint`] as a time sample at the time code `time`.
    ///
    /// # Errors
    /// As for [`Self::set_extents_hint`].
    #[doc(alias = "UsdGeomModelAPI::SetExtentsHint")]
    #[doc(alias = "SetExtentsHint")]
    pub fn set_extents_hint_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        extents: &[Extent],
    ) -> Result<&Self, ExtentsHintError> {
        self.write_extents_hint(edit, Some(time), extents)
    }

    fn write_extents_hint(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: Option<f64>,
        extents: &[Extent],
    ) -> Result<&Self, ExtentsHintError> {
        if !(1..=4).contains(&extents.len()) {
            return Err(ExtentsHintError::Count {
                count: extents.len(),
            });
        }
        let ty = PropertyType::parse("float3[]").expect("a registered value type");
        match edit.attribute_type(self.path(), EXTENTS_HINT) {
            Some(existing) if existing != ty => {
                return Err(ExtentsHintError::IncompatibleAttribute {
                    type_name: existing.type_name().into(),
                });
            }
            Some(_) => {}
            None => edit.create_attribute(self.path(), EXTENTS_HINT, ty),
        }
        let points: Vec<[f32; 3]> = extents.iter().flatten().copied().collect();
        let value: Value = crate::value::write_float3_array_owned(points, edit.tokens());
        edit.set_value(self.path(), EXTENTS_HINT, time, value);
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bounds::Range3d;
    use crate::usd_geom::Xform;
    use alloc::sync::Arc;
    use layerstack::{InMemoryStore, Layer, LayerId, LiveStage, StageOptions, edit::EditTarget};

    #[test]
    fn hints_hold_one_to_four_extents_and_an_empty_range_is_float_max() {
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer::new(LayerId(1)));
        let path = store.path("/Model");
        let options = StageOptions {
            schemas: Some(Arc::new(crate::openusd(store.tokens()))),
            ..StageOptions::default()
        };
        let live = LiveStage::compose(&mut store, LayerId(1), options);
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        Xform::define(&mut edit, path).expect("outside any instance");
        let model = GeomModelApiEdit::new(&edit, path)
            .expect("outside any instance")
            .unwrap();
        let before = edit.transaction().clone();
        let empty = Range3d::default().to_extent();
        assert_eq!(empty, [[f32::MAX; 3], [-f32::MAX; 3]]);
        assert_eq!(
            model.set_extents_hint(&mut edit, &[]),
            Err(ExtentsHintError::Count { count: 0 })
        );
        assert_eq!(
            model.set_extents_hint(&mut edit, &[empty; 5]),
            Err(ExtentsHintError::Count { count: 5 })
        );
        assert_eq!(edit.transaction(), &before, "refusals author nothing");
        model.set_extents_hint(&mut edit, &[empty; 4]).unwrap();
        assert_eq!(
            edit.attribute_type(path, EXTENTS_HINT),
            PropertyType::parse("float3[]").ok()
        );
    }
}
