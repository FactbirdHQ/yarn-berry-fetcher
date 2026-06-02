use anyhow::{Context, bail};

use std::io::Seek;

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
