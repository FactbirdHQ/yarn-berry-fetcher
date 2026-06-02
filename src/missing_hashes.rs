use std::collections::BTreeMap;

use crate::{
    EntryExt, Lockfile, LockfileExt, SourceWithIntegrity, SourceWithoutIntegrity,
    fetch::fetch_to_tempfile, zip,
};

use anyhow::Context;
use rayon::prelude::*;
use sha2::{Digest, Sha512};

pub fn get_missing_hashes(
    lockfile: Lockfile,
    http_client: &reqwest::blocking::Client,
) -> anyhow::Result<BTreeMap<String, String>> {
    let (_cache_key, compression) = lockfile
        .cache_key_parsed()
        .expect("validated lockfile to have cache_key");

    let missing = lockfile
        .entries
        .into_iter()
        .filter(EntryExt::is_real_source)
        .filter_map(|entry| {
            SourceWithIntegrity::try_from(&entry)
                .err()
                .map(|err| (entry, err))
        })
        .collect::<Vec<_>>();

    rayon::ThreadPoolBuilder::new()
        .num_threads(20)
        .build_global()
        .unwrap();

    missing
        .into_par_iter()
        .map(
            |(entry, SourceWithoutIntegrity::Tgz { url })| -> anyhow::Result<(String, String)> {
                let f = fetch_to_tempfile(&http_client, &url)?;
                eprintln!("Success:  {url}");

                Ok((
                    entry.resolved.to_string(),
                    calc_integrity(f, compression, &entry.name).context("calculating integrity")?,
                ))
            },
        )
        .collect()
}

/// Calculates the integrity digest, which is the sha512 sum of a specially crafted zipfile,
/// lowerhex-encoded.
/// This must be written to a named temp file as we call into C here, and that wants a path.
fn calc_integrity(
    data: impl std::io::Read,
    compression: Option<u32>,
    entry_name: &str,
) -> anyhow::Result<String> {
    // write_yarn_zip expects to be the first one opening the file,
    // so we create a TempDir and pass it out.zip in that.
    let zip_dir = tempfile::TempDir::new().context("creating tempdir")?;
    let zip_path = zip_dir.path().join("out.zip");
    zip::write_yarn_zip(entry_name, &zip_path, data, compression);

    // hash the produced zip file
    let mut hasher = Sha512::new();
    let mut zip_file = std::fs::File::open(zip_path).context("opening written zipfile")?;
    std::io::copy(&mut zip_file, &mut hasher).context("hashing the written zipfile")?;

    zip_dir.close().context("cleaning up the tempdir")?;

    Ok(hex::encode(hasher.finalize()))
}
