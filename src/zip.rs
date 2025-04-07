use std::collections::HashSet;
use std::ffi::CString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;

use axfive_libzip::archive::{Archive as ZipArchive, OpenFlag};
use axfive_libzip::file::{Compression, Encoding};
use axfive_libzip::source::Source;
use chrono::DateTime;
use deko::read::AnyDecoder;
use dostime::DOSDateTime;
//use axfive_libzip::error::Zip as ZipError;

// https://github.com/yarnpkg/berry/blob/e06bacdb8091b7a25fdb7911c3466184b94fa040/packages/yarnpkg-fslib/sources/constants.ts#L15
const SAFE_TIME: LazyLock<DOSDateTime> = LazyLock::new(|| {
    DOSDateTime::try_from(DateTime::from_timestamp(456789000, 0).unwrap().naive_utc()).unwrap()
});

fn add_ancestors(zip: &mut ZipArchive, included_directories: &mut HashSet<PathBuf>, dir: &Path) {
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
    )
    .unwrap();
}

pub fn write_yarn_zip(
    package_name: &str,
    dst: PathBuf,
    source_stream: impl std::io::Read,
    compression: Option<u32>,
) {
    let mut tar = tar::Archive::new(AnyDecoder::new(source_stream));

    let mut included_directories = HashSet::new();

    let mut first_open = true;
    for entry in tar.entries().unwrap() {
        let mut entry = entry.unwrap();

        let path = entry.path().unwrap();
        let mut path_iter = path.components();
        path_iter.next();
        let path = path_iter.as_path();
        // strip "package/" and add "node_modules/{package_name}/"
        let path = PathBuf::from("node_modules/")
            .join(&package_name)
            .join(path);

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
                let add_result = zip.add(
                    path,
                    src,
                    Encoding::Guess,
                    match compression {
                        None => Compression::Default,
                        Some(0) => Compression::Store,
                        Some(i) => Compression::Deflate(i),
                    },
                    //if level == 0 { Compression::Store } else { Compression::Default },
                    Some(
                        mode as u16 & 0o755
                            | (if (mode & 0o111) != 0 { 0o111 } else { 0 })
                            | (if (mode & 0o444) != 0 { 0o444 } else { 0 }),
                    ),
                    Some((*SAFE_TIME).into()),
                    false,
                );
                if let Err(e) = add_result {
                    match format!("{}", e).as_str() {
                        "Error: File already exists" => {} // ignore
                        _ => panic!("{}", e),
                    }
                    /*match &e.zip {
                        Some(ZipError::Exists) => {}, // ignore
                        _ => panic!("{}", e),
                    }*/
                }
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
                )
                .unwrap();
            }
            other => {
                panic!("Unsupported tar entry: {:?} {:?}", path, other)
            }
        }

        zip.close().unwrap();
    }
}
