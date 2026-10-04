//! Git dependencies the caller has already fetched.
//!
//! A git dependency is otherwise cloned with `nix-prefetch-git`, which runs with
//! an empty `HOME` and no system gitconfig, so inside a fixed-output derivation
//! it has no credential and no URL rewrite to reach a private repository with.
//!
//! A checkout is the commit's tree without `.git`, which is what
//! `nix-prefetch-git --builder` writes, so the cache hashes the same whichever
//! way it was fetched. The exception is a repository whose `.gitattributes`
//! converts files on checkout with `eol`, `text` or `ident`. The git CLI behind
//! `nix-prefetch-git` applies those and `builtins.fetchGit` doesn't, so such a
//! checkout fails the fixed-output hash.

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use anyhow::Context;

/// Returns the checkout of `commit` in `dir`, or `None` when `dir` doesn't hold
/// that commit. The path is resolved through symlinks, since a fixed-output
/// derivation's output can't reference another store path and has to hold the
/// tree itself. Only a hexadecimal commit is looked up, so a lockfile can't name
/// a path outside `dir`.
fn find(dir: &Path, commit: &str) -> anyhow::Result<Option<PathBuf>> {
    if commit.is_empty() || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(None);
    }

    let entry = dir.join(commit);
    match std::fs::canonicalize(&entry) {
        Ok(checkout) => {
            anyhow::ensure!(checkout.is_dir(), "{} is not a directory", entry.display());
            Ok(Some(checkout))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("resolving {}", entry.display())),
    }
}

/// Copies the checkout of `commit` in `dir` to `dest`, and returns whether `dir`
/// held that commit.
pub fn copy_checkout(dir: &Path, commit: &str, dest: &Path) -> anyhow::Result<bool> {
    let Some(src) = find(dir, commit)? else {
        return Ok(false);
    };
    std::fs::create_dir_all(dest.parent().expect("checkouts/<commit> has a parent"))
        .context("creating checkouts directory")?;
    copy_tree(&src, dest)?;
    Ok(true)
}

/// Copies `src` to `dest`, keeping symlinks as symlinks and file modes as they
/// are, then makes everything owner-writable. A store path is read-only, and the
/// fixed-output builder has to be able to remove its own output on failure.
fn copy_tree(src: &Path, dest: &Path) -> anyhow::Result<()> {
    let metadata =
        std::fs::symlink_metadata(src).with_context(|| format!("reading {}", src.display()))?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        let target =
            std::fs::read_link(src).with_context(|| format!("reading link {}", src.display()))?;
        symlink(&target, dest).with_context(|| format!("creating link {}", dest.display()))?;
    } else if file_type.is_dir() {
        std::fs::create_dir(dest).with_context(|| format!("creating {}", dest.display()))?;
        for child in std::fs::read_dir(src).with_context(|| format!("listing {}", src.display()))? {
            let child = child?;
            copy_tree(&child.path(), &dest.join(child.file_name()))?;
        }
        add_owner_write(dest, metadata.permissions().mode() | 0o700)?;
    } else {
        std::fs::copy(src, dest)
            .with_context(|| format!("copying {} to {}", src.display(), dest.display()))?;
        add_owner_write(dest, metadata.permissions().mode() | 0o200)?;
    }
    Ok(())
}

fn add_owner_write(path: &Path, mode: u32) -> anyhow::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("setting the mode of {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_linked_checkout_by_its_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path().join("source");
        std::fs::create_dir(&checkout).unwrap();
        let dir = tmp.path().join("checkouts");
        std::fs::create_dir(&dir).unwrap();
        symlink(&checkout, dir.join("abc123")).unwrap();

        assert_eq!(
            find(&dir, "abc123").unwrap(),
            Some(checkout.canonicalize().unwrap())
        );
        assert_eq!(find(&dir, "def456").unwrap(), None);
    }

    #[test]
    fn rejects_a_checkout_that_is_not_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("checkouts");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(tmp.path().join("file"), "").unwrap();
        symlink(tmp.path().join("file"), dir.join("abc123")).unwrap();

        let err = find(&dir, "abc123").unwrap_err();
        assert!(
            err.to_string().ends_with("abc123 is not a directory"),
            "{err}"
        );
    }

    #[test]
    fn looks_up_only_hexadecimal_commits() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("checkouts")).unwrap();

        assert_eq!(
            find(&tmp.path().join("checkouts"), "../checkouts").unwrap(),
            None
        );
        assert_eq!(find(&tmp.path().join("checkouts"), "").unwrap(), None);
    }

    #[test]
    fn copies_a_read_only_tree_with_its_modes_and_links() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::write(src.join("bin/tool"), "#!/bin/sh\n").unwrap();
        std::fs::write(src.join("README"), "hi").unwrap();
        symlink("README", src.join("README.link")).unwrap();
        std::fs::set_permissions(src.join("bin/tool"), PermissionsExt::from_mode(0o555)).unwrap();
        std::fs::set_permissions(src.join("README"), PermissionsExt::from_mode(0o444)).unwrap();
        std::fs::set_permissions(src.join("bin"), PermissionsExt::from_mode(0o555)).unwrap();

        let dest = tmp.path().join("dest");
        copy_tree(&src, &dest).unwrap();

        let mode = |path: &str| {
            std::fs::symlink_metadata(dest.join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("bin/tool"), 0o755);
        assert_eq!(mode("README"), 0o644);
        assert_eq!(mode("bin"), 0o755);
        assert_eq!(std::fs::read_to_string(dest.join("README")).unwrap(), "hi");
        assert_eq!(
            std::fs::read_link(dest.join("README.link")).unwrap(),
            PathBuf::from("README")
        );

        // The source is left read-only for the store's sake; restore it so the
        // tempdir can be removed.
        std::fs::set_permissions(src.join("bin"), PermissionsExt::from_mode(0o755)).unwrap();
    }
}
