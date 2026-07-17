use crate::SandboxError;
use std::path::{Path, PathBuf};

pub fn normalize_existing_path(path: &Path) -> Result<PathBuf, SandboxError> {
    if !path.exists() {
        return Err(SandboxError::MissingPath(path.to_path_buf()));
    }
    dunce::canonicalize(path).map_err(|err| {
        SandboxError::Operation(anyhow::anyhow!(
            "canonicalize {} failed: {err}",
            path.display()
        ))
    })
}

pub fn canonical_path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

pub fn path_is_within(root: &Path, path: &Path) -> bool {
    let root = canonical_path_key(root);
    let path = canonical_path_key(path);
    path == root
        || path
            .strip_prefix(&root)
            .is_some_and(|suffix| suffix.starts_with('\\'))
}

/// Reject Windows reparse points on every existing component of `path`.
///
/// `std::fs::Metadata::file_type().is_symlink()` does not identify all Windows
/// junctions and other reparse points. This check is used by the host-side
/// preview policy before file operations; a missing leaf is allowed, but its
/// existing parent chain must still be ordinary directories.
pub fn ensure_no_reparse_points(path: &Path) -> Result<(), SandboxError> {
    use crate::winutil::path_to_wide;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, GetFileAttributesW, INVALID_FILE_ATTRIBUTES,
    };

    let mut current = path.to_path_buf();
    loop {
        if let Ok(metadata) = std::fs::symlink_metadata(&current) {
            if metadata.file_type().is_symlink() {
                return Err(SandboxError::Operation(anyhow::anyhow!(
                    "Windows symlink is not allowed in sandboxed path: {}",
                    current.display()
                )));
            }
            let attributes = unsafe { GetFileAttributesW(path_to_wide(&current).as_ptr()) };
            if attributes == INVALID_FILE_ATTRIBUTES {
                return Err(SandboxError::Operation(anyhow::anyhow!(
                    "GetFileAttributesW failed for {}",
                    current.display()
                )));
            }
            if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(SandboxError::Operation(anyhow::anyhow!(
                    "Windows reparse point is not allowed in sandboxed path: {}",
                    current.display()
                )));
            }
        }
        if !current.pop() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::canonical_path_key;
    use super::path_is_within;
    use std::path::Path;

    #[test]
    fn canonical_key_is_case_and_separator_insensitive() {
        assert_eq!(
            canonical_path_key(Path::new(r"C:\Users\Dev\Repo")),
            canonical_path_key(Path::new("c:/users/dev/repo/"))
        );
    }

    #[test]
    fn containment_uses_component_boundaries() {
        assert!(path_is_within(
            Path::new(r"C:\workspace"),
            Path::new(r"c:/workspace\nested")
        ));
        assert!(!path_is_within(
            Path::new(r"C:\workspace"),
            Path::new(r"C:\workspace-other")
        ));
    }

    #[test]
    fn containment_is_case_insensitive_for_windows_paths() {
        assert!(path_is_within(
            Path::new(r"C:\Workspace"),
            Path::new(r"c:\workspace\src\main.rs")
        ));
    }
}
