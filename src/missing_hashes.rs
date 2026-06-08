use crate::{EntryExt, Lockfile, LockfileExt, fetch::fetch_to_tempfile, zip};
use anyhow::Context;
use futures::{StreamExt, TryStreamExt};
use sha2::{Digest, Sha512};
use std::{collections::BTreeMap, path::PathBuf};
use tokio_util::io::InspectReader;

pub async fn get_missing_hashes(
    lockfile: Lockfile<'_>,
    http_client: &reqwest::Client,
    fetch_concurrency: usize,
) -> anyhow::Result<BTreeMap<String, String>> {
    let (_cache_key, compression) = lockfile
        .cache_key_parsed()
        .expect("validated lockfile to have cache_key");

    // find entries with missing hashes
    futures::stream::iter(lockfile.entries.into_iter().filter(|entry| {
        entry.is_real_source()
            && (entry.is_npm() || entry.is_tar())
            && entry.integrity_sha512().is_none()
    }))
    .map(|entry| async move {
        let url = if entry.is_npm() {
            entry.npm_url()
        } else if entry.is_tar() {
            entry.resolution().to_owned()
        } else {
            anyhow::bail!("Unsupported or unrecognized source: {}", entry.resolved);
        };

        let f = fetch_to_tempfile(http_client, &url).await?;
        eprintln!("Success:  {url}");

        let zip_dir = async_tempfile::TempDir::new()
            .await
            .context("creating tempdir")?;

        // write_yarn_zip expects to be the first one opening the file,
        // so we create a TempDir and pass it out.zip in that.
        let zip_path = zip_dir.dir_path().join("out.zip");

        let integrity = write_zip_and_calc_integrity(f, zip_path, compression, entry.name)
            .await
            .context("calculating integrity")?;

        zip_dir.drop_async().await;

        Ok::<_, anyhow::Error>((entry.resolved.to_string(), integrity))
    })
    .buffer_unordered(fetch_concurrency)
    .try_collect::<BTreeMap<_, _>>()
    .await
}

/// Calculates the integrity digest, which is the sha512 sum of a specially crafted zipfile,
/// lowerhex-encoded.
/// This must be written to a named file as we call into C here, and that wants a path.
pub async fn write_zip_and_calc_integrity(
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    path: PathBuf,
    compression: Option<u32>,
    entry_name: &str,
) -> anyhow::Result<String> {
    zip::write_yarn_zip_async(entry_name.to_owned(), path.clone(), reader, compression).await?;

    // hash the produced zip file
    let zip_file = tokio::fs::File::open(&path)
        .await
        .context("opening written zipfile")?;

    let mut hasher = Sha512::new();
    let mut r = InspectReader::new(zip_file, |d| {
        hasher.update(d);
    });
    tokio::io::copy(&mut r, &mut tokio::io::sink())
        .await
        .context("reading the written zipfile")?;

    Ok(hex::encode(hasher.finalize()))
}
