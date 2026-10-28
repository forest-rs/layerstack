// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authoring a document: apply a transaction, then load what it asks for.

use super::*;
use alloc::boxed::Box;
use layerstack::{Applied, EditError, Transaction};

/// Why [`StageDocument::apply`] failed.
#[derive(Clone, Debug)]
pub enum ApplyError {
    /// The transaction was rejected ([`LiveStage::apply`]); nothing was
    /// applied and nothing was loaded.
    Edit(EditError),
    /// The transaction was applied, but loading the assets the stage asks
    /// for failed ([`StageDocument::load_requested_assets`]). The layers
    /// earlier rounds loaded stay loaded.
    Load {
        /// What the transaction did, with the inverse that undoes it.
        applied: Box<Applied>,
        /// The load failure.
        error: IoError,
    },
}
impl ApplyError {
    /// The error in words, a rejected edit's paths and names written as
    /// text with `store`, the document's store ([`EditError::display`]).
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl core::fmt::Display + 'a {
        struct Shown<'a>(&'a ApplyError, &'a dyn LayerStore);
        impl core::fmt::Display for Shown<'_> {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                match self.0 {
                    ApplyError::Edit(error) => write!(f, "{}", error.display(self.1)),
                    load => write!(f, "{load}"),
                }
            }
        }
        Shown(self, store)
    }
}
impl core::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Edit(error) => write!(f, "{error}"),
            Self::Load { error, .. } => {
                write!(
                    f,
                    "the edit was applied, but loading an asset failed: {error}"
                )
            }
        }
    }
}
impl core::error::Error for ApplyError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Edit(error) => Some(error),
            Self::Load { error, .. } => Some(error),
        }
    }
}
impl From<EditError> for ApplyError {
    fn from(error: EditError) -> Self {
        Self::Edit(error)
    }
}

impl<B: Storage> StageDocument<B> {
    /// Applies `txn` to the document's stage ([`LiveStage::apply`]), then
    /// loads the assets the stage asks for
    /// ([`Self::load_requested_assets`]): the obvious way to author a
    /// document.
    ///
    /// An edit can introduce asset requests
    /// ([`Applied::asset_requests`]): a deferred payload or reference it
    /// adds, a sublayer by asset path it adds to any layer the stage reads
    /// ([`layerstack::SublayerEntry::unresolved`]), a variant selection
    /// that reaches one, or a changed input of a dynamic file format's
    /// arguments. Those arcs contribute nothing until
    /// their assets are loaded, so this loads them before it returns, as
    /// opening the document does. It is the same as
    /// [`Self::parts_mut`], `LiveStage::apply` and
    /// [`Self::load_requested_assets`] in turn. With nothing requested, the
    /// report is empty and nothing is read.
    ///
    /// ```no_run
    /// # #[cfg(feature = "std")]
    /// # fn example() -> Result<(), Box<dyn core::error::Error>> {
    /// use layerstack::{
    ///     EditTarget, LayerOffset, ListPosition, Reference, ReferenceTarget, Specifier,
    ///     StageOptions, Time, Transaction,
    /// };
    /// use layerstack_io::{Filesystem, StageDocument};
    ///
    /// let storage = Filesystem::new("garden", [])?;
    /// let mut doc = StageDocument::create(storage, "garden.usda", StageOptions::default())?;
    /// let root = doc.stage().stage().root_layer().unwrap();
    /// let shrub = doc.store_mut().path("/Shrub");
    /// let at = EditTarget::for_layer(root);
    /// let mut edit = Transaction::new();
    /// edit.create_prim(at.prim(shrub), Specifier::Def, None);
    /// let plant = Reference::deferred(
    ///     root,
    ///     "./plant.usda",
    ///     ReferenceTarget::DefaultPrim,
    ///     LayerOffset::IDENTITY,
    /// );
    /// edit.add_payload(at.prim(shrub), plant, ListPosition::BackOfPrepend);
    ///
    /// let (applied, loaded) = doc.apply(&edit)?;
    /// assert_eq!(applied.asset_requests.len(), 1); // the payload asked for `./plant.usda`
    /// assert_eq!(loaded.layers.len(), 1); // and `apply` loaded it
    /// let height = doc.view().attribute("/Shrub.height").unwrap();
    /// println!("{:?}", height.value::<f64>(Time::Default));
    /// doc.save();
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// [`ApplyError::Edit`] when the transaction is rejected, and nothing
    /// changed; [`ApplyError::Load`] when it was applied but a load failed
    /// (see [`Self::load_requested_assets`]), with the [`Applied`] whose
    /// inverse undoes it.
    pub fn apply(&mut self, txn: &Transaction) -> Result<(Applied, LoadReport), ApplyError> {
        let applied = self.stage.apply(&mut self.store, txn)?;
        match self.load_requested_assets() {
            Ok(report) => Ok((applied, report)),
            Err(error) => Err(ApplyError::Load {
                applied: Box::new(applied),
                error,
            }),
        }
    }
}
