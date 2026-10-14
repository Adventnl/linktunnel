//! Which paths a controller may touch.
//!
//! The target shares a short list of *roots*. Every path from the wire is checked against them
//! after symlinks are resolved, so a link inside a root that points outside it, a `..` that climbs
//! out, or a path under a read-only root being written to are all refused. Roots are canonicalised
//! once at start-up.
//!
//! This is a check-then-use policy: between resolving a path and opening it, another local
//! process could swap a component for a symlink. That is outside the threat model (an attacker
//! with local write access to a shared folder can already do what they like there), and is listed
//! in `docs/LIMITATIONS.md`.

use std::path::{Component, Path, PathBuf};

use crate::error::FileError;

/// A folder a controller may browse.
#[derive(Debug, Clone)]
pub struct SharedRoot {
    /// Folder on this device.
    pub path: PathBuf,
    /// May the controller create, overwrite and delete inside it?
    pub writable: bool,
}

/// What an operation intends to do to the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Look at it or read it.
    Read,
    /// Create, replace or delete.
    Write,
}

#[derive(Debug, Clone)]
struct Root {
    canonical: PathBuf,
    writable: bool,
}

/// The set of shared folders.
#[derive(Debug, Clone, Default)]
pub struct FileRoots {
    roots: Vec<Root>,
}

impl FileRoots {
    /// Canonicalises `roots`; ones that do not exist are skipped (with a warning).
    #[must_use]
    pub fn new(roots: Vec<SharedRoot>) -> Self {
        let roots = roots
            .into_iter()
            .filter_map(|r| match r.path.canonicalize() {
                Ok(canonical) if canonical.is_dir() => Some(Root {
                    canonical,
                    writable: r.writable,
                }),
                Ok(_) | Err(_) => {
                    tracing::warn!(path = %r.path.display(), "shared folder is missing or not a directory; skipped");
                    None
                }
            })
            .collect();
        Self { roots }
    }

    /// No folders shared: every request is refused.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// The canonical root paths with their writability, for the top-level listing.
    #[must_use]
    pub fn roots(&self) -> Vec<(PathBuf, bool)> {
        self.roots
            .iter()
            .map(|r| (r.canonical.clone(), r.writable))
            .collect()
    }

    /// Is `canonical` exactly one of the roots?
    #[must_use]
    pub fn is_root(&self, canonical: &Path) -> bool {
        self.roots.iter().any(|r| r.canonical == canonical)
    }

    fn syntax(wire: &str) -> Result<PathBuf, FileError> {
        if wire.is_empty() || wire.contains('\0') {
            return Err(FileError::InvalidPath("empty or contains NUL".into()));
        }
        #[cfg(windows)]
        if wire.split(['\\', '/']).any(|part| part == "..") {
            return Err(FileError::InvalidPath(format!("{wire} contains `..`")));
        }
        // Mac/Linux controllers append `/name` to listing paths. Canonical Windows roots
        // carry the verbatim `\\?\` prefix, where Win32 does not accept forward slashes.
        #[cfg(windows)]
        let p = PathBuf::from(wire.replace('/', "\\"));
        #[cfg(not(windows))]
        let p = PathBuf::from(wire);
        if !p.is_absolute() {
            return Err(FileError::InvalidPath(format!("{wire} is not absolute")));
        }
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(FileError::InvalidPath(format!("{wire} contains `..`")));
        }
        Ok(p)
    }

    fn check(&self, canonical: &Path, access: Access) -> Result<(), FileError> {
        // The longest matching root decides (roots may nest).
        let root = self
            .roots
            .iter()
            .filter(|r| canonical.starts_with(&r.canonical))
            .max_by_key(|r| r.canonical.components().count())
            .ok_or_else(|| {
                FileError::PermissionDenied(format!(
                    "{} is outside the shared folders",
                    canonical.display()
                ))
            })?;
        if access == Access::Write && !root.writable {
            return Err(FileError::PermissionDenied(format!(
                "{} is in a read-only shared folder",
                canonical.display()
            )));
        }
        Ok(())
    }

    /// Resolves an existing path (following symlinks) and checks it.
    pub fn resolve(&self, wire: &str, access: Access) -> Result<PathBuf, FileError> {
        let p = Self::syntax(wire)?;
        let canonical = p.canonicalize().map_err(|e| FileError::from_io(&e, wire))?;
        self.check(&canonical, access)?;
        Ok(canonical)
    }

    /// Resolves a path whose last component may not exist yet (creating or replacing a file).
    /// The parent must exist and be inside a root; if the last component is an existing symlink
    /// its target is checked too, since writing would go through it.
    pub fn resolve_new(&self, wire: &str, access: Access) -> Result<PathBuf, FileError> {
        let p = Self::syntax(wire)?;
        let name = p
            .file_name()
            .ok_or_else(|| FileError::InvalidPath(format!("{wire} has no file name")))?;
        let parent = p
            .parent()
            .ok_or_else(|| FileError::InvalidPath(format!("{wire} has no parent")))?;
        let parent = parent
            .canonicalize()
            .map_err(|e| FileError::from_io(&e, &parent.display().to_string()))?;
        let full = parent.join(name);
        match full.symlink_metadata() {
            Ok(m) if m.file_type().is_symlink() => {
                let target = full
                    .canonicalize()
                    .map_err(|e| FileError::from_io(&e, wire))?;
                self.check(&target, access)?;
                Ok(target)
            }
            _ => {
                self.check(&full, access)?;
                Ok(full)
            }
        }
    }

    /// Like [`resolve_new`](Self::resolve_new) but never follows a final symlink: used to delete
    /// the link itself rather than what it points at.
    pub fn resolve_entry(&self, wire: &str, access: Access) -> Result<PathBuf, FileError> {
        let p = Self::syntax(wire)?;
        let name = p
            .file_name()
            .ok_or_else(|| FileError::InvalidPath(format!("{wire} has no file name")))?;
        let parent = p
            .parent()
            .ok_or_else(|| FileError::InvalidPath(format!("{wire} has no parent")))?;
        let parent = parent
            .canonicalize()
            .map_err(|e| FileError::from_io(&e, &parent.display().to_string()))?;
        let full = parent.join(name);
        self.check(&full, access)?;
        Ok(full)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, FileRoots) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("share/sub")).unwrap();
        std::fs::create_dir_all(dir.path().join("ro")).unwrap();
        std::fs::create_dir_all(dir.path().join("secret")).unwrap();
        std::fs::write(dir.path().join("share/a.txt"), b"a").unwrap();
        std::fs::write(dir.path().join("secret/key"), b"k").unwrap();
        let roots = FileRoots::new(vec![
            SharedRoot {
                path: dir.path().join("share"),
                writable: true,
            },
            SharedRoot {
                path: dir.path().join("ro"),
                writable: false,
            },
        ]);
        (dir, roots)
    }

    fn s(p: &Path) -> String {
        p.to_str().unwrap().to_owned()
    }

    #[test]
    fn paths_inside_roots_resolve() {
        let (d, r) = setup();
        assert!(
            r.resolve(&s(&d.path().join("share/a.txt")), Access::Read)
                .is_ok()
        );
        assert!(
            r.resolve(&s(&d.path().join("share/sub")), Access::Write)
                .is_ok()
        );
        assert!(
            r.resolve_new(&s(&d.path().join("share/new.bin")), Access::Write)
                .is_ok()
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_controller_can_append_forward_slashes_to_a_windows_listing_path() {
        let (dir, roots) = setup();
        let listing = roots.roots()[0].0.clone();
        let wire = format!("{}/from-mac.bin", listing.display());
        let resolved = roots.resolve_new(&wire, Access::Write).unwrap();
        std::fs::write(&resolved, b"from the controller").unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("share/from-mac.bin")).unwrap(),
            b"from the controller"
        );
        assert!(matches!(
            roots.resolve_new(
                &format!("{}/../escape.bin", listing.display()),
                Access::Write
            ),
            Err(FileError::InvalidPath(_))
        ));
    }

    #[test]
    fn outside_relative_and_dotdot_are_refused() {
        let (d, r) = setup();
        let secret = s(&d.path().join("secret/key"));
        assert!(matches!(
            r.resolve(&secret, Access::Read),
            Err(FileError::PermissionDenied(_))
        ));
        assert!(matches!(
            r.resolve("a.txt", Access::Read),
            Err(FileError::InvalidPath(_))
        ));
        let climb = format!("{}/share/../secret/key", d.path().display());
        assert!(matches!(
            r.resolve(&climb, Access::Read),
            Err(FileError::InvalidPath(_))
        ));
        assert!(matches!(
            r.resolve_new(&format!("{}/share/../x", d.path().display()), Access::Write),
            Err(FileError::InvalidPath(_))
        ));
        assert!(matches!(
            r.resolve("", Access::Read),
            Err(FileError::InvalidPath(_))
        ));
        assert!(matches!(
            r.resolve("/etc/pass\0wd", Access::Read),
            Err(FileError::InvalidPath(_))
        ));
        // A new file next to the root, not inside it.
        assert!(matches!(
            r.resolve_new(&s(&d.path().join("x.bin")), Access::Write),
            Err(FileError::PermissionDenied(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape() {
        let (d, r) = setup();
        std::os::unix::fs::symlink(d.path().join("secret"), d.path().join("share/link")).unwrap();
        std::os::unix::fs::symlink(d.path().join("secret/key"), d.path().join("share/keylink"))
            .unwrap();
        assert!(matches!(
            r.resolve(&s(&d.path().join("share/link/key")), Access::Read),
            Err(FileError::PermissionDenied(_))
        ));
        assert!(matches!(
            r.resolve(&s(&d.path().join("share/keylink")), Access::Read),
            Err(FileError::PermissionDenied(_))
        ));
        // Writing "through" an existing link whose target is outside is refused too.
        assert!(matches!(
            r.resolve_new(&s(&d.path().join("share/keylink")), Access::Write),
            Err(FileError::PermissionDenied(_))
        ));
        // A parent that is a link to outside.
        assert!(matches!(
            r.resolve_new(&s(&d.path().join("share/link/new")), Access::Write),
            Err(FileError::PermissionDenied(_))
        ));
        // Deleting the link itself is allowed (it is inside the root).
        assert!(
            r.resolve_entry(&s(&d.path().join("share/keylink")), Access::Write)
                .is_ok()
        );
    }

    #[test]
    fn read_only_roots_refuse_writes() {
        let (d, r) = setup();
        assert!(matches!(
            r.resolve(&s(&d.path().join("ro")), Access::Write),
            Err(FileError::PermissionDenied(_))
        ));
        assert!(matches!(
            r.resolve_new(&s(&d.path().join("ro/new")), Access::Write),
            Err(FileError::PermissionDenied(_))
        ));
        assert!(r.resolve(&s(&d.path().join("ro")), Access::Read).is_ok());
    }

    #[test]
    fn missing_roots_are_skipped_and_empty_refuses_everything() {
        let r = FileRoots::new(vec![SharedRoot {
            path: "/definitely/not/here".into(),
            writable: true,
        }]);
        assert!(r.is_empty());
        assert!(r.resolve("/tmp", Access::Read).is_err());
    }
}
