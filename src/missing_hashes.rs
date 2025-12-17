use std::collections::BTreeMap;

use crate::{
    CacheKey, EntryExt, Lockfile, SourceWithIntegrity, SourceWithoutIntegrity,
    fetch::fetch_to_tempfile, zip,
};

use rayon::prelude::*;
use sha2::{Digest, Sha512};

pub fn get_missing_hashes(lockfile: Lockfile, cache_key: CacheKey) -> BTreeMap<String, String> {
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

    let x = missing
        .into_par_iter()
        .panic_fuse()
        .map_init(oxhttp::Client::new, |client, (entry, source)| {
            let unwind_result = std::panic::catch_unwind(|| {
                let SourceWithoutIntegrity::Tgz { url } = source;
                let f = fetch_to_tempfile(client, &url);
                eprintln!("Success:  {url}");

                (
                    entry.resolved.to_string(),
                    calc_integrity(f, cache_key.compression, &entry.name),
                )
            });
            match unwind_result {
                Err(_) => std::process::exit(1),
                Ok(v) => v,
            }
        })
        .collect::<Vec<_>>();

    x.into_iter().collect::<BTreeMap<_, _>>()
}

/// Calculates the integrity digest, which is the sha512 sum of a specially crafted zipfile,
/// lowerhex-encoded.
fn calc_integrity(f: impl std::io::Read, compression: Option<u32>, entry_name: &str) -> String {
    let mut zip_out = tempfile::NamedTempFile::new().expect("tempfile created");
    zip::write_yarn_zip(entry_name, &zip_out.path(), f, compression);

    // hash the produced zip file
    let mut hasher = Sha512::new();
    std::io::copy(&mut zip_out, &mut hasher).unwrap();
    hex::encode(hasher.finalize())
}
