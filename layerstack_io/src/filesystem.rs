// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit filesystem identifier/search-path policy.
use super::*;
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Filesystem transport with an explicit root and ordered search directories.
/// Relative assets first try their authoring file's directory, then search
/// directories, then `root`. No process-global resolver context is modified.
///
/// An identifier is the absolute path of the file found, with `.` and `..`
/// segments removed from its spelling: identifying asks the filesystem
/// only which candidate exists, and nothing when there is one candidate.
/// Symbolic links are not resolved and names keep their authored case, so
/// a file reached through two different spellings, such as a link and its
/// target, is two layers.
///
/// Spec: AOUSD Core §9.4 (anchored and search-path identifiers), §9.5
/// (relative resolution and normalization, RFC 3986 §5.2 and §6). OpenUSD's
/// `ArDefaultResolver` identifies files the same way (`TfAbsPath`,
/// `TfNormPath`).
///
/// Writes stage a complete sibling file, sync its contents, then rename it over
/// the destination. Errors before replacement leave the destination intact.
/// Writes follow existing symlinks. Replacement changes file identity (and breaks
/// hard links); this is not a transaction across files or a power-loss durability
/// guarantee for the directory entry. Existing permissions are copied on native
/// platforms; WASI relies on host-granted filesystem rights. On macOS
/// the standard library also copies extended metadata. Other platforms' extended
/// access controls require a host-specific [`Storage`] backend.
/// Range sources retain an open handle across atomic replacement. Hosts must
/// exclude external in-place writes for their lifetime; size/mtime checks reject
/// observed changes but do not provide a filesystem snapshot.
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
        let anchor_directory = || anchor.and_then(|a| Path::new(a).parent());
        let selected = if path.is_absolute() {
            normalize(path)
        } else if file_relative {
            normalize(&anchor_directory().unwrap_or(&self.root).join(path))
        } else {
            // The first candidate that is a file, or else the root's.
            let mut candidates = anchor_directory()
                .into_iter()
                .chain(self.search_paths.iter().map(PathBuf::as_path))
                .map(|directory| normalize(&directory.join(path)));
            candidates
                .find(|candidate| candidate.is_file())
                .unwrap_or_else(|| normalize(&self.root.join(path)))
        };
        selected.to_str().map(ToString::to_string).ok_or_else(|| {
            IoError::new(
                IoErrorKind::Unsupported,
                "USD identifiers require UTF-8 filesystem paths",
            )
        })
    }
    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        fs::read(identifier).map_err(storage_error)
    }
    fn read_bounded(&self, identifier: &str, max_bytes: u64) -> Result<Vec<u8>, IoError> {
        use std::io::Read;
        let file = fs::File::open(identifier).map_err(storage_error)?;
        if file.metadata().map_err(storage_error)?.len() > max_bytes {
            return Err(IoError::new(
                IoErrorKind::Rejected,
                "resource exceeds encoded byte budget",
            ));
        }
        let limit = max_bytes
            .checked_add(1)
            .ok_or_else(|| IoError::new(IoErrorKind::Rejected, "resource byte budget overflow"))?;
        let mut bytes = Vec::new();
        file.take(limit)
            .read_to_end(&mut bytes)
            .map_err(storage_error)?;
        if bytes.len() as u64 > max_bytes {
            return Err(IoError::new(
                IoErrorKind::Rejected,
                "resource exceeds encoded byte budget",
            ));
        }
        Ok(bytes)
    }
    fn open_ranges(
        &self,
        identifier: &str,
    ) -> Result<Arc<dyn layerstack_usdz::range::RangeSource<Error = IoError> + Send + Sync>, IoError>
    {
        let file = fs::File::open(identifier).map_err(storage_error)?;
        let metadata = file.metadata().map_err(storage_error)?;
        if !metadata.is_file() {
            return Err(IoError::new(
                IoErrorKind::Unsupported,
                "range source must be a regular file",
            ));
        }
        Ok(Arc::new(FileRanges {
            length: metadata.len(),
            modified: metadata.modified().map_err(storage_error)?,
            file: std::sync::Mutex::new(file),
        }))
    }
    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        atomic_write(Path::new(identifier), |file| file.write_all(bytes)).map_err(storage_error)
    }
}

/// A retained open handle survives atomic replacement. Callers must exclude
/// external in-place writes; length/mtime checks detect ordinary changes but are
/// not a filesystem snapshot or a defense against writers restoring timestamps.
/// A private cursor lock keeps concurrent range reads independent on all platforms.
struct FileRanges {
    file: std::sync::Mutex<fs::File>,
    length: u64,
    modified: std::time::SystemTime,
}
impl layerstack_usdz::range::RangeSource for FileRanges {
    type Error = IoError;
    fn byte_len(&self) -> u64 {
        self.length
    }
    fn read_range(&self, range: core::ops::Range<u64>) -> Result<EncodedBytes, IoError> {
        use std::io::{Read, Seek, SeekFrom};
        let size = range
            .end
            .checked_sub(range.start)
            .filter(|_| range.end <= self.length)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| IoError::new(IoErrorKind::Rejected, "invalid source range"))?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| IoError::new(IoErrorKind::Storage, "range source lock poisoned"))?;
        let check = |file: &fs::File| -> Result<(), IoError> {
            let metadata = file.metadata().map_err(storage_error)?;
            if metadata.len() != self.length
                || metadata.modified().map_err(storage_error)? != self.modified
            {
                return Err(IoError::new(
                    IoErrorKind::Storage,
                    "range source changed in place",
                ));
            }
            Ok(())
        };
        check(&file)?;
        file.seek(SeekFrom::Start(range.start))
            .map_err(storage_error)?;
        let mut bytes = alloc::vec![0; size];
        file.read_exact(&mut bytes).map_err(storage_error)?;
        check(&file)?;
        Ok(bytes.into())
    }
}

static NEXT_CANDIDATE: AtomicU64 = AtomicU64::new(0);

/// The callback seam permits deterministic partial-write failures in tests.
fn atomic_write(
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let target = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => fs::canonicalize(path)?,
        Ok(_) => path.to_path_buf(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => return Err(error),
    };
    let metadata = match fs::metadata(&target) {
        Ok(metadata) => {
            // WASI exposes filesystem rights rather than file permission bits.
            if !metadata.is_file()
                || (!cfg!(target_os = "wasi") && metadata.permissions().readonly())
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "destination is not a writable regular file",
                ));
            }
            Some(metadata)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // WASI has no process ID. create_new still arbitrates candidate collisions.
    #[cfg(target_os = "wasi")]
    let process = 0;
    #[cfg(not(target_os = "wasi"))]
    let process = std::process::id();
    // Close the candidate handle before replacement, including on Windows.
    let candidate = {
        let (candidate, mut file) = loop {
            let serial = NEXT_CANDIDATE.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(alloc::format!(".layerstack-save-{process}-{serial}"));
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&candidate) {
                Ok(file) => break (Candidate(candidate), file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        // TODO: Preserve ACLs on Windows/Linux through suitable Rust platform APIs,
        // and verify replacement retains access controls with native regression tests.
        // macOS std::fs::copy uses COPYFILE_ALL, preserving ACLs and extended metadata
        // as well as mode. Copy before truncating the candidate, never the destination.
        #[cfg(target_os = "macos")]
        if metadata.is_some() {
            fs::copy(&target, &candidate.0)?;
            file.set_len(0)?;
        }
        write(&mut file)?;
        if let Some(metadata) = metadata
            && !cfg!(target_os = "wasi")
        {
            file.set_permissions(metadata.permissions())?;
        }
        file.sync_all()?;
        candidate
    };
    fs::rename(&candidate.0, &target)?;
    Ok(())
}

struct Candidate(PathBuf);
impl Drop for Candidate {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
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
/// `path` with its `.` and `..` segments removed, as RFC 3986 §5.2.4 removes
/// dot segments: a `..` drops the segment before it, whatever that names.
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
    fn range_sources_keep_old_handles_and_views_after_atomic_replacement() {
        #[cfg(target_os = "wasi")]
        let base = std::env::current_dir().unwrap();
        #[cfg(not(target_os = "wasi"))]
        let base = std::env::temp_dir();
        let root = base.join(alloc::format!(
            "layerstack-ranges-{}",
            NEXT_CANDIDATE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("scene.usdz");
        let identifier = target.to_str().unwrap();
        let mut storage = Filesystem::new(&root, []).unwrap();
        storage.write(identifier, b"old bytes").unwrap();
        let source = storage.open_ranges(identifier).unwrap();
        let view = source.read_range(0..3).unwrap();
        assert!(
            source
                .read_range(core::ops::Range { start: 4, end: 2 })
                .is_err()
        );
        assert!(source.read_range(0..10).is_err());
        storage.write(identifier, b"new bytes").unwrap();
        assert_eq!(source.read_range(4..9).unwrap().as_ref(), b"bytes");
        assert_eq!(source.read_range(0..3).unwrap().as_ref(), b"old");
        let new = storage.open_ranges(identifier).unwrap();
        assert_eq!(new.read_range(0..3).unwrap().as_ref(), b"new");
        // A length change deterministically detects an external in-place write.
        fs::write(&target, b"changed length").unwrap();
        assert_eq!(new.read_range(0..3).unwrap_err().kind, IoErrorKind::Storage);
        drop(source);
        drop(new);
        fs::remove_dir_all(root).unwrap();
        assert_eq!(view.as_ref(), b"old");
    }

    // wasm32-wasip1 has no thread spawning support.
    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn concurrent_replacements_publish_complete_files_and_clean_candidates() {
        use std::sync::{Arc, Barrier, atomic::AtomicUsize};
        let root = std::env::temp_dir().join(alloc::format!(
            "layerstack-save-concurrent-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("scene.usda");
        const SIZE: usize = 128 * 1024;
        fs::write(&target, alloc::vec![0; SIZE]).unwrap();
        let barrier = Arc::new(Barrier::new(5));
        let pending = Arc::new(AtomicUsize::new(4));
        std::thread::scope(|scope| {
            for value in 1..=4_u8 {
                let target = &target;
                let barrier = barrier.clone();
                let pending = pending.clone();
                scope.spawn(move || {
                    let bytes = alloc::vec![value; SIZE];
                    barrier.wait();
                    let result = atomic_write(target, |file| file.write_all(&bytes));
                    pending.fetch_sub(1, Ordering::Release);
                    result.unwrap();
                });
            }
            barrier.wait();
            loop {
                let bytes = fs::read(&target).unwrap();
                assert_eq!(bytes.len(), SIZE);
                assert!(bytes[0] <= 4);
                assert!(bytes.iter().all(|value| *value == bytes[0]));
                if pending.load(Ordering::Acquire) == 0 {
                    break;
                }
            }
        });
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn replacement_preserves_extended_metadata() {
        use std::process::Command;
        let root = std::env::temp_dir().join(alloc::format!(
            "layerstack-save-metadata-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("scene.usda");
        fs::write(&target, b"previous scene").unwrap();
        let attribute = "com.layerstack.save-test";
        let set = Command::new("/usr/bin/xattr")
            .args(["-w", attribute, "preserved"])
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            set.status.success(),
            "{}",
            String::from_utf8_lossy(&set.stderr)
        );
        atomic_write(&target, |file| file.write_all(b"replacement")).unwrap();
        let get = Command::new("/usr/bin/xattr")
            .args(["-p", attribute])
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            get.status.success(),
            "{}",
            String::from_utf8_lossy(&get.stderr)
        );
        assert_eq!(get.stdout, b"preserved\n");
        assert_eq!(fs::read(&target).unwrap(), b"replacement");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_write_and_replace_failure_preserve_destination_and_clean_candidates() {
        #[cfg(target_os = "wasi")]
        let root = std::env::current_dir()
            .unwrap()
            .join("layerstack-atomic-save-wasi");
        #[cfg(not(target_os = "wasi"))]
        let root = std::env::temp_dir().join(alloc::format!(
            "layerstack-atomic-save-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("scene.usda");
        fs::write(&target, b"previous scene").unwrap();
        let error = atomic_write(&target, |file| {
            file.write_all(b"partial replacement")?;
            Err(std::io::Error::other("injected write failure"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "injected write failure");
        assert_eq!(fs::read(&target).unwrap(), b"previous scene");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        // Force rename failure after a complete write without touching the old file.
        let moved = root.join("moved.usda");
        atomic_write(&target, |file| {
            file.write_all(b"complete candidate")?;
            fs::rename(&target, &moved)?;
            fs::create_dir(&target)
        })
        .unwrap_err();
        assert_eq!(fs::read(&moved).unwrap(), b"previous scene");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        fs::remove_dir(&target).unwrap();
        let mut storage = Filesystem::new(&root, []).unwrap();
        storage
            .write(target.to_str().unwrap(), b"new scene")
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new scene");
        storage
            .write(target.to_str().unwrap(), b"replacement scene")
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"replacement scene");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replacement_follows_symlink_and_preserves_mode_and_readonly_policy() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = std::env::temp_dir().join(alloc::format!(
            "layerstack-save-mode-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("scene.usda");
        let link = root.join("link.usda");
        fs::write(&target, b"old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&target, &link).unwrap();
        atomic_write(&link, |file| file.write_all(b"new")).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::set_permissions(&target, fs::Permissions::from_mode(0o400)).unwrap();
        assert!(atomic_write(&target, |file| file.write_all(b"forbidden")).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"new");
        fs::remove_dir_all(root).unwrap();
    }

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
        let storage = Filesystem::new(&root, [search.clone()]).unwrap();
        let anchor = authored.join("root.usda");
        let anchor = anchor.to_str().unwrap();
        let search_path = storage.identify("asset.usda", Some(anchor)).unwrap();
        assert_eq!(Path::new(&search_path), search.join("asset.usda"));
        let file_relative = storage.identify("./asset.usda", Some(anchor)).unwrap();
        assert_eq!(Path::new(&file_relative), authored.join("asset.usda"));
        assert_eq!(
            storage.read(&file_relative).unwrap_err().kind,
            IoErrorKind::NotFound
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Identifiers are spellings with their dot segments removed: a link
    /// keeps its own path, so a file reached through a link and through
    /// its target is two identifiers, and so two layers.
    #[cfg(unix)]
    #[test]
    fn identifiers_remove_dot_segments_and_keep_links() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(alloc::format!(
            "layerstack-filesystem-lexical-{}",
            std::process::id()
        ));
        let real = root.join("real");
        fs::create_dir_all(real.join("nested")).unwrap();
        fs::write(real.join("asset.usda"), b"#usda 1.0\n").unwrap();
        symlink(&real, root.join("link")).unwrap();
        let storage = Filesystem::new(&root, []).unwrap();
        let anchor = real.join("nested/root.usda");
        let anchor = anchor.to_str().unwrap();
        assert_eq!(
            Path::new(&storage.identify("../asset.usda", Some(anchor)).unwrap()),
            real.join("asset.usda")
        );
        assert_eq!(
            Path::new(
                &storage
                    .identify("./sub/.././../asset.usda", Some(anchor))
                    .unwrap()
            ),
            real.join("asset.usda")
        );
        let through_link = storage.identify("link/asset.usda", None).unwrap();
        assert_eq!(Path::new(&through_link), root.join("link/asset.usda"));
        assert_eq!(storage.read(&through_link).unwrap(), b"#usda 1.0\n");
        // A search candidate is chosen by whether its file exists.
        let found = storage.identify("asset.usda", Some(anchor)).unwrap();
        assert_eq!(Path::new(&found), root.join("asset.usda"));
        let storage = Filesystem::new(&root, [real.clone()]).unwrap();
        let found = storage.identify("asset.usda", Some(anchor)).unwrap();
        assert_eq!(Path::new(&found), real.join("asset.usda"));
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod asset_byte_tests {
    use super::*;
    use layerstack::StageOptions;
    #[test]
    fn arbitrary_bytes_follow_the_winning_layer_location_and_external_package_anchor() {
        #[cfg(target_os = "wasi")]
        let root = std::env::current_dir()
            .unwrap()
            .join("layerstack-asset-bytes");
        #[cfg(not(target_os = "wasi"))]
        let root = std::env::temp_dir().join(alloc::format!(
            "layerstack-asset-bytes-{}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("layers")).unwrap();
        fs::write(
            root.join("root.usda"),
            b"#usda 1.0\n(subLayers=[@layers/base.usda@])",
        )
        .unwrap();
        fs::write(root.join("layers/base.usda"), b"#usda 1.0\ndef \"Root\" {}").unwrap();
        fs::write(root.join("layers/image.png"), b"member directory").unwrap();
        fs::write(root.join("image.png"), b"root directory").unwrap();
        fs::write(
            root.join("scene.usdz"),
            layerstack_usdz::write_usdz(&[layerstack_usdz::PackageFile::new(
                "root.usda",
                b"#usda 1.0",
            )])
            .unwrap(),
        )
        .unwrap();
        let mut document = StageDocument::open(
            Filesystem::new(&root, []).unwrap(),
            "root.usda",
            StageOptions::default(),
        )
        .unwrap();
        let layer = document
            .load_report()
            .layers
            .iter()
            .copied()
            .find(|id| document.identifier(*id).unwrap().ends_with("base.usda"))
            .unwrap();
        let bytes = document
            .read_asset_bytes("./image.png", Some(layer))
            .unwrap();
        assert_eq!(bytes.bytes.as_ref(), b"member directory");
        assert_eq!(Path::new(&bytes.identifier), root.join("layers/image.png"));
        let mut package = StageDocument::open(
            Filesystem::new(&root, []).unwrap(),
            "scene.usdz",
            StageOptions::default(),
        )
        .unwrap();
        let package_root = package.stage().stage().root_layer().unwrap();
        assert_eq!(
            package
                .read_asset_bytes("image.png", Some(package_root))
                .unwrap()
                .bytes
                .as_ref(),
            b"root directory"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod bounded_tests {
    use super::*;
    #[test]
    fn oversized_sparse_resource_rejects_before_reading_payload() {
        #[cfg(target_os = "wasi")]
        let path = std::env::current_dir()
            .unwrap()
            .join("layerstack-sparse-resource-wasm");
        #[cfg(not(target_os = "wasi"))]
        let path = std::env::temp_dir().join(alloc::format!(
            "layerstack-sparse-resource-{}",
            std::process::id()
        ));
        {
            let file = fs::File::create(&path).unwrap();
            file.set_len(2 * 1024 * 1024 * 1024).unwrap();
            let storage = Filesystem::new(".", []).unwrap();
            assert_eq!(
                storage
                    .read_bounded(path.to_str().unwrap(), 1024)
                    .unwrap_err()
                    .kind,
                IoErrorKind::Rejected
            );
        }
        fs::remove_file(path).unwrap();
    }
}
