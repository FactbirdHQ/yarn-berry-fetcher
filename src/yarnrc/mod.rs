//! Reads the registry credentials the fetcher needs out of `.yarnrc.yml`.
//!
//! Values are interpolated with `shellexpand::env`, mirroring how yarn's own Rust port
//! resolves `.yarnrc.yml` settings in `packages/zpm-config/src/lib.rs` of `yarnpkg/zpm`.
//!
//! That is not the same as yarn 4's JavaScript behaviour, which `replaceEnvVariables` in
//! `packages/yarnpkg-core/sources/miscUtils.ts` defines. `shellexpand` expands a bare
//! `$NAME`, takes `:-` as falling back only when a variable is unset rather than also when
//! it is empty, has no `${NAME-fallback}` operator, ignores the `\$` escape, and leaves an
//! unclosed `${` alone instead of rejecting it. A token holding a `$` or a `\` therefore
//! resolves differently here than under the yarn binary that wrote the lockfile.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;

const IMPURE_ENV_HINT: &str = r#"
This npmAuthToken reads the environment. Nix clears the environment of a
fetchYarnBerryDeps derivation, so pass the variable through with impureEnvVars,
which needs the configurable-impure-env experimental feature:

  fetchYarnBerryDeps {
    # ...
    impureEnvVars = lib.fetchers.proxyImpureEnvVars ++ [ "NPM_AUTH_TOKEN" ];
  }

Append to proxyImpureEnvVars rather than assigning a bare list. Caller arguments
overwrite the proxy variables that fetchYarnBerryDeps sets for itself.
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
    /// Reads the tokens out of the `.yarnrc.yml` at `path`, expanding the environment
    /// variables their values reference. A missing file yields no tokens. A file that does
    /// not parse, or that reads a variable which is not set, is an error.
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

            let token = shellexpand::env(&token)
                .map_err(|e| {
                    anyhow::anyhow!("{e}")
                        .context(IMPURE_ENV_HINT)
                        .context(format!(
                            "resolving the npmAuthToken of registry {registry} in {}",
                            path.display()
                        ))
                })?
                .into_owned();

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

/// Reduces a registry key or a request URL to the `host[/path]` the two compare on.
///
/// `npmRegistries` keys are conventionally scheme-relative, as in `//npm.pkg.github.com`,
/// so dropping the scheme from both sides is what lets one match an `https://` lockfile URL.
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

    fn write_yarnrc(dir: &Path, token: &str) -> std::path::PathBuf {
        let path = dir.join(".yarnrc.yml");
        std::fs::write(
            &path,
            format!("npmRegistries:\n  \"//npm.example.com\":\n    npmAuthToken: \"{token}\"\n"),
        )
        .unwrap();
        path
    }

    /// `shellexpand::env` reads the process environment, so every case that touches it lives
    /// in this one test rather than racing sibling tests over the same global state.
    #[test]
    fn expands_environment_variables_in_a_token() {
        unsafe {
            std::env::set_var("YBF_TEST_TOKEN", "s3cr3t");
            std::env::remove_var("YBF_TEST_MISSING");
        }
        let dir = tempfile::TempDir::new().unwrap();

        let path = write_yarnrc(dir.path(), "${YBF_TEST_TOKEN}");
        let tokens = RegistryTokens::load(&path).unwrap();
        assert_eq!(
            tokens.for_url("https://npm.example.com/pkg.tgz"),
            Some("s3cr3t")
        );

        // An unset variable is what raises the impureEnvVars hint.
        let path = write_yarnrc(dir.path(), "${YBF_TEST_MISSING}");
        assert!(RegistryTokens::load(&path).is_err());

        let path = write_yarnrc(dir.path(), "${YBF_TEST_MISSING:-fallback}");
        assert_eq!(
            RegistryTokens::load(&path)
                .unwrap()
                .for_url("https://npm.example.com/pkg.tgz"),
            Some("fallback")
        );

        // Recorded, not endorsed: yarn 4 resolves these differently. See the module docs.
        let path = write_yarnrc(dir.path(), "literal-$YBF_TEST_TOKEN");
        assert_eq!(
            RegistryTokens::load(&path)
                .unwrap()
                .for_url("https://npm.example.com/pkg.tgz"),
            // yarn 4 leaves a bare $NAME alone and yields "literal-$YBF_TEST_TOKEN".
            Some("literal-s3cr3t")
        );
    }

    #[test]
    fn a_missing_yarnrc_is_not_an_error() {
        let dir = tempfile::TempDir::new().unwrap();

        let tokens = RegistryTokens::load(&dir.path().join(".yarnrc.yml")).unwrap();
        assert_eq!(tokens.for_url("https://npm.example.com/pkg.tgz"), None);
    }
}
