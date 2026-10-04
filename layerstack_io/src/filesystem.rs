// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit filesystem identifier/search-path policy.
use super::*;
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

/// Filesystem transport with an explicit root and ordered search directories.
/// Relative assets first try their authoring file's directory, then search
/// directories, then `root`. No process-global resolver context is modified.
#[derive(Clone, Debug)]
pub struct Filesystem {
    root: PathBuf,
    search_paths: Vec<PathBuf>,
}
impl Filesystem {
    /// Creates a filesystem backend. Relative roots/search paths are captured
    /// against the current directory once, avoiding later working-directory drift.
    pub fn new(
        root: impl AsRef<Path>,
        search_paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, IoError> {
        let cwd = std::env::current_dir().map_err(storage_error)?;
        let absolute = |p: &Path| {
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                cwd.join(p)
            }
        };
        Ok(Self {
            root: absolute(root.as_ref()),
            search_paths: search_paths.into_iter().map(|p| absolute(&p)).collect(),
        })
    }
}
impl Storage for Filesystem {
    fn identify(&self, asset: &str, anchor: Option<&str>) -> Result<String, IoError> {
        if asset.contains("://") || asset.contains('[') {
            return Err(IoError::new(
                IoErrorKind::Unsupported,
                "filesystem backend requires an unpackaged file identifier",
            ));
        }
        let path = Path::new(asset);
        let file_relative = asset.starts_with("./") || asset.starts_with("../");
        let candidates = if path.is_absolute() {
            alloc::vec![path.to_path_buf()]
        } else if file_relative {
            alloc::vec![
                anchor
                    .and_then(|a| Path::new(a).parent())
                    .unwrap_or(&self.root)
                    .join(path)
            ]
        } else {
            anchor
                .and_then(|a| Path::new(a).parent())
                .map(|p| p.join(path))
                .into_iter()
                .chain(self.search_paths.iter().map(|p| p.join(path)))
                .chain(core::iter::once(self.root.join(path)))
                .collect()
        };
        let selected = candidates
            .iter()
            .find(|p| p.is_file())
            .or_else(|| candidates.last())
            .expect("root candidate");
        let selected = fs::canonicalize(selected).unwrap_or_else(|_| normalize(selected));
        selected.to_str().map(ToString::to_string).ok_or_else(|| {
            IoError::new(
                IoErrorKind::Unsupported,
                "USD identifiers require UTF-8 filesystem paths",
            )
        })
    }
    fn read(&mut self, identifier: &str) -> Result<Vec<u8>, IoError> {
        fs::read(identifier).map_err(storage_error)
    }
    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        fs::write(identifier, bytes).map_err(storage_error)
    }
}
fn storage_error(error: std::io::Error) -> IoError {
    let kind = if error.kind() == std::io::ErrorKind::NotFound {
        IoErrorKind::NotFound
    } else {
        IoErrorKind::Storage
    };
    IoError::new(kind, error.to_string())
}
fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            _ => result.push(component.as_os_str()),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_relative_assets_do_not_fall_through_to_search_directories() {
        #[cfg(target_os = "wasi")]
        let root = std::env::current_dir()
            .unwrap()
            .join("layerstack-filesystem-policy");
        #[cfg(not(target_os = "wasi"))]
        let root = std::env::temp_dir().join(alloc::format!(
            "layerstack-filesystem-policy-{}",
            std::process::id()
        ));
        let authored = root.join("authored");
        let search = root.join("search");
        fs::create_dir_all(&authored).unwrap();
        fs::create_dir_all(&search).unwrap();
        fs::write(search.join("asset.usda"), b"#usda 1.0\n").unwrap();
        let mut storage = Filesystem::new(&root, [search.clone()]).unwrap();
        let anchor = authored.join("root.usda");
        let anchor = anchor.to_str().unwrap();
        let search_path = storage.identify("asset.usda", Some(anchor)).unwrap();
        assert_eq!(
            Path::new(&search_path),
            fs::canonicalize(search.join("asset.usda")).unwrap()
        );
        let file_relative = storage.identify("./asset.usda", Some(anchor)).unwrap();
        assert_eq!(Path::new(&file_relative), authored.join("asset.usda"));
        assert_eq!(
            storage.read(&file_relative).unwrap_err().kind,
            IoErrorKind::NotFound
        );
        fs::remove_dir_all(root).unwrap();
    }
}
