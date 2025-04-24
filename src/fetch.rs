use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use crate::{Cache, EntryExt, Lockfile, SourceWithIntegrity, SourceWithoutIntegrity};

use oxhttp::model::{Body, Request, StatusCode};
use rayon::prelude::*;

const OUTDATED_MISSING_HASHES_ERR: &'static str = r#"
Error fetching berry dependencies:

The missingHashes passed to fetchYarnBerryDeps was either missing or outdated
and did not match the provided yarn.lock file.  Refer to the manual for more
information:

https://nixos.org/manual/nixpkgs/unstable/#javascript-yarnBerry-missing-hashes
"#;
const NIX_PREFETCH_GIT_ERR: &'static str = r#"
Error fetching berry dependencies:

Could not fetch git dependency:
"#;

impl Cache {
    pub fn fetch(&self, lockfile: Lockfile, missing_hashes_path: Option<&str>) {
        let mut missing_hashes: HashMap<String, String> = missing_hashes_path
            .map(|path| serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap())
            .unwrap_or_default();

        let sources = lockfile
            .entries
            .into_iter()
            .filter(EntryExt::is_real_source)
            .map(|entry| {
                let source = match SourceWithIntegrity::try_from(&entry) {
                    Ok(source) => source,
                    Err(missing_integrity) => {
                        let SourceWithoutIntegrity::Tgz { url } = missing_integrity;
                        let Some(integrity) = missing_hashes.remove(entry.resolved) else {
                            eprintln!("{}", OUTDATED_MISSING_HASHES_ERR);
                            std::process::exit(1);
                        };
                        assert_eq!(
                            integrity.len(),
                            128,
                            "Invalid length for sha512 integrity in missing-hashes.json {}",
                            entry.resolved
                        );
                        SourceWithIntegrity::Tgz { url, integrity }
                    }
                };
                (entry, source)
            })
            .collect::<Vec<_>>();

        if !missing_hashes.is_empty() {
            eprintln!("{}", OUTDATED_MISSING_HASHES_ERR);
            std::process::exit(1);
        }

        std::fs::create_dir_all(PathBuf::from(&self.out_dir).join("cache")).unwrap();

        rayon::ThreadPoolBuilder::new()
            .num_threads(20)
            .build_global()
            .unwrap();

        sources.into_par_iter().panic_fuse().for_each_init(
            oxhttp::Client::new,
            |client, (entry, source)| {
                let unwind_result =
                    std::panic::catch_unwind(|| self.fetch_source(client, entry, source));
                if unwind_result.is_err() {
                    std::process::exit(1);
                }
            },
        );
    }

    fn fetch_source(
        &self,
        client: &oxhttp::Client,
        entry: yarn_lock_parser::Entry,
        source: SourceWithIntegrity,
    ) {
        match source {
            SourceWithIntegrity::Tgz { url, integrity } => {
                self.fetch_tgz_and_write_zip(client, entry, url, integrity)
            }
            SourceWithIntegrity::Git { repo, commit, .. } => self.fetch_git(repo, commit),
        }
    }

    fn fetch_git(&self, repo: String, commit: String) {
        let output = match std::process::Command::new("nix-prefetch-git")
            .arg("--builder")
            .arg(&repo)
            .arg(&commit)
            .arg("--out")
            .arg(PathBuf::from(&self.out_dir).join("checkouts").join(&commit))
            .output()
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Error spawning nix-prefetch-git: {}", e);
                std::process::exit(1);
            }
        };
        if !output.status.success() {
            eprintln!(
                "{}\n  repo: {}\n  commit: {}\n  status: {:?}\n  stderr:",
                NIX_PREFETCH_GIT_ERR,
                repo,
                commit,
                output.status.code()
            );
            std::io::stderr().write_all(&output.stderr).unwrap();
            std::process::exit(1);
        }
        eprintln!("Success:  git+{}#commit={}", repo, commit);
    }

    fn fetch_tgz_and_write_zip(
        &self,
        client: &oxhttp::Client,
        entry: yarn_lock_parser::Entry,
        url: String,
        integrity: String,
    ) {
        let response = client
            .request(Request::builder().uri(&url).body(Body::empty()).unwrap())
            .unwrap();

        if response.status() != StatusCode::OK {
            eprintln!("Failed to fetch {}: {}", url, response.status());
            std::process::exit(1);
        }

        if let Err(out_hash) = self.write_zip_and_check(entry, &integrity, response.into_body()) {
            eprintln!("Fail:     {}", url);
            eprintln!("  expected: {}", integrity);
            eprintln!("  got:      {}", out_hash);
            std::process::exit(1);
        } else {
            eprintln!("Success:  {}", url);
        }
    }
}
