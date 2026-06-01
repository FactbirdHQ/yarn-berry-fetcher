use std::collections::HashSet;
use std::ffi::CString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;

use axfive_libzip::archive::{Archive as ZipArchive, OpenFlag};
use axfive_libzip::error::Zip as ZipError;
use axfive_libzip::file::{Compression, Encoding};
use axfive_libzip::source::Source;
use chrono::DateTime;
use deko::read::AnyDecoder;
use dostime::DOSDateTime;

// https://github.com/yarnpkg/berry/blob/e06bacdb8091b7a25fdb7911c3466184b94fa040/packages/yarnpkg-fslib/sources/constants.ts#L15
static SAFE_TIME: LazyLock<DOSDateTime> = LazyLock::new(|| {
    DOSDateTime::try_from(DateTime::from_timestamp(456789000, 0).unwrap().naive_utc()).unwrap()
});

const BATCH_SIZE: usize = 1024 * 1024 * 10; // 10 MiB
const BIG_FILE_THRESHOLD: usize = 1024 * 1024 * 2; // 2 MiB

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
    dst: impl AsRef<Path>,
    source_stream: impl std::io::Read,
    compression: Option<u32>,
) {
    let mut tar = tar::Archive::new(AnyDecoder::new(source_stream));

    let mut included_directories = HashSet::new();

    let mut first_open = true;
    let mut entries_iter = tar.entries().unwrap();

    // libzip may access the buffers we pass until the archive has been closed, but we need
    // to prevent keeping the entire archive contents in-memory.
    //
    // We can work around this by closing and re-opening the archive, which will trigger
    // libzip to free the buffers. On the other hand, we don't want to close and re-open
    // the archive on each file, because this is slow for a lot of smaller files.
    // As a compromise, we add up to BATCH_SIZE of content to the archive before performing the flush.
    'outer: loop {
        let mut zip = {
            let src = Source::try_from(dst.as_ref()).unwrap();
            if first_open {
                first_open = false;
                ZipArchive::open(src, [OpenFlag::Create, OpenFlag::Exclusive]).unwrap()
            } else {
                ZipArchive::open(src, []).unwrap()
            }
        };

        let mut bytes_added = 0;

        while bytes_added < BATCH_SIZE {
            let Some(entry) = entries_iter.next() else {
                zip.close().unwrap();
                break 'outer;
            };

            let mut entry = entry.unwrap();

            let path = entry.path().unwrap();
            let mut path_iter = path.components();
            path_iter.next();
            let path = path_iter.as_path();
            // strip "package/" and add "node_modules/{package_name}/"
            let path = PathBuf::from("node_modules/").join(package_name).join(path);

            let header = entry.header();
            let mode = header.mode().unwrap();

            // Insert all parent directories of the new path
            if let Some(parent) = path.parent() {
                add_ancestors(&mut zip, &mut included_directories, parent);
            }

            match header.entry_type() {
                tar::EntryType::XGlobalHeader => {}
                tar::EntryType::Regular => {
                    let (src, must_flush) = if entry.size() >= (BIG_FILE_THRESHOLD as u64) {
                        // The file is >= BIG_FILE_THRESHOLD, pass it as a streaming reader to avoid buffering it,
                        // and immediately close the archive to force reading while the input tar
                        // stream is still at the position of that Entry.
                        let size = entry.size();
                        let src = Source::from_reader_with_size(
                            Box::new(entry) as Box<dyn std::io::Read>,
                            size as usize,
                        )
                        .unwrap();
                        (src, true)
                    } else {
                        // The file is < BIG_FILE_THRESHOLD, read it into a buffer and add it in batch containing
                        // up to BATCH_SIZE of contents combined
                        let mut buf = vec![];
                        bytes_added += entry.read_to_end(&mut buf).unwrap();
                        let src = Source::try_from(buf.into_boxed_slice()).unwrap();
                        (src, false)
                    };
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
                        Some(
                            mode as u16 & 0o755
                                | (if (mode & 0o111) != 0 { 0o111 } else { 0 })
                                | (if (mode & 0o444) != 0 { 0o444 } else { 0 }),
                        ),
                        Some((*SAFE_TIME).into()),
                        false,
                    );
                    if let Err(e) = add_result {
                        match &e.zip {
                            Some(ZipError::Exists) => {} // ignore
                            _ => panic!("{}", e),
                        }
                    }
                    if must_flush {
                        zip.close().unwrap();
                        continue 'outer;
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
                    panic!("Unsupported tar entry: {path:?} {other:?}")
                }
            }
        }

        // At this point we have written enough data to the archive, so we start a new batch
        zip.close().unwrap();
    }
}
