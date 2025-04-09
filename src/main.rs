mod fetch;
mod zip;

use std::collections::HashSet;
use std::path::PathBuf;

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
        Some("fetch") => {
            let lockfile_path = args.next().expect("yarn-zip fetch <yarn.lock>");
            let lockfile_contents = std::fs::read_to_string(&lockfile_path).unwrap();
            let (cache_version, lockfile) = parse_lockfile(&lockfile_contents);
            let out_dir = std::env::var("out").unwrap_or("out".into());
            let cache = Cache {
                out_dir: out_dir.clone(),
                compression: cache_version.compression,
            };
            std::fs::write(PathBuf::from(out_dir).join("yarn.lock"), &lockfile_contents).unwrap();
            cache.fetch(lockfile)
        }
        Some("convert") => {
            let help = "yarn-zip convert <full package name> <package version> <expected sha512> <npm.tgz>";
            let cache = Cache {
                out_dir: ".".into(),
                compression: None,
            };
            let package_name = args.next().expect(help);
            let version = args.next().expect(help);
            let expected_hash = args.next().expect(help);
            match cache.write_zip_and_check(
                &package_name,
                "npm",
                &format!("npm:{}", version),
                &expected_hash,
                std::fs::File::open(args.next().expect(help)).unwrap(),
            ) {
                Ok(out) => eprintln!("Wrote {:?}", out),
                Err(_) => eprintln!("Hash mismatch"),
            }
        }
        _ => {
            eprintln!("USAGE: yarn-zip <fetch|convert> [options]");
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

        let lockfile: Lockfile = serde_yml::from_str(lockfile_contents)
            .expect("yarn.lock is not valid YAML. Are you trying to pass a yarn v1 lockfile?");
        let mut iter = lockfile.__metadata.cache_key.split('c');
        let version_str = iter.next().unwrap();
        let compression_str = iter.next();
        CacheKey {
            version: version_str.parse().unwrap(),
            compression: compression_str.map(|c| c.parse().unwrap()),
        }
    };

    eprintln!("{:?}", cache_version);

    let supported_version: usize = std::env!("YARN_ZIP_SUPPORTED_CACHE_VERSION")
        .parse()
        .unwrap();
    assert_eq!(cache_version.version, supported_version);

    let lockfile = yarn_lock_parser::parse_str(lockfile_contents).unwrap();

    (cache_version, lockfile)
}

fn get_sources_from_lockfile(lockfile: Lockfile) -> Vec<Source> {
    let mut hashes_found = HashSet::new();
    lockfile
        .entries
        .into_iter()
        .filter_map(|package| {
            let Some((name, version)) = package.resolved.split_once("@npm:") else {
                // Something other than npm

                if package.resolved.contains("@workspace:")
                    || package.resolved.contains("@patch:")
                    || package.resolved.contains("@link:")
                {
                    // these dependencies can be handled offline by yarn at a later stage,
                    // provided that all the sources have been fetched
                    return None;
                }

                if let Some((_name, url_and_commit)) = package.resolved.split_once("@https:") {
                    let Some((url, commit)) = url_and_commit.split_once("#commit=") else {
                        eprintln!("Git dependency without commit hash: {}", package.resolved);
                        std::process::exit(1);
                    };
                    if commit.len() != 40 {
                        eprintln!(
                            "Git dependency with bad commit hash length {}: {}...",
                            commit.len(),
                            package.resolved
                        );
                        std::process::exit(1);
                    }
                    if !hashes_found.insert(commit) {
                        eprintln!(
                            "Duplicate commit hash for git dependency {}, skipping...",
                            package.resolved
                        );
                        return None;
                    }

                    let repo = format!("https:{}", url);
                    return Some(Source::Git {
                        repo,
                        commit: commit.into(),
                    });
                }

                eprintln!("Unsupported source: {}", package.resolved);
                std::process::exit(1);
            };
            // We have an npm dependency
            let integrity = package.integrity.split("/").last().unwrap();
            match integrity.len() {
                128 => {}
                0 => {
                    eprintln!("Missing hash for package {} {}, skipping...", name, version);
                    return None;
                }
                len => {
                    eprintln!(
                        "Bad hash length {} for package {} {}, skipping...",
                        len, name, version
                    );
                    return None;
                }
            }

            if !hashes_found.insert(integrity) {
                eprintln!(
                    "Duplicate integrity for package {} {}, skipping...",
                    name, version
                );
                return None;
            }

            Some(Source::Npm {
                name: name.into(),
                version: version.into(),
                integrity: integrity.into(),
            })
        })
        .collect()
}

enum Source {
    Npm {
        name: String,
        version: String,
        integrity: String,
    },
    Git {
        repo: String,
        commit: String,
    },
}

struct Cache {
    out_dir: String,
    compression: Option<u32>,
}

impl Cache {
    fn write_zip_and_check(
        &self,
        package_name: &str,
        protocol: &str,
        reference: &str,
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
        let locator_hash = hex::encode(Sha512::digest(format!("{}{}", ident_hash, reference)));

        let dst = PathBuf::from(format!(
            "{}/cache/{}-{}-{}-{}.zip",
            self.out_dir,
            package_name.replace("/", "-"),
            protocol,
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
