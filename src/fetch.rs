use anyhow::{self, Context};
use tokio::io::AsyncSeekExt;
use tokio_retry2::{
    Retry, RetryError,
    strategy::{ExponentialFactorBackoff, jitter},
};

use crate::yarnrc::RegistryTokens;

const MAX_ATTEMPTS: usize = 5;

/// Fetches the given URL and writes contents to a temporary file. Exponential backoff.
/// Requests to a registry that `registry_tokens` has a token for are authenticated with it.
pub async fn fetch_to_tempfile(
    http_client: &reqwest::Client,
    url: &str,
    registry_tokens: &RegistryTokens,
) -> Result<async_tempfile::TempFile, anyhow::Error> {
    let i = std::sync::atomic::AtomicUsize::new(0);

    let action = async || -> Result<async_tempfile::TempFile, RetryError<anyhow::Error>> {
        let i = i.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if i != 0 {
            let prev = i - 1;
            eprintln!("Failed to fetch (on try {prev}/{MAX_ATTEMPTS}): {url}");
        }

        let mut file = async_tempfile::TempFile::new()
            .await
            .context("opening tempfile")
            .map_err(RetryError::Permanent)?;

        let mut request = http_client.get(url);
        if let Some(token) = registry_tokens.for_url(url) {
            request = request.bearer_auth(token);
        }

        let mut response = request
            .send()
            .await
            .context("sending http request")
            .map_err(|err| RetryError::Transient {
                err,
                // use normal retry behaviour
                retry_after: None,
            })?;

        if !response.status().is_success() {
            return RetryError::to_transient(anyhow::anyhow!(
                "non-successful HTTP response: {}",
                response.status()
            ));
        }

        while let Some(chunk) = response
            .chunk()
            .await
            .context("reading response chunk")
            .map_err(|err| RetryError::Transient {
                err,
                retry_after: None,
            })?
        {
            tokio::io::copy(&mut std::io::Cursor::new(chunk), &mut file)
                .await
                .context("copying response chunk into tempfile")
                .map_err(RetryError::Permanent)?;
        }

        file.seek(std::io::SeekFrom::Start(0))
            .await
            .context("seeking tempfile back to the beginning")
            .map_err(RetryError::Permanent)?;

        Ok(file)
    };

    let retry_strategy = ExponentialFactorBackoff::from_millis(500, 2.0)
        .map(jitter)
        .take(MAX_ATTEMPTS);

    Retry::spawn(retry_strategy, action)
        .await
        .context("finally gave up fetching")
}
