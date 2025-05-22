mod fetch;
mod missing_hashes;
mod zip;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use serde::Deserialize;
use sha2::{Digest, Sha512};
use yarn_lock_parser::Lockfile;

#[derive(Debug)]
struct CacheKey {
    version: usize,
    compression: Option<u32>,
}

static SUPPORTED_CACHE_VERSION: LazyLock<usize> = LazyLock::new(|| {
    std::env!("YARN_ZIP_SUPPORTED_CACHE_VERSION")
        .parse()
        .unwrap()
});

fn fetch(lockfile_path: &str, missing_hashes_path: Option<&str>, out_dir: &Path) {
    let lockfile_contents =
        std::fs::read_to_string(lockfile_path).expect("unable to open lockfile");
    let (cache_version, lockfile) = parse_lockfile(&lockfile_contents);
    let cache = Cache {
        out_dir: out_dir.to_owned(),
        key: cache_version,
        is_global: false,
    };
    cache.fetch(lockfile, missing_hashes_path);
    std::fs::write(out_dir.join("yarn.lock"), &lockfile_contents).unwrap();
    if let Some(missing_hashes_path) = missing_hashes_path {
        std::fs::copy(
            missing_hashes_path,
            PathBuf::from(&out_dir).join("missing-hashes.json"),
        )
        .unwrap();
    }
}

fn main() {
    let mut args = std::env::args().skip(1);

    match args.next().as_deref() {
        Some("fetch") => {
            let lockfile_path = args
                .next()
                .expect("yarn-berry-fetcher fetch <yarn.lock> [missing-hashes.json]");
            let missing_hashes_path = args.next();
            let out_dir = PathBuf::from(std::env::var("out").unwrap_or("out".into()));
            fetch(&lockfile_path, missing_hashes_path.as_deref(), &out_dir)
        }
        Some("prefetch") => {
            let lockfile_path = args
                .next()
                .expect("yarn-berry-fetcher prefetch <yarn.lock> [missing-hashes.json]");
            let missing_hashes_path = args.next();
            let tmp_dir = tempfile::TempDir::new().unwrap();
            fetch(
                &lockfile_path,
                missing_hashes_path.as_deref(),
                tmp_dir.path(),
            );

            let mut output = match std::process::Command::new("nix-hash")
                .arg("--type")
                .arg("sha256")
                .arg("--sri")
                .arg(tmp_dir.path())
                .output()
            {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Error spawning nix-hash: {}", e);
                    std::process::exit(1);
                }
            };
            if !output.status.success() {
                eprintln!("nix-hash errored:");
                std::io::stderr().write_all(&mut output.stderr).unwrap();
                std::process::exit(1);
            }

            tmp_dir.close().unwrap();
            println!("{}", String::from_utf8_lossy(&output.stdout));
        }
        Some("missing-hashes") => {
            let lockfile_path = args.next().expect("yarn-berry-fetcher missing-hashes <yarn.lock>");
            let lockfile_contents = std::fs::read_to_string(&lockfile_path).unwrap();
            let (cache_version, lockfile) = parse_lockfile(&lockfile_contents);

            let missing_hashes = missing_hashes::get_missing_hashes(lockfile, cache_version);

            println!("{}", serde_json::to_string_pretty(&missing_hashes).unwrap());
        }
        Some("convert") => {
            let help = "yarn-berry-fetcher convert <full package name> <npm.tgz>";
            let package_name = args.next().expect(help);
            zip::write_yarn_zip(
                &package_name,
                "out.zip".into(),
                std::fs::File::open(args.next().expect(help)).unwrap(),
                None,
            );
            eprintln!("wrote out.zip");
        }
        _ => {
            eprintln!(r#"USAGE: yarn-berry-fetcher <fetch|prefetch|missing-hashes|convert> [options]

fetch <yarn.lock> [missing-hashes.json]
    download packages in the given yarn lock file to the the directory
    specified by the "$out" environment variable. If "out" is unset,
    the literal direcory "out" will be used.

prefetch <yarn.lock> [missing-hashes.json]
    download packages in the given yarn.lock file, and print the
    appropriate 'nix-hash' for it.

missing-hashes <yarn.lock>
    Produce the missing-hashes data, and print it to stdout.
    Other commands expect this as the 'missing-hashes.json'
    argument.

convert <full package name> <npm.tgz>
    Convert an npm tgz file, write it to 'out.zip'."#);
            std::process::exit(1);
        }
    }
}

fn parse_lockfile(lockfile_contents: &str) -> (CacheKey, Lockfile<'_>) {
    let cache_version = {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct LockfileMetadata {
            cache_key: String,
        }
        #[derive(Deserialize)]
        struct Lockfile {
            __metadata: LockfileMetadata,
        }

        let lockfile: Lockfile = serde_yml::from_str(lockfile_contents)
            .expect("yarn.lock is not valid YAML. Are you trying to pass a yarn v1 lockfile?");
        let mut iter = lockfile.__metadata.cache_key.split('c');
        let version_str = iter.next().unwrap();
        let compression_str = iter.next();
        CacheKey {
            version: version_str.parse().unwrap(),
            compression: compression_str.map(|c| c.parse().unwrap()),
        }
    };

    eprintln!("{:?}", cache_version);

    if cache_version.version != *SUPPORTED_CACHE_VERSION {
        eprintln!(
            r#"
Error fetching berry dependencies.
Found cache version {}
Supported by this version of yarn-berry-fetcher: {}
Hint: Are you using fetchYarnBerryDeps from the correct yarn-berry version?"#,
            cache_version.version, *SUPPORTED_CACHE_VERSION
        );

        std::process::exit(1);
    }

    let lockfile = yarn_lock_parser::parse_str(lockfile_contents).unwrap();

    (cache_version, lockfile)
}

pub trait EntryExt {
    fn name(&self) -> &str;
    fn name_rest(&self) -> &str;
    fn scope(&self) -> Option<&str>;
    fn scope_name(&self) -> Option<&str>;
    fn resolution(&self) -> &str;
    fn protocol(&self) -> &str;
    fn protocol_and_source(&self) -> Option<&str>;
    fn source_selector_and_bindings(&self) -> &str;
    fn bindings(&self) -> Option<form_urlencoded::Parse<'_>>;
    fn source_and_selector(&self) -> &str;
    fn source(&self) -> Option<&str>;
    fn selector(&self) -> &str;
    fn is_real_source(&self) -> bool;
    fn is_npm(&self) -> bool;
    fn npm_url(&self) -> String;
    fn is_tar(&self) -> bool;
    fn is_git(&self) -> bool;
    fn git_commit(&self) -> Option<&str>;
    fn integrity_sha512(&self) -> Option<&str>;
    fn slug(&self) -> String;
    fn is_content_addressed(&self) -> bool;
}

impl EntryExt for yarn_lock_parser::Entry<'_> {
    fn name(&self) -> &str {
        if self.resolved.starts_with("@") {
            let second_at = self.resolved[1..].find("@").unwrap() + 1;
            &self.resolved[..second_at]
        } else {
            self.resolved.split_once("@").unwrap().0
        }
    }

    fn name_rest(&self) -> &str {
        let name = self.name();
        match name.split_once("/") {
            None => name,
            Some((_, rest)) => rest,
        }
    }

    fn scope(&self) -> Option<&str> {
        self.name().split_once("/").map(|(scope, _)| scope)
    }

    fn scope_name(&self) -> Option<&str> {
        self.scope().map(|scope| scope.strip_prefix("@").unwrap())
    }

    fn resolution(&self) -> &str {
        if self.resolved.starts_with("@") {
            let second_at = self.resolved[1..].find("@").unwrap() + 1;
            &self.resolved[second_at + 1..]
        } else {
            self.resolved.split_once("@").unwrap().1
        }
    }

    fn protocol(&self) -> &str {
        self.resolution().split_once(":").unwrap().0
    }

    fn source_selector_and_bindings(&self) -> &str {
        self.resolution().split_once(":").unwrap().1
    }

    fn bindings(&self) -> Option<form_urlencoded::Parse<'_>> {
        let source_selector_and_bindings = self.source_selector_and_bindings();
        source_selector_and_bindings
            .rsplit_once("::")
            .map(|(_, bindings)| form_urlencoded::parse(bindings.as_bytes()))
    }

    fn source_and_selector(&self) -> &str {
        let source_selector_and_bindings = self.source_selector_and_bindings();
        source_selector_and_bindings
            .rsplit_once("::")
            .unwrap_or((source_selector_and_bindings, ""))
            .0
    }

    fn protocol_and_source(&self) -> Option<&str> {
        self.resolution()
            .split_once("#")
            .map(|(protocol_and_source, _)| protocol_and_source)
    }

    fn selector(&self) -> &str {
        let source_and_selector = self.source_and_selector();
        source_and_selector
            .split_once("#")
            .unwrap_or(("", source_and_selector))
            .1
    }

    fn source(&self) -> Option<&str> {
        self.source_and_selector()
            .split_once("#")
            .map(|(source, _selector)| source)
    }

    fn is_real_source(&self) -> bool {
        !["workspace", "patch", "link"].contains(&self.protocol())
    }

    fn npm_url(&self) -> String {
        if let Some(mut bindings) = self.bindings() {
            if let Some(archive_url) = bindings
                .find(|(name, _)| name == &"__archiveUrl")
                .map(|(_, archive_url)| archive_url)
            {
                return archive_url.into();
            }
        }

        format!(
            "https://registry.npmjs.org/{}/-/{}-{}.tgz",
            self.name(),
            self.name_rest(),
            self.selector()
        )
    }

    fn is_npm(&self) -> bool {
        self.protocol() == "npm"
    }

    fn is_tar(&self) -> bool {
        let resolution = self.resolution();
        ["http", "https"].contains(&self.protocol())
            && (resolution.ends_with(".tar.gz") || resolution.ends_with(".tgz"))
    }

    fn is_git(&self) -> bool {
        self.resolution().contains(".git#")
    }

    fn git_commit(&self) -> Option<&str> {
        self.selector()
            .split("&")
            .filter_map(|pair| pair.split_once("="))
            .find(|(name, _)| name == &"commit")
            .map(|(_, commit)| commit)
    }

    fn integrity_sha512(&self) -> Option<&str> {
        let integrity = self.integrity.split("/").last().unwrap();
        if integrity.is_empty() {
            None
        } else {
            assert_eq!(integrity.len(), 128);
            Some(integrity)
        }
    }

    fn slug(&self) -> String {
        let mut slug = "".to_string();
        slug.push_str(&self.name().replace("/", "-"));
        slug.push('-');
        slug.push_str(self.protocol());
        let selector = self.selector();
        if semver::Version::parse(selector).is_ok() {
            slug.push('-');
            slug.push_str(selector);
        }
        slug
    }

    fn is_content_addressed(&self) -> bool {
        !self.integrity.is_empty()
    }
}

// Panics if we don't know how to fetch the source (even with added integrity data)
impl TryFrom<&yarn_lock_parser::Entry<'_>> for SourceWithIntegrity {
    type Error = SourceWithoutIntegrity;

    fn try_from(
        e: &yarn_lock_parser::Entry,
    ) -> Result<SourceWithIntegrity, SourceWithoutIntegrity> {
        assert!(
            (e.is_npm() as u8 + e.is_tar() as u8 + e.is_git() as u8) < 2,
            "Ambiguous source: {}",
            e.resolved
        );
        if e.is_npm() {
            match e.integrity_sha512() {
                None => Err(SourceWithoutIntegrity::Tgz { url: e.npm_url() }),
                Some(integrity) => Ok(SourceWithIntegrity::Tgz {
                    url: e.npm_url(),
                    integrity: integrity.into(),
                }),
            }
        } else if e.is_tar() {
            match e.integrity_sha512() {
                None => Err(SourceWithoutIntegrity::Tgz {
                    url: e.resolution().into(),
                }),
                Some(integrity) => Ok(SourceWithIntegrity::Tgz {
                    url: e.resolution().into(),
                    integrity: integrity.into(),
                }),
            }
        } else if e.is_git() {
            match e.git_commit() {
                None => panic!("Git dependency without commit hash: {}", e.resolved),
                Some(commit) => Ok(SourceWithIntegrity::Git {
                    repo: e.protocol_and_source().unwrap().into(),
                    commit: commit.into(),
                }),
            }
        } else {
            panic!("Unsupported or unrecognized source: {}", e.resolved);
        }
    }
}

#[derive(Debug)]
enum SourceWithoutIntegrity {
    Tgz { url: String },
}

enum SourceWithIntegrity {
    Tgz { url: String, integrity: String },
    Git { repo: String, commit: String },
}

struct Cache {
    out_dir: PathBuf,
    key: CacheKey,
    is_global: bool,
}

impl Cache {
    fn zip_name(&self, entry: &yarn_lock_parser::Entry, integrity: &str) -> String {
        let ident_hash = hex::encode(Sha512::digest(format!(
            "{}{}",
            entry.scope_name().unwrap_or_default(),
            entry.name_rest(),
        )));
        let locator_hash = hex::encode(Sha512::digest(format!(
            "{}{}",
            ident_hash,
            entry.resolution()
        )));

        format!(
            "{}-{}-{}.zip",
            entry.slug(),
            &locator_hash[..10],
            if self.is_global || !entry.is_content_addressed() {
                format!(
                    "{}{}",
                    self.key.version,
                    self.key
                        .compression
                        .map(|c| format!("c{}", c))
                        .unwrap_or_default()
                )
            } else {
                integrity[..10].to_string()
            },
        )
    }

    fn write_zip_and_check(
        &self,
        entry: yarn_lock_parser::Entry,
        integrity: &str,
        source: impl std::io::Read,
    ) -> Result<PathBuf, String> {
        let dst = self
            .out_dir
            .join("cache")
            .join(self.zip_name(&entry, integrity));
        zip::write_yarn_zip(entry.name(), dst.clone(), source, self.key.compression);

        let out_hash = {
            let mut hasher = Sha512::new();
            let mut file = std::fs::File::open(&dst).unwrap();
            std::io::copy(&mut file, &mut hasher).unwrap();
            hex::encode(hasher.finalize())
        };

        if integrity == out_hash {
            Ok(dst)
        } else {
            Err(out_hash)
        }
    }
}
