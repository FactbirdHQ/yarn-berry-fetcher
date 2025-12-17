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
                add_integrity(client, cache_key.compression, entry, source)
            });
            match unwind_result {
                Err(_) => std::process::exit(1),
                Ok(v) => v,
            }
        })
        .collect::<Vec<_>>();

    x.into_iter().collect::<BTreeMap<_, _>>()
}

fn add_integrity(
    client: &oxhttp::Client,
    compression: Option<u32>,
    entry: yarn_lock_parser::Entry,
    source: SourceWithoutIntegrity,
) -> (String, String) {
    let SourceWithoutIntegrity::Tgz { url } = source;

    let f = fetch_to_tempfile(client, &url);
    eprintln!("Success:  {}", url);

    let tmp_dir = tempfile::TempDir::new().unwrap();
    let dst = tmp_dir.path().join("out.zip");

    zip::write_yarn_zip(entry.name(), dst.clone(), f, compression);

    let out_hash = {
        let mut hasher = Sha512::new();
        let mut file = std::fs::File::open(&dst).unwrap();
        std::io::copy(&mut file, &mut hasher).unwrap();
        hex::encode(hasher.finalize())
    };

    tmp_dir.close().unwrap();

    (entry.resolved.to_string(), out_hash)
}
