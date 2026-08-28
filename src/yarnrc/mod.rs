//! Reads the registry credentials the fetcher needs out of `.yarnrc.yml`.

mod env;

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;

const IMPURE_ENV_HINT: &str = r#"
The .yarnrc.yml reads this token from the environment. A fetchYarnBerryDeps
derivation runs with a cleared environment unless it is told otherwise, so pass
the variable through with impureEnvVars, which needs the configurable-impure-env
experimental feature:

  fetchYarnBerryDeps {
    # ...
    impureEnvVars = [ "NPM_AUTH_TOKEN" ];
  }
"#;

#[derive(serde::Deserialize)]
struct NpmRegistry {
    #[serde(rename = "npmAuthToken")]
    npm_auth_token: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct YarnRc {
    #[serde(rename = "npmRegistries", default)]
    npm_registries: HashMap<String, NpmRegistry>,
}

/// The `npmAuthToken`s from a `.yarnrc.yml`, keyed by the registry they authenticate to.
#[derive(Default)]
pub struct RegistryTokens {
    /// Normalized `host[/path]` prefixes and their token, longest prefix first.
    entries: Vec<(String, String)>,
}

impl RegistryTokens {
    /// Reads the tokens out of the `.yarnrc.yml` at `path`, resolving the environment
    /// variables their values interpolate. A missing file yields no tokens; a file that
    /// does not parse, or that reads a variable which is not set, is an error.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).context(format!("reading {}", path.display())),
        };

        let yarnrc: YarnRc =
            serde_yml::from_str(&contents).context(format!("parsing {}", path.display()))?;

        let mut entries = Vec::new();
        for (registry, config) in yarnrc.npm_registries {
            let Some(token) = config.npm_auth_token else {
                continue;
            };

            let token = env::interpolate_from_env(&token).map_err(|e| {
                e.context(IMPURE_ENV_HINT).context(format!(
                    "resolving the npmAuthToken of registry {registry} in {}",
                    path.display()
                ))
            })?;

            entries.push((normalize_registry(&registry), token));
        }

        // A URL under a registry that has a path is also under the registry that has only
        // the host, so match the most specific one.
        entries.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

        Ok(Self { entries })
    }

    /// Returns the token to authenticate a request for `url` with, if any registry in the
    /// `.yarnrc.yml` covers it.
    pub fn for_url(&self, url: &str) -> Option<&str> {
        let url = normalize_registry(url);

        self.entries
            .iter()
            .find(|(registry, _)| {
                url.strip_prefix(registry.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            })
            .map(|(_, token)| token.as_str())
    }
}

/// Reduces a registry key or a request URL to the `host[/path]` the two are compared on.
///
/// The keys under `npmRegistries` are conventionally written without a scheme, as in
/// `//npm.pkg.github.com`, so dropping the scheme from both sides is what lets such a key
/// match the `https://` URL the lockfile resolves to.
fn normalize_registry(registry: &str) -> String {
    let registry = registry
        .split_once("://")
        .map(|(_scheme, rest)| rest)
        .unwrap_or(registry);

    registry
        .trim_start_matches('/')
        .trim_end_matches('/')
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(pairs: &[(&str, &str)]) -> RegistryTokens {
        let mut entries: Vec<(String, String)> = pairs
            .iter()
            .map(|(registry, token)| (normalize_registry(registry), token.to_string()))
            .collect();
        entries.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        RegistryTokens { entries }
    }

    #[test]
    fn matches_a_scheme_relative_registry_against_an_https_url() {
        let tokens = tokens(&[("//npm.pkg.github.com", "gh")]);

        assert_eq!(
            tokens.for_url("https://npm.pkg.github.com/@scope/pkg/-/pkg-1.0.0.tgz"),
            Some("gh")
        );
    }

    #[test]
    fn matches_a_registry_written_with_a_scheme() {
        let tokens = tokens(&[("https://npm.example.com", "t")]);

        assert_eq!(tokens.for_url("https://npm.example.com/pkg.tgz"), Some("t"));
        // The scheme is not what decides the match.
        assert_eq!(tokens.for_url("http://npm.example.com/pkg.tgz"), Some("t"));
    }

    #[test]
    fn ignores_a_trailing_slash_on_either_side() {
        let tokens = tokens(&[("//npm.example.com/", "t")]);

        assert_eq!(tokens.for_url("https://npm.example.com"), Some("t"));
        assert_eq!(tokens.for_url("https://npm.example.com/pkg.tgz"), Some("t"));
    }

    #[test]
    fn does_not_match_a_host_that_merely_starts_the_same() {
        let tokens = tokens(&[("//npm.example.com", "t")]);

        assert_eq!(
            tokens.for_url("https://npm.example.com.attacker.test/pkg.tgz"),
            None
        );
        assert_eq!(tokens.for_url("https://registry.npmjs.org/pkg.tgz"), None);
    }

    #[test]
    fn prefers_the_registry_with_the_longest_matching_path() {
        let tokens = tokens(&[
            ("//npm.example.com", "host"),
            ("//npm.example.com/team", "team"),
        ]);

        assert_eq!(
            tokens.for_url("https://npm.example.com/team/pkg.tgz"),
            Some("team")
        );
        assert_eq!(
            tokens.for_url("https://npm.example.com/other/pkg.tgz"),
            Some("host")
        );
    }

    #[test]
    fn a_yarnrc_without_registries_yields_no_tokens() {
        assert_eq!(
            tokens(&[]).for_url("https://registry.npmjs.org/pkg.tgz"),
            None
        );
    }
}
