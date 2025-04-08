mod zip;

use std::collections::HashSet;
use std::path::PathBuf;

use oxhttp::model::{Body, Request, StatusCode};
use rayon::prelude::*;
use serde::Deserialize;
use sha2::{Digest, Sha512};
use yarn_lock_parser::Lockfile;

#[derive(Debug)]
struct CacheKey {
    version: usize,
    compression: Option<u32>,
}

fn main() {
    let mut args = std::env::args().skip(1);

    match args.next().as_deref() {
        Some("generate") => {
            let lockfile_path = args.next().expect("yarn-zip generate <yarn.lock>");
            let lockfile_contents = std::fs::read_to_string(&lockfile_path).unwrap();
            let (cache_version, lockfile) = parse_lockfile(&lockfile_contents);
            let out_dir = std::env::var("out").unwrap_or("out".into());
            let cache = Cache {
                out_dir,
                compression: cache_version.compression,
            };
            cache.generate(lockfile)
        }
        Some("convert") => {
            let help = "yarn-zip generate <full package name> <package version> <expected sha512> <npm.tgz>";
            let cache = Cache {
                out_dir: ".".into(),
                compression: None,
            };
            match cache.write_zip_and_check(
                &args.next().expect(help),
                &args.next().expect(help),
                &args.next().expect(help),
                std::fs::File::open(args.next().expect(help)).unwrap(),
            ) {
                Ok(out) => eprintln!("Wrote {:?}", out),
                Err(_) => eprintln!("Hash mismatch"),
            }
        }
        Some("post") => {}
        _ => {
            eprintln!("USAGE: yarn-zip <generate|convert|post> [options]");
            std::process::exit(1);
        }
    }
}

fn parse_lockfile(lockfile_contents: &str) -> (CacheKey, Lockfile<'_>) {
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

        let lockfile: Lockfile = serde_yml::from_str(&lockfile_contents)
            .expect("yarn.lock is not valid YAML. Are you trying to pass a yarn v1 lockfile?");
        let mut iter = lockfile.__metadata.cache_key.split('c');
        let version_str = iter.next().unwrap();
        let compression_str = iter.next();
        CacheKey {
            version: version_str.parse().unwrap(),
            compression: compression_str.map(|c| c.parse().unwrap()),
        }
    };

    println!("{:?}", cache_version);

    let supported_version: usize = std::env!("YARN_ZIP_SUPPORTED_CACHE_VERSION")
        .parse()
        .unwrap();
    assert_eq!(cache_version.version, supported_version);

    let lockfile = yarn_lock_parser::parse_str(&lockfile_contents).unwrap();

    (cache_version, lockfile)
}

struct Cache {
    out_dir: String,
    compression: Option<u32>,
}

impl Cache {
    fn generate(&self, lockfile: Lockfile) {
        std::fs::create_dir_all(&self.out_dir).unwrap();

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
                        if patch.contains("builtin<compat/") {
                            return None;
                        }
                    }
                    if package.resolved.contains("@workspace:") {
                        return None;
                    }

                    println!(
                        "Unsupported source: {} (Hint: Git dependencies are not supported)",
                        package.resolved
                    );
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

        rayon::ThreadPoolBuilder::new()
            .num_threads(20)
            .build_global()
            .unwrap();

        packages.into_par_iter().panic_fuse().for_each_init(
            oxhttp::Client::new,
            |client, (name, version, integrity)| {
                let unwind_result = std::panic::catch_unwind(|| {
                    self.fetch_and_write_zip(&client, name, version, integrity)
                });
                if unwind_result.is_err() {
                    std::process::exit(1);
                }
            },
        );
    }

    fn fetch_and_write_zip(
        &self,
        client: &oxhttp::Client,
        name: &str,
        version: &str,
        integrity: &str,
    ) {
        let (_, name_rest) = name.split_once("/").unwrap_or(("", name));

        let url = format!(
            "https://registry.npmjs.org/{}/-/{}-{}.tgz",
            name, name_rest, version
        );
        let response = client
            .request(Request::builder().uri(&url).body(Body::empty()).unwrap())
            .unwrap();

        if response.status() != StatusCode::OK {
            println!("Failed to fetch {}: {}", url, response.status());
            std::process::exit(1);
        }

        if let Err(out_hash) =
            self.write_zip_and_check(name, version, integrity, response.into_body())
        {
            println!("Fail:     {}", url);
            println!("  expected: {}", integrity);
            println!("  got:      {}", out_hash);
            std::process::exit(1);
        } else {
            println!("Success:  {}", url);
        }
    }

    fn write_zip_and_check(
        &self,
        package_name: &str,
        version: &str,
        integrity: &str,
        source: impl std::io::Read,
    ) -> Result<PathBuf, String> {
        let (scope_prefix, name_rest) = package_name.split_once("/").unwrap_or(("", package_name));
        let scope_name = scope_prefix.strip_prefix("@");

        let ident_hash = hex::encode(Sha512::digest(format!(
            "{}{}",
            scope_name.unwrap_or_default(),
            name_rest
        )));
        let locator_hash = hex::encode(Sha512::digest(format!("{}npm:{}", ident_hash, version)));

        let dst = PathBuf::from(format!(
            "{}/{}-npm-{}-{}-{}.zip",
            self.out_dir,
            package_name.replace("/", "-"),
            version,
            &locator_hash[..10],
            &integrity[..10]
        ));
        zip::write_yarn_zip(package_name, dst.clone(), source, self.compression);

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
