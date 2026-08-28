use std::collections::HashMap;
use std::path::Path;

use anyhow::{self, Context};
use tokio::io::AsyncSeekExt;
use tokio_retry2::{
    Retry, RetryError,
    strategy::{ExponentialFactorBackoff, jitter},
};

const MAX_ATTEMPTS: usize = 5;

/// Reads the `npmAuthToken` of every registry in the `.yarnrc.yml` at the given path,
/// keyed by the registry it authenticates to.
pub fn load_registry_tokens(yarnrc_path: &Path) -> HashMap<String, String> {
    #[derive(serde::Deserialize, Default)]
    struct NpmRegistry {
        #[serde(rename = "npmAuthToken", default)]
        npm_auth_token: Option<String>,
    }
    #[derive(serde::Deserialize, Default)]
    struct YarnRc {
        #[serde(rename = "npmRegistries", default)]
        npm_registries: HashMap<String, NpmRegistry>,
    }

    let content = match std::fs::read_to_string(yarnrc_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(e) => {
            eprintln!("warning: failed to read {}: {e}", yarnrc_path.display());
            return HashMap::new();
        }
    };
    let yarnrc: YarnRc = match serde_yml::from_str(&content) {
        Ok(y) => y,
        Err(e) => {
            eprintln!("warning: failed to parse {}: {e}", yarnrc_path.display());
            return HashMap::new();
        }
    };
    yarnrc
        .npm_registries
        .into_iter()
        .filter_map(|(url, reg)| reg.npm_auth_token.map(|t| (url, t)))
        .collect()
}

/// Fetches the given URL and writes contents to a temporary file. Exponential backoff.
/// Requests to a registry that `registry_tokens` has a token for are authenticated with it.
pub async fn fetch_to_tempfile(
    http_client: &reqwest::Client,
    url: &str,
    registry_tokens: &HashMap<String, String>,
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
        if let Some((_, token)) = registry_tokens
            .iter()
            .find(|(registry, _)| url.starts_with(registry.as_str()))
        {
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
