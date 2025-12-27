//! String and path interning for `layerstack`.
//!
//! This module keeps the public types small (`TokenId`, `PathId`) while allowing
//! deterministic, stable IDs throughout the composition engine.
//!
//! Spec: AOUSD Core uses tokens pervasively; this crate uses interning to model
//! token identity efficiently (see AOUSD Core terminology around tokens).

use alloc::{sync::Arc, vec::Vec};

use hashbrown::HashMap;

/// A stable identifier for an interned token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenId(u32);

impl TokenId {
    /// Returns the underlying numeric identifier.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Interns strings into [`TokenId`] values.
#[derive(Debug, Default)]
pub struct TokenInterner {
    by_str: HashMap<Arc<str>, TokenId>,
    strings: Vec<Arc<str>>,
}

impl TokenInterner {
    /// Interns a string, returning its stable [`TokenId`].
    #[must_use]
    pub fn intern(&mut self, s: impl AsRef<str>) -> TokenId {
        let s_ref = s.as_ref();
        if let Some(id) = self.by_str.get(s_ref) {
            return *id;
        }

        let id = TokenId(u32::try_from(self.strings.len()).expect("token interner overflow"));
        let arc: Arc<str> = Arc::from(s_ref);
        self.strings.push(arc.clone());
        self.by_str.insert(arc, id);
        id
    }

    /// Resolves a [`TokenId`] back to its string.
    #[must_use]
    pub fn resolve(&self, id: TokenId) -> &str {
        &self.strings[usize::try_from(id.0).expect("token id out of range")]
    }
}
