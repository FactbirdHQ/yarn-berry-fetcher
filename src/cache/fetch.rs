use crate::fetch::fetch_to_tempfile;
use crate::{EntryExt, SourceWithIntegrity, SourceWithoutIntegrity};

use super::Cache;
use anyhow::Context;
use futures::{StreamExt, TryStreamExt};
use std::collections::HashMap;
use tokio::io::AsyncWriteExt;

const NIX_PREFETCH_GIT_ERR: &str = r#"
Error fetching berry dependencies:

Could not fetch git dependency:
"#;

const OUTDATED_MISSING_HASHES_ERR: &str = r#"
Error fetching berry dependencies:

The missingHashes passed to fetchYarnBerryDeps was either missing or outdated
and did not match the provided yarn.lock file.  Refer to the manual for more
information:

https://nixos.org/manual/nixpkgs/unstable/#javascript-yarnBerry-missing-hashes
"#;

impl Cache<'_> {
    /// Fetches all sources specified in the lockfile with the specified http_client.
    /// Also takes a collection of missing hashes, which will supplement those
    /// in the lockfile.
    pub async fn fetch_all(
        &self,
        mut missing_hashes: HashMap<String, String>,
        http_client: &reqwest::Client,
        fetch_concurrency: usize,
    ) -> anyhow::Result<()> {
        let sources = self
            .lockfile
            .entries
            .iter()
            .filter(|x| x.is_real_source())
            .map(|entry| {
                let source = match SourceWithIntegrity::try_from(entry) {
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

        tokio::fs::create_dir_all(self.out_dir.join("cache"))
            .await
            .context("creating cache directory")?;

        futures::stream::iter(sources)
            .map(|(entry, source)| async move {
                self.fetch_source(http_client, entry, source).await
            })
            .buffer_unordered(fetch_concurrency)
            .try_collect::<Vec<()>>()
            .await?;

        Ok(())
    }

    async fn fetch_source(
        &self,
        client: &reqwest::Client,
        entry: &yarn_lock_parser::Entry<'_>,
        source: SourceWithIntegrity,
    ) -> anyhow::Result<()> {
        match source {
            SourceWithIntegrity::Tgz { url, integrity } => {
                self.fetch_tgz_and_write_zip(client, entry, url, integrity)
                    .await?
            }
            SourceWithIntegrity::Git { repo, commit, .. } => self.fetch_git(repo, commit).await?,
        }
        Ok(())
    }

    async fn fetch_git(&self, repo: String, commit: String) -> anyhow::Result<()> {
        let output = async_process::Command::new("nix-prefetch-git")
            .arg("--builder")
            .arg(&repo)
            .arg(&commit)
            .arg("--out")
            .arg(self.out_dir.join("checkouts").join(&commit))
            .output()
            .await
            .context("spawning nix-prefetch-git")?;

        if !output.status.success() {
            eprintln!(
                "{}\n  repo: {}\n  commit: {}\n  status: {:?}\n  stderr:",
                NIX_PREFETCH_GIT_ERR,
                repo,
                commit,
                output.status.code()
            );
            tokio::io::stderr().write_all(&output.stderr).await.unwrap();
            anyhow::bail!("nix-prefetch-git failed");
        }
        eprintln!("Success:  git+{repo}#commit={commit}");
        Ok(())
    }

    async fn fetch_tgz_and_write_zip(
        &self,
        client: &reqwest::Client,
        entry: &yarn_lock_parser::Entry<'_>,
        url: String,
        integrity: String,
    ) -> anyhow::Result<()> {
        let file = fetch_to_tempfile(client, &url).await?;

        if let Err(out_hash) = self.write_zip_and_check(entry, &integrity, file).await {
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
