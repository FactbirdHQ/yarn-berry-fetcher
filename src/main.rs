mod zip;

use std::collections::HashSet;
use std::path::PathBuf;

use oxhttp::model::{Body, Request, StatusCode};
use rayon::prelude::*;
use serde::Deserialize;
use sha2::{Digest, Sha512};

#[derive(Debug)]
struct CacheKey {
    version: usize,
    compression: Option<u32>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let lockfile_path = args.next().expect("yarn-zip <project_dir>");

    let lockfile_contents = std::fs::read_to_string(&lockfile_path).unwrap();
    let cache_version = {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct LockfileMetadata {
            cache_key: String,
        }
        #[derive(Deserialize)]
        struct Lockfile {
            __metadata: LockfileMetadata,
        }

        let lockfile: Lockfile = serde_yml::from_str(&lockfile_contents).unwrap();
        let mut iter = lockfile.__metadata.cache_key.split('c');
        let version_str = iter.next().unwrap();
        let compression_str = iter.next();
        CacheKey {
            version: version_str.parse().unwrap(),
            compression: compression_str.map(|c| c.parse().unwrap()),
        }
    };

    println!("{:?}", cache_version);

    let supported_version: usize = std::env!("YARN_ZIP_SUPPORTED_LOCKFILE_VERSION")
        .parse()
        .unwrap();
    assert_eq!(cache_version.version, supported_version);

    let lockfile = yarn_lock_parser::parse_str(&lockfile_contents).unwrap();

    let out_dir = std::env::var("out").unwrap_or("out".into());
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut hashes_found = HashSet::new();
    let packages = lockfile
        .entries
        .into_iter()
        .filter_map(|package| {
            let Some((name, version)) = package.resolved.split_once("@npm:") else {
                // Something other than npm

                if let Some((_, patch)) = package.resolved.split_once("@patch:") {
                    // These "builtin" patch dependencies (usually for PnP support)
                    // can be handled offline by yarn at a later stage
                    if patch.contains("#optional!builtin<compat/") {
                        return None;
                    }
                }

                println!("Unsupported source: {} (Hint: Git dependencies are not supported)", package.resolved);
                std::process::exit(1);
            };
            // We have an npm dependency

            let integrity = package.integrity.split("/").last().unwrap();
            match integrity.len() {
                128 => {}
                0 => {
                    println!("Missing hash for package {} {}, skipping...", name, version);
                    return None;
                }
                len => {
                    println!(
                        "Bad hash length {} for package {} {}, skipping...",
                        len, name, version
                    );
                    return None;
                }
            }

            if !hashes_found.insert(integrity) {
                println!(
                    "Duplicate integrity for package {} {}, skipping...",
                    name, version
                );
                return None;
            }

            Some((name, version, integrity))
        })
        .collect::<Vec<_>>();
    //println!("{:#?}", packages);

    rayon::ThreadPoolBuilder::new()
        .num_threads(20)
        .build_global()
        .unwrap();

    let expected_cnt = packages.len();
    let cnt = packages
        .into_par_iter()
        .panic_fuse()
        .map_init(oxhttp::Client::new, |client, (name, version, integrity)| {
            let unwind_result = std::panic::catch_unwind(|| {
                let (scope_prefix, name_rest) = name.split_once("/").unwrap_or(("", name));
                let scope_name = scope_prefix.strip_prefix("@");

                let url = format!(
                    "https://registry.npmjs.org/{}/-/{}-{}.tgz",
                    name, name_rest, version
                );
                //println!("Fetching {}", url);
                let response = client
                    .request(Request::builder().uri(&url).body(Body::empty()).unwrap())
                    .unwrap();

                if response.status() != StatusCode::OK {
                    println!("Failed to fetch {}: {}", url, response.status());
                    std::process::exit(1);
                }

                let ident_hash = hex::encode(Sha512::digest(format!(
                    "{}{}",
                    scope_name.unwrap_or_default(),
                    name_rest
                )));
                let locator_hash =
                    hex::encode(Sha512::digest(format!("{}npm:{}", ident_hash, version)));

                let dst = PathBuf::from(format!(
                    "{}/{}-npm-{}-{}-{}.zip",
                    out_dir,
                    name.replace("/", "-"),
                    version,
                    &locator_hash[..10],
                    &integrity[..10]
                ));
                zip::write_yarn_zip(
                    name,
                    dst.clone(),
                    response.into_body(),
                    cache_version.compression,
                );

                let out_hash = {
                    let mut hasher = Sha512::new();
                    let mut file = std::fs::File::open(&dst).unwrap();
                    std::io::copy(&mut file, &mut hasher).unwrap();
                    hex::encode(hasher.finalize())
                };
                if integrity == out_hash {
                    println!("Success:  {}", url);
                } else {
                    println!("Fail:     {}", url);
                    println!("  expected: {}", integrity);
                    println!("  got:      {}", out_hash);
                    std::process::exit(1);
                }
            });
            if unwind_result.is_err() {
                std::process::exit(1);
            }
        })
        .count();

    assert_eq!(expected_cnt, cnt);
}
