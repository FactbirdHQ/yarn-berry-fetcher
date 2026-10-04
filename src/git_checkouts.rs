use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use anyhow::Context;

/// Returns the checkout of `commit` in `dir`, resolved through symlinks.
/// Only a commit hash is looked up, so a lockfile can't name a path outside `dir`.
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

/// Copies the checkout of `commit` in `dir` to `dest`, if `dir` holds one.
pub fn copy_checkout(dir: &Path, commit: &str, dest: &Path) -> anyhow::Result<bool> {
    let Some(src) = find(dir, commit)? else {
        return Ok(false);
    };
    std::fs::create_dir_all(dest.parent().expect("checkouts/<commit> has a parent"))
        .context("creating checkouts directory")?;
    copy_tree(&src, dest)?;
    Ok(true)
}

/// Copies `src` to `dest`, keeping symlinks and file modes.
fn copy_tree(src: &Path, dest: &Path) -> anyhow::Result<()> {
    let file_type = std::fs::symlink_metadata(src)
        .with_context(|| format!("reading {}", src.display()))?
        .file_type();
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
    } else {
        std::fs::copy(src, dest)
            .with_context(|| format!("copying {} to {}", src.display(), dest.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;

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
    fn copies_a_read_only_tree_with_its_links_and_executables() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::write(src.join("bin/tool"), "#!/bin/sh\n").unwrap();
        std::fs::write(src.join("README"), "hi").unwrap();
        symlink("README", src.join("README.link")).unwrap();
        std::fs::set_permissions(src.join("bin/tool"), Permissions::from_mode(0o555)).unwrap();
        std::fs::set_permissions(src.join("README"), Permissions::from_mode(0o444)).unwrap();
        std::fs::set_permissions(src.join("bin"), Permissions::from_mode(0o555)).unwrap();

        let dest = tmp.path().join("dest");
        copy_tree(&src, &dest).unwrap();

        let tool = std::fs::metadata(dest.join("bin/tool")).unwrap();
        assert_ne!(tool.permissions().mode() & 0o111, 0);
        assert_eq!(std::fs::read_to_string(dest.join("README")).unwrap(), "hi");
        assert_eq!(
            std::fs::read_link(dest.join("README.link")).unwrap(),
            PathBuf::from("README")
        );
        std::fs::remove_dir_all(&dest).unwrap();

        std::fs::set_permissions(src.join("bin"), Permissions::from_mode(0o755)).unwrap();
    }
}
