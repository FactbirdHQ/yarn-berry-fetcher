//! Git dependencies the caller has already fetched.
//!
//! A git dependency is otherwise cloned with `nix-prefetch-git`, which runs with
//! an empty `HOME` and no system gitconfig, so inside a fixed-output derivation
//! it has no credential and no URL rewrite to reach a private repository with.
//! A caller that can fetch the commit some other way, such as `builtins.fetchGit`
//! while Nix evaluates, passes the tree in here and the clone is skipped.
//!
//! The tree is the commit's checkout without `.git`, which is what
//! `nix-prefetch-git --builder` writes, so the cache hashes the same whichever
//! way it was fetched.

use std::collections::HashMap;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use anyhow::Context;

/// Commit to the directory holding its checkout.
pub type GitCheckouts = HashMap<String, PathBuf>;

/// Parses `<commit>=<path>` entries. Empty entries are skipped, so an empty or
/// whitespace-only environment variable means no checkouts.
pub fn parse<'a>(entries: impl IntoIterator<Item = &'a str>) -> anyhow::Result<GitCheckouts> {
    entries
        .into_iter()
        .flat_map(str::split_whitespace)
        .map(|entry| {
            let (commit, path) = entry
                .split_once('=')
                .with_context(|| format!("git checkout {entry:?} is not <commit>=<path>"))?;
            anyhow::ensure!(
                !commit.is_empty() && !path.is_empty(),
                "git checkout {entry:?} is not <commit>=<path>"
            );
            Ok((commit.to_owned(), PathBuf::from(path)))
        })
        .collect()
}

/// Copies `src` to `dest`, keeping symlinks as symlinks and file modes as they
/// are, then makes everything owner-writable. A store path is read-only, and the
/// fixed-output builder has to be able to remove its own output on failure.
pub fn copy_tree(src: &Path, dest: &Path) -> anyhow::Result<()> {
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
    fn parses_pairs_and_skips_empty_entries() {
        let checkouts = parse(["", "  abc=/nix/store/x-source  def=/nix/store/y-source "]).unwrap();
        assert_eq!(checkouts.len(), 2);
        assert_eq!(checkouts["abc"], PathBuf::from("/nix/store/x-source"));
        assert_eq!(checkouts["def"], PathBuf::from("/nix/store/y-source"));
        assert!(parse([""]).unwrap().is_empty());
    }

    #[test]
    fn rejects_entries_without_a_commit_or_path() {
        assert!(parse(["abc"]).is_err());
        assert!(parse(["=/nix/store/x"]).is_err());
        assert!(parse(["abc="]).is_err());
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
