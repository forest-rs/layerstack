// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Per-file replace-if-changed publication. Rendering and formatting happen first.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub(crate) fn write_if_changed(path: &Path, text: &str) -> Result<(), String> {
    publish(path, text.as_bytes(), |file, bytes| file.write_all(bytes))
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn publish(
    path: &Path,
    bytes: &[u8],
    write: impl FnOnce(&mut File, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(io::Error::other(
                    "generated output must be a regular file, not a symlink or directory",
                ));
            }
            if fs::read(path)? == bytes {
                return Ok(());
            }
            if metadata.permissions().readonly() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "generated output is read-only",
                ));
            }
            Some(metadata)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("generated output requires a filename"))?;
    let (temporary, mut file) = loop {
        let mut sibling = name.to_os_string();
        sibling.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let temporary = parent.join(sibling);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => break (Temporary(temporary), file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    if let Some(metadata) = metadata {
        file.set_permissions(metadata.permissions())?;
    }
    write(&mut file, bytes)?;
    file.flush()?;
    drop(file); // Windows replacement requires our handle to be closed.
    fs::rename(&temporary.0, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "layerstack-generator-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn unchanged_outputs_do_not_invoke_the_writer_or_change_metadata() {
        let directory = Directory::new();
        let path = directory.0.join("generated.rs");
        fs::write(&path, "original").unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        publish(&path, b"original", |_, _| {
            panic!("unchanged output must not be written")
        })
        .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().modified().unwrap(),
            modified,
            "unchanged timestamp is preserved"
        );
        assert_eq!(
            fs::read_dir(&directory.0).unwrap().count(),
            1,
            "no temporary file is created"
        );
    }
    #[test]
    fn partial_write_failure_preserves_original_and_removes_temporary() {
        let directory = Directory::new();
        let path = directory.0.join("generated.rs");
        fs::write(&path, "original").unwrap();
        let error = publish(&path, b"replacement", |file, bytes| {
            file.write_all(&bytes[..3])?;
            Err(io::Error::other("injected write failure"))
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("injected"),
            "writer error is preserved"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "original",
            "partial output never replaces the original"
        );
        assert_eq!(
            fs::read_dir(&directory.0).unwrap().count(),
            1,
            "failed temporary is removed"
        );
    }
    #[test]
    fn replacement_and_new_nested_output_publish_complete_bytes() {
        let directory = Directory::new();
        let path = directory.0.join("nested/generated.rs");
        write_if_changed(&path, "first").unwrap();
        write_if_changed(&path, "replacement").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "replacement",
            "complete output is published"
        );
        assert_eq!(
            fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "successful temporary is consumed"
        );
        let blocked = directory.0.join("blocked");
        fs::create_dir(&blocked).unwrap();
        assert!(
            write_if_changed(&blocked, "x").is_err(),
            "directories are never replaced"
        );
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_are_rejected_and_existing_permissions_are_preserved() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let directory = Directory::new();
        let path = directory.0.join("output");
        fs::write(&path, "original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        write_if_changed(&path, "replacement").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640,
            "replacement preserves ordinary file permissions"
        );
        let link = directory.0.join("link");
        symlink(&path, &link).unwrap();
        assert!(
            write_if_changed(&link, "bad").is_err(),
            "symlink output rejects before writing"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "replacement",
            "symlink target stays unchanged"
        );
    }
}
