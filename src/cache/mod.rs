use std::path::PathBuf;

use sha2::{Digest, Sha512};
use yarn_lock_parser::Lockfile;

use crate::{EntryExt, LockfileExt, zip};

mod fetch;

pub struct Cache<'l> {
    out_dir: PathBuf,
    lockfile: Lockfile<'l>,
}

impl<'l> Cache<'l> {
    pub fn open(out_dir: impl Into<PathBuf>, lockfile: Lockfile<'l>) -> Self {
        Self {
            out_dir: out_dir.into(),
            lockfile,
        }
    }
}

impl Cache<'_> {
    fn cache_key_compression(&self) -> Option<u32> {
        self.lockfile
            .cache_key_parsed()
            .expect("validated lockfile to have cache_key")
            .1
    }

    /// Returns the filename a given entry should have.
    fn zip_name(&self, entry: &yarn_lock_parser::Entry, integrity: &str) -> String {
        let ident_hash = hex::encode(Sha512::digest(format!(
            "{}{}",
            entry.scope_name().unwrap_or_default(),
            entry.name_rest(),
        )));
        let locator_hash = hex::encode(Sha512::digest(format!(
            "{}{}",
            ident_hash,
            entry.resolution()
        )));

        format!(
            "{}-{}-{}.zip",
            entry.slug(),
            &locator_hash[..10],
            if !entry.is_content_addressed() {
                self.lockfile
                    .cache_key
                    .expect("validated lockfile to have cache_key")
            } else {
                &integrity[..10]
            },
        )
    }

    fn write_zip_and_check(
        &self,
        entry: &yarn_lock_parser::Entry,
        integrity: &str,
        source: impl std::io::Read,
    ) -> Result<PathBuf, String> {
        let dst = self
            .out_dir
            .join("cache")
            .join(self.zip_name(&entry, integrity));

        zip::write_yarn_zip(entry.name(), &dst, source, self.cache_key_compression());

        let out_hash = {
            let mut hasher = Sha512::new();
            let mut file = std::fs::File::open(&dst).unwrap();
            std::io::copy(&mut file, &mut hasher).unwrap();
            hex::encode(hasher.finalize())
        };

        if integrity == out_hash {
            Ok(dst)
        } else {
            Err(out_hash)
        }
    }
}
