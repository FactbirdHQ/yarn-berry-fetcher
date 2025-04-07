mod zip;

use std::path::PathBuf;
use std::collections::HashSet;

use chaste_types::PackageSource;
use sha2::{Sha512, Digest};
use oxhttp::model::{Body, Request, StatusCode};

fn main() {
    let mut args = std::env::args().skip(1);
    let lockfile_path = args.next().unwrap();
    let lockfile = chaste_yarn::parse(lockfile_path).unwrap();
    let client = oxhttp::Client::new();
    let mut hashes_done = HashSet::new();
    let packages = lockfile.packages()
        .into_iter()
        .filter_map(|package| {
            let version = package.version().unwrap();
            let name = package.name().unwrap();
            let name_with_scope = format!("{}{}", name.scope_prefix().unwrap_or_default(), name.name_rest());
            match package.source() {
                Some(PackageSource::Npm) => {
                    let (algo, expected_hash) = package.checksums().unwrap().integrity().to_hex();
                    if algo != ssri::Algorithm::Sha512 || expected_hash.len() != 128 {
                        println!("Bad hash length {} type {} for package {} {}, ignoring...", expected_hash.len(), algo, name.name_rest(), version);
                        return None;
                    }
                    if !hashes_done.insert(expected_hash.clone()) {
                        println!("Duplicate package {} {}, ignoring...", name.name_rest(), version);
                        return None;
                    }

                    Some((version, name, name_with_scope, expected_hash))
                }
                other => {
                    println!("Unsupported package source {:?} for package {} {}, skipping...", other, name.name_rest(), version);
                    None
                }
            }
        })
        .collect::<Vec<_>>();

    for (version, name, name_with_scope, expected_hash) in packages {
        let url = format!("https://registry.npmjs.org/{}/-/{}-{}.tgz", name_with_scope, name.name_rest(), version);
        //println!("Fetching {}", url);
        let response = client.request(Request::builder().uri(&url).body(Body::empty()).unwrap()).unwrap();

        if response.status() != StatusCode::OK {
            println!("Failed to fetch {}: {}", url, response.status());
            continue;
        }

        let cache_key = "10";
        let hash_key = &expected_hash[..10]; // TODO figure out what this actually is

        let dst = PathBuf::from(format!("out/{}-npm-{}-{}-{}.zip", name.name_rest(), version, hash_key, cache_key));
        zip::write_yarn_zip(&name_with_scope, dst.clone(), response.into_body());

        let mut hasher = Sha512::new();
        let mut file = std::fs::File::open(dst).unwrap();
        std::io::copy(&mut file, &mut hasher).unwrap();
        let out_hash = hex::encode(hasher.finalize());
        if expected_hash == out_hash {
            println!("Success: {}", url);
        } else {
            println!("Fail:    {}", url);
            //println!("{}", expected_hash);
            //println!("{}", out_hash)
        }
    }

    /*
    let name = args.next().unwrap();
    let dst = args.next().unwrap();
    let dst = PathBuf::from(dst);
    zip::write_yarn_zip(&name, dst);
    */
}
