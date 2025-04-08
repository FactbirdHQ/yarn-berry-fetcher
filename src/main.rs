mod zip;

use std::collections::HashSet;
use std::path::PathBuf;

use rayon::prelude::*;
use chaste_types::PackageSource;
use oxhttp::model::{Body, Request, StatusCode};
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
    let lockfile = chaste_yarn::parse(&lockfile_path).unwrap();
    let mut hashes_done = HashSet::new();

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

        let lockfile_path = PathBuf::from(lockfile_path).join("yarn.lock");
        let lockfile_contents = std::fs::read(&lockfile_path).unwrap();
        let lockfile: Lockfile = serde_yml::from_slice(&lockfile_contents).unwrap();
        let mut iter = lockfile.__metadata.cache_key.split('c');
        let version_str = iter.next().unwrap();
        let compression_str = iter.next();
        CacheKey {
            version: version_str.parse().unwrap(),
            compression: compression_str.map(|c| c.parse().unwrap()),
        }
    };
    println!("{:?}", cache_version);

    let supported_version: usize = std::env!("YARN_ZIP_SUPPORTED_LOCKFILE_VERSION").parse().unwrap();
    assert_eq!(cache_version.version, supported_version);

    let out_dir = std::env::var("out").unwrap();
    std::fs::create_dir_all(&out_dir).unwrap();

    let packages = lockfile
        .packages()
        .into_iter()
        .filter_map(|package| {
            let version = package.version().unwrap();
            let name = package.name().unwrap();
            match package.source() {
                Some(PackageSource::Npm) => {
                    let (algo, expected_hash) = package.checksums().unwrap().integrity().to_hex();
                    if algo != ssri::Algorithm::Sha512 || expected_hash.len() != 128 {
                        println!(
                            "Bad hash length {} type {} for package {} {}, ignoring...",
                            expected_hash.len(),
                            algo,
                            name.name_rest(),
                            version
                        );
                        return None;
                    }
                    if !hashes_done.insert(expected_hash.clone()) {
                        println!(
                            "Duplicate package {} {}, ignoring...",
                            name.name_rest(),
                            version
                        );
                        return None;
                    }

                    Some((version, name, expected_hash))
                }
                other => {
                    println!(
                        "Unsupported package source {:?} for package {} {}, skipping...",
                        other,
                        name.name_rest(),
                        version
                    );
                    None
                }
            }
        })
        .collect::<Vec<_>>();

    rayon::ThreadPoolBuilder::new().num_threads(20).build_global().unwrap();
    packages
        .into_par_iter()
        .for_each_init(
            oxhttp::Client::new,
            |client, (version, name, expected_hash)| {
                let url = format!(
                    "https://registry.npmjs.org/{}/-/{}-{}.tgz",
                    name,
                    name.name_rest(),
                    version
                );
                //println!("Fetching {}", url);
                let response = client
                    .request(Request::builder().uri(&url).body(Body::empty()).unwrap())
                    .unwrap();

                if response.status() != StatusCode::OK {
                    println!("Failed to fetch {}: {}", url, response.status());
                    std::process::exit(1);
                }

                let ident_hash = hex::encode(Sha512::digest(format!("{}{}", name.scope_name().unwrap_or_default(), name.name_rest())));
                let locator_hash = hex::encode(Sha512::digest(format!("{}npm:{}", ident_hash, version)));

                let dst = PathBuf::from(format!(
                    "{}/{}-npm-{}-{}-{}.zip",
                    out_dir,
                    name.to_string().replace("/", "-"),
                    version,
                    &locator_hash[..10],
                    &expected_hash[..10]
                ));
                zip::write_yarn_zip(
                    &format!("{}{}", name.scope_prefix().unwrap_or_default(), name.name_rest()),
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
                if expected_hash == out_hash {
                    println!("Success:  {}", url);
                } else {
                    println!("Fail:     {}", url);
                    println!("  expected: {}", expected_hash);
                    println!("  got:      {}", out_hash);
                    std::process::exit(1);
                }
            }
        );
}
