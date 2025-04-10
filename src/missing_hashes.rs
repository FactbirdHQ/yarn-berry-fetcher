use crate::{Lockfile, CacheKey, Source};

impl generate_from_lockfile(lockfile: Lockfile, cache_key: CacheKey) {

}

impl from_file(path: Option<&str>) -> Vec<Source> {
    let Some(path) = path else {
        return vec![];
    };
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
