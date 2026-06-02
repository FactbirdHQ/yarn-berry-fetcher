use std::path::PathBuf;

use sha2::{Digest, Sha512};
use yarn_lock_parser::Lockfile;

use crate::{EntryExt, LockfileExt, missing_hashes::write_zip_and_calc_integrity};

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

    async fn write_zip_and_check(
        &self,
        entry: &yarn_lock_parser::Entry<'_>,
        integrity: &str,
        reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    ) -> Result<(), String> {
        let path = self
            .out_dir
            .join("cache")
            .join(self.zip_name(entry, integrity));

        let actual_integrity =
            write_zip_and_calc_integrity(reader, path, self.cache_key_compression(), entry.name())
                .await
                .expect("writing zip and calculating integrity");

        if integrity == actual_integrity {
            Ok(())
        } else {
            Err(actual_integrity)
        }
    }
}
