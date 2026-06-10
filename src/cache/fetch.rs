use crate::EntryExt;
use crate::fetch::fetch_to_tempfile;

use super::Cache;
use anyhow::{Context, bail};
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
                // For this entry, construct a [SourceWithIntegrity], while looking up from `missing_hashes`.
                let source = entry_to_source(entry, &mut missing_hashes)?;

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

        let path = self
            .out_dir
            .join("cache")
            .join(self.zip_name(entry, &integrity));

        let actual_integrity = crate::missing_hashes::write_zip_and_calc_integrity(
            file,
            path,
            self.cache_key_compression(),
            entry.name(),
        )
        .await
        .context("writing zip and calculating integrity")?;

        if actual_integrity != integrity {
            eprintln!("Fail:     {url}");
            eprintln!("  expected: {integrity}");
            eprintln!("  got:      {actual_integrity}");
            anyhow::bail!("got wrong hash");
        } else {
            eprintln!("Success:  {url}");
        }

        Ok(())
    }
}

fn entry_to_source(
    entry: &yarn_lock_parser::Entry<'_>,
    missing_lookup: &mut HashMap<String, String>,
) -> anyhow::Result<SourceWithIntegrity> {
    if (entry.is_npm() as u8 + entry.is_tar() as u8 + entry.is_git() as u8) > 1 {
        bail!("Ambiguous source: {}", entry.resolved)
    }
    if entry.is_npm() || entry.is_tar() {
        let integrity = if let Some(integrity) = entry.integrity_sha512() {
            integrity.to_owned()
        } else {
            missing_lookup.remove(entry.resolved).ok_or_else(|| {
                anyhow::anyhow!(OUTDATED_MISSING_HASHES_ERR).context(format!(
                    "unable to find missing hash for {}",
                    entry.resolved
                ))
            })?
        };

        let url = if entry.is_npm() {
            entry.npm_url()
        } else {
            entry.resolution().to_owned()
        };

        if integrity.len() != 128 {
            bail!(
                "Invalid length for sha512 integrity in missing-hashes.json {}",
                entry.resolved
            );
        }

        Ok(SourceWithIntegrity::Tgz { url, integrity })
    } else if entry.is_git() {
        let commit = entry.git_commit().ok_or_else(|| {
            anyhow::anyhow!("Git dependency without commit hash: {}", entry.resolved)
        })?;
        Ok(SourceWithIntegrity::Git {
            repo: entry.protocol_and_source().unwrap().into(),
            commit: commit.to_owned(),
        })
    } else {
        bail!("Unsupported or unrecognized source: {}", entry.resolved);
    }
}

enum SourceWithIntegrity {
    Tgz { url: String, integrity: String },
    Git { repo: String, commit: String },
}
