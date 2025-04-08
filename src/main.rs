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
        Some("post") => {
            let lockfile_contents = std::fs::read_to_string("yarn.lock").unwrap();
            let (cache_version, lockfile) = parse_lockfile(&lockfile_contents);
            let cache_path = std::env::var("offlineCache").unwrap();
            make_cache_writable(&cache_path);
            let cache = Cache {
                out_dir: ".yarn/cache".into(),
                compression: cache_version.compression,
            };
            cache.repack_git_deps(lockfile);
        }
        _ => {
            eprintln!("USAGE: yarn-zip <generate|convert|post> [options]");
            std::process::exit(1);
        }
    }
}

fn make_cache_writable(cache_dir: &str) {
    assert!(
        std::process::Command::new("rm")
            .arg("-rf")
            .arg(".yarn/cache")
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("cp")
            .arg("-R")
            .arg("--reflink=auto")
            .arg(cache_dir)
            .arg(".yarn/cache")
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("chmod")
            .arg("-R")
            .arg("u+w")
            .arg(".yarn/cache")
            .status()
            .unwrap()
            .success()
    );
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

    eprintln!("{:?}", cache_version);

    let supported_version: usize = std::env!("YARN_ZIP_SUPPORTED_CACHE_VERSION")
        .parse()
        .unwrap();
    assert_eq!(cache_version.version, supported_version);

    let lockfile = yarn_lock_parser::parse_str(&lockfile_contents).unwrap();

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

                if let Some((name, url)) = package.resolved.split_once("@https:") {
                    let integrity = package.integrity.split("/").last().unwrap();
                    let version = package.version;
                    let Some((url, commit)) = url.split_once("#commit=") else {
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
                        name: name.into(),
                        version: version.into(),
                        integrity: integrity.into(),
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
        name: String,
        version: String,
        integrity: String,
        repo: String,
        commit: String,
    },
}

struct Cache {
    out_dir: String,
    compression: Option<u32>,
}

impl Cache {
    fn generate(&self, lockfile: Lockfile) {
        let sources = get_sources_from_lockfile(lockfile);

        std::fs::create_dir_all(&self.out_dir).unwrap();

        rayon::ThreadPoolBuilder::new()
            .num_threads(20)
            .build_global()
            .unwrap();

        sources.into_par_iter().panic_fuse().for_each_init(
            oxhttp::Client::new,
            |client, source| {
                let unwind_result = std::panic::catch_unwind(|| self.fetch_source(&client, source));
                if unwind_result.is_err() {
                    std::process::exit(1);
                }
            },
        );
    }

    fn fetch_source(&self, client: &oxhttp::Client, source: Source) {
        match source {
            Source::Npm {
                name,
                version,
                integrity,
            } => self.fetch_npm_and_write_zip(client, name, version, integrity),
            Source::Git { repo, commit, .. } => self.fetch_git(repo, commit),
        }
    }

    fn fetch_git(&self, repo: String, commit: String) {
        let output = std::process::Command::new("nix-prefetch-git")
            .arg("--builder")
            .arg(&repo)
            .arg(&commit)
            .arg("--out")
            .arg(PathBuf::from(&self.out_dir).join(&commit))
            .output()
            .unwrap();
        assert!(output.status.success());
        eprintln!("Success:  git+{}#commit={}", repo, commit);
    }

    fn fetch_npm_and_write_zip(
        &self,
        client: &oxhttp::Client,
        name: String,
        version: String,
        integrity: String,
    ) {
        let (_, name_rest) = name.split_once("/").unwrap_or(("", &name));

        let url = format!(
            "https://registry.npmjs.org/{}/-/{}-{}.tgz",
            name, name_rest, version
        );
        let response = client
            .request(Request::builder().uri(&url).body(Body::empty()).unwrap())
            .unwrap();

        if response.status() != StatusCode::OK {
            eprintln!("Failed to fetch {}: {}", url, response.status());
            std::process::exit(1);
        }

        if let Err(out_hash) =
            self.write_zip_and_check(&name, &version, &integrity, response.into_body())
        {
            eprintln!("Fail:     {}", url);
            eprintln!("  expected: {}", integrity);
            eprintln!("  got:      {}", out_hash);
            std::process::exit(1);
        } else {
            eprintln!("Success:  {}", url);
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

    fn repack_git_deps(&self, lockfile: Lockfile) {
        let sources = get_sources_from_lockfile(lockfile);
        for source in sources {
            let Source::Git {
                name,
                version,
                integrity,
                commit,
                ..
            } = source
            else {
                continue;
            };

            let mut tar_proc = std::process::Command::new("tar")
                .arg("--sort=name")
                .arg("-C")
                .arg(&format!(".yarn/cache/{}", commit))
                .arg(".")
                .spawn()
                .unwrap();

            self.write_zip_and_check(&name, &version, &integrity, tar_proc.stdout.take().unwrap())
                .unwrap();

            let tar_output = tar_proc.wait_with_output().unwrap();
            assert!(tar_output.status.success());
        }
    }
}
