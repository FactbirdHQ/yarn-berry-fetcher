use std::path::PathBuf;

use crate::{Cache, EntryExt, Lockfile, SourceWithIntegrity};

use oxhttp::model::{Body, Request, StatusCode};
use rayon::prelude::*;

impl Cache {
    pub fn fetch(&self, lockfile: Lockfile) {
        let sources = lockfile
            .entries
            .into_iter()
            .filter(EntryExt::is_real_source)
            .map(|entry| {
                let Ok(source) = SourceWithIntegrity::try_from(&entry) else {
                    panic!("Missing integrity {}", entry.resolved);
                };
                (entry, source)
            })
            .collect::<Vec<_>>();

        std::fs::create_dir_all(&PathBuf::from(&self.out_dir).join("cache")).unwrap();

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
        let output = std::process::Command::new("nix-prefetch-git")
            .arg("--builder")
            .arg(&repo)
            .arg(&commit)
            .arg("--out")
            .arg(PathBuf::from(&self.out_dir).join("checkouts").join(&commit))
            .output()
            .unwrap();
        assert!(output.status.success());
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
