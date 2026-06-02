use anyhow::{Context, bail};
use std::collections::HashMap;
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};

use crate::{Cache, EntryExt, Lockfile, SourceWithIntegrity, SourceWithoutIntegrity};

use rayon::prelude::*;

const OUTDATED_MISSING_HASHES_ERR: &str = r#"
Error fetching berry dependencies:

The missingHashes passed to fetchYarnBerryDeps was either missing or outdated
and did not match the provided yarn.lock file.  Refer to the manual for more
information:

https://nixos.org/manual/nixpkgs/unstable/#javascript-yarnBerry-missing-hashes
"#;
const NIX_PREFETCH_GIT_ERR: &str = r#"
Error fetching berry dependencies:

Could not fetch git dependency:
"#;

const MAX_ATTEMPTS: usize = 5;

/// Fetches the given URL and writes contents to a temporary file. Exponential backoff.
pub fn fetch_to_tempfile(
    http_client: &reqwest::blocking::Client,
    url: &str,
) -> anyhow::Result<std::fs::File> {
    let mut file = tempfile::tempfile().context("opening tempfile")?;

    if let Err(err) = retry::retry_with_index(
        retry::delay::Exponential::from_millis(500)
            .take(MAX_ATTEMPTS)
            .map(retry::delay::jitter),
        |i| {
            // i is the number of the try, so starts from 1, not 0.
            if i != 1 {
                let prev = i - 1;
                eprintln!("Failed to fetch (on try {prev}/{MAX_ATTEMPTS}): {url}");
            }

            let mut response = http_client
                .get(url)
                .send()
                .context("sending http request")?;
            if !response.status().is_success() {
                bail!("non-successful HTTP response: {}", response.status())
            }

            std::io::copy(&mut response, &mut file)
                .context("reading from response into tempfile")?;

            file.seek(std::io::SeekFrom::Start(0))
                .context("seeking tempfile back to the beginning")?;
            Ok(())
        },
    ) {
        Err(err
            .error
            .context(format!("gave up fetching {url} after {} tries", err.tries)))?
    }

    Ok(file)
}

impl Cache {
    /// Fetches all sources specified in the lockfile with the specified http_client.
    /// Also takes a collection of missing hashes, which will supplement those
    /// in the lockfile.
    pub fn fetch_all(
        &self,
        lockfile: Lockfile,
        mut missing_hashes: HashMap<String, String>,
        http_client: &reqwest::blocking::Client,
    ) -> anyhow::Result<()> {
        let sources = lockfile
            .entries
            .into_iter()
            .filter(EntryExt::is_real_source)
            .map(|entry| {
                let source = match SourceWithIntegrity::try_from(&entry) {
                    Ok(source) => source,
                    Err(SourceWithoutIntegrity::Tgz { url }) => {
                        let Some(integrity) = missing_hashes.remove(entry.resolved) else {
                            anyhow::bail!("{OUTDATED_MISSING_HASHES_ERR}");
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
                Ok((entry, source))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        if !missing_hashes.is_empty() {
            anyhow::bail!("{OUTDATED_MISSING_HASHES_ERR}");
        }

        std::fs::create_dir_all(self.out_dir.join("cache")).context("creating cache directory")?;

        rayon::ThreadPoolBuilder::new()
            .num_threads(20)
            .build_global()
            .unwrap();

        sources
            .into_par_iter()
            .map(|(entry, source)| self.fetch_source(&http_client, entry, source))
            .collect::<anyhow::Result<Vec<()>>>()?;

        Ok(())
    }

    fn fetch_source(
        &self,
        client: &reqwest::blocking::Client,
        entry: yarn_lock_parser::Entry,
        source: SourceWithIntegrity,
    ) -> anyhow::Result<()> {
        match source {
            SourceWithIntegrity::Tgz { url, integrity } => {
                self.fetch_tgz_and_write_zip(client, entry, url, integrity)?
            }
            SourceWithIntegrity::Git { repo, commit, .. } => self.fetch_git(repo, commit)?,
        }
        Ok(())
    }

    fn fetch_git(&self, repo: String, commit: String) -> anyhow::Result<()> {
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
                eprintln!("Error spawning nix-prefetch-git: {e}");
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
            anyhow::bail!("nix-prefetch-git failed");
        }
        eprintln!("Success:  git+{repo}#commit={commit}");
        Ok(())
    }

    fn fetch_tgz_and_write_zip(
        &self,
        client: &reqwest::blocking::Client,
        entry: yarn_lock_parser::Entry,
        url: String,
        integrity: String,
    ) -> anyhow::Result<()> {
        let mut file = fetch_to_tempfile(client, &url)?;

        if let Err(out_hash) = self.write_zip_and_check(entry, &integrity, &mut file) {
            eprintln!("Fail:     {url}");
            eprintln!("  expected: {integrity}");
            eprintln!("  got:      {out_hash}");
            anyhow::bail!("got wrong hash");
        } else {
            eprintln!("Success:  {url}");
        }
        Ok(())
    }
}
