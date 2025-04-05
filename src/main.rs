use std::collections::HashSet;
use std::ffi::CString;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;

use chrono::DateTime;
use deko::read::AnyDecoder;
use dostime::DOSDateTime;
use axfive_libzip::archive::{Archive as ZipArchive, OpenFlag};
use axfive_libzip::file::{Compression, Encoding};
use axfive_libzip::source::Source;

// https://github.com/yarnpkg/berry/blob/e06bacdb8091b7a25fdb7911c3466184b94fa040/packages/yarnpkg-fslib/sources/constants.ts#L15
const SAFE_TIME: LazyLock<DOSDateTime> = LazyLock::new(|| {
    DOSDateTime::try_from(DateTime::from_timestamp(456789000, 0).unwrap().naive_utc()).unwrap()
});

fn add_ancestors(
    zip: &mut ZipArchive,
    included_directories: &mut HashSet<PathBuf>,
    dir: &Path,
) {
    if let Some(parent) = dir.parent() {
        add_ancestors(zip, included_directories, parent);
    }

    if dir == Path::new("") {
        return;
    }

    if !included_directories.insert(dir.to_owned()) {
        return;
    }

    let mut dir = dir.to_owned().into_os_string().into_vec();
    dir.push(b'/');

    zip.add_dir_entry(
        CString::new(dir).unwrap(),
        Encoding::Guess,
        Some(0o755),
        Some((*SAFE_TIME).into()),
    ).unwrap();
}

fn main() {
    let mut args = std::env::args().skip(1);
    let name = args.next().unwrap();
    let dst = args.next().unwrap();
    let dst = PathBuf::from(dst);

    let mut tar = tar::Archive::new(AnyDecoder::new(std::io::stdin()));

    let mut included_directories = HashSet::new();

    let mut first_open = true;
    for entry in tar.entries().unwrap() {
        let mut entry = entry.unwrap();

        // strip "package/" and add "node_modules/{name}/"
        let path = PathBuf::from("node_modules/")
            .join(&name)
            .join(OsString::from_vec(
                entry
                    .path_bytes()
                    .strip_prefix(b"package/")
                    .expect("Paths in tar must be prefixed with 'package/'")
                    .to_vec(),
            ));

        let header = entry.header();
        let mode = header.mode().unwrap();

        // We need to re-open the zip for each entry, because libzip may access
        // the buffer we pass until the archive has been closed, but we want to
        // free the buffer after processing each entry.
        let mut buf = vec![];
        let mut zip = {
            let src = Source::try_from(AsRef::<Path>::as_ref(&dst)).unwrap();
            if first_open {
                first_open = false;
                ZipArchive::open(src, [OpenFlag::Create, OpenFlag::Exclusive]).unwrap()
            } else {
                ZipArchive::open(src, []).unwrap()
            }
        };

        // Insert all parent directories of the new path
        if let Some(parent) = path.parent() {
            add_ancestors(&mut zip, &mut included_directories, parent);
        }

        match header.entry_type() {
            tar::EntryType::Regular => {
                entry.read_to_end(&mut buf).unwrap();

                let src = Source::try_from(&buf[..]).unwrap();
                let path = CString::new(path.into_os_string().into_vec()).unwrap();
                zip.add(
                    path,
                    src,
                    Encoding::Guess,
                    Compression::Store,
                    Some(mode as u16),
                    Some((*SAFE_TIME).into()),
                    false,
                )
                .unwrap();
            }
            tar::EntryType::Directory => {
                if !included_directories.insert(path.to_owned()) {
                    return;
                }

                let path = path.to_owned().into_os_string().into_vec();

                zip.add_dir_entry(
                    CString::new(path).unwrap(),
                    Encoding::Guess,
                    Some(0o755),
                    Some((*SAFE_TIME).into()),
                ).unwrap();
            }
            other => {
                panic!("Unsupported tar entry: {:?} {:?}", path, other)
            }
        }

        zip.close().unwrap();
    }
}
