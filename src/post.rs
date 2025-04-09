use crate::{Cache, Lockfile, Source, get_sources_from_lockfile};

pub fn make_cache_writable(cache_dir: &str) {
    assert!(
        std::process::Command::new("rm")
            .arg("-rf")
            .arg(".yarn/cache")
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("mkdir")
            .arg("-p")
            .arg(".yarn")
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("cp")
            .arg("-R")
            .arg("--reflink=auto")
            .arg(cache_dir)
            .arg(".yarn/cache")
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("chmod")
            .arg("-R")
            .arg("u+w")
            .arg(".yarn/cache")
            .status()
            .unwrap()
            .success()
    );
}

impl Cache {
    pub fn repack_git_deps(&self, lockfile: Lockfile) {
        let sources = get_sources_from_lockfile(lockfile);
        for source in sources {
            let Source::Git {
                name,
                integrity,
                commit,
                repo,
            } = source
            else {
                continue;
            };

            /*
            let mut tar_proc = std::process::Command::new("tar")
                .arg("--sort=name")
                .arg("--exclude=.gitignore")
                .arg("--exclude=package-lock.json")
                .arg("--exclude=yarn.lock")
                .arg("--exclude=pnpm-lock.yaml")
                .arg("-c")
                .arg("-C")
                .arg(&format!(".yarn/cache/{}", commit))
                .arg(".")
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            */

            let package_tgz =
                if std::fs::exists(format!(".yarn/cache/{}/package-lock.json", commit)).unwrap() {
                    let pack_output = std::process::Command::new("npm")
                        .arg("pack")
                        .current_dir(format!(".yarn/cache/{}", commit))
                        .output()
                        .unwrap();
                    if !pack_output.status.success() {
                        eprintln!("{:?}", pack_output);
                        std::process::exit(1);
                    }
                    format!(
                        ".yarn/cache/{}/{}",
                        commit,
                        String::from_utf8(pack_output.stdout).unwrap().trim()
                    )
                } else {
                    std::fs::File::create(format!(".yarn/cache/{}/yarn.lock", commit)).unwrap();
                    let pack_output = std::process::Command::new("yarn")
                        .arg("pack")
                        .arg("--out")
                        .arg("package.tgz")
                        .current_dir(format!(".yarn/cache/{}", commit))
                        .output()
                        .unwrap();
                    if !pack_output.status.success() {
                        eprintln!("{:?}", pack_output);
                        std::process::exit(1);
                    }
                    format!(".yarn/cache/{}/package.tgz", commit)
                };

            self.write_zip_and_check(
                &name,
                "https",
                &format!("{}#commit={}", repo, commit),
                &integrity,
                std::fs::File::open(package_tgz).unwrap(),
            )
            .unwrap();

            //let tar_output = tar_proc.wait_with_output().unwrap();
            //assert!(tar_output.status.success());
        }
    }
}
