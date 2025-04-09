use std::path::PathBuf;

use crate::{Cache, Lockfile, Source, get_sources_from_lockfile};

use oxhttp::model::{Body, Request, StatusCode};
use rayon::prelude::*;

impl Cache {
    pub fn fetch(&self, lockfile: Lockfile) {
        let sources = get_sources_from_lockfile(lockfile);

        std::fs::create_dir_all(&PathBuf::from(&self.out_dir).join("cache")).unwrap();

        rayon::ThreadPoolBuilder::new()
            .num_threads(20)
            .build_global()
            .unwrap();

        sources.into_par_iter().panic_fuse().for_each_init(
            oxhttp::Client::new,
            |client, source| {
                let unwind_result = std::panic::catch_unwind(|| self.fetch_source(client, source));
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

        if let Err(out_hash) = self.write_zip_and_check(
            &name,
            &format!("npm-{}", version),
            &format!("npm:{}", version),
            &integrity,
            response.into_body(),
        ) {
            eprintln!("Fail:     {}", url);
            eprintln!("  expected: {}", integrity);
            eprintln!("  got:      {}", out_hash);
            std::process::exit(1);
        } else {
            eprintln!("Success:  {}", url);
        }
    }
}
