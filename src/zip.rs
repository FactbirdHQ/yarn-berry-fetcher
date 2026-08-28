use std::collections::HashSet;
use std::ffi::CString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::Context;
use axfive_libzip::archive::{Archive as ZipArchive, OpenFlag};
use axfive_libzip::error::Zip as ZipError;
use axfive_libzip::file::{Compression, Encoding};
use axfive_libzip::source::Source;
use chrono::DateTime;
use deko::read::AnyDecoder;
use dostime::DOSDateTime;
use tokio_util::io::SyncIoBridge;

// https://github.com/yarnpkg/berry/blob/e06bacdb8091b7a25fdb7911c3466184b94fa040/packages/yarnpkg-fslib/sources/constants.ts#L15
static SAFE_TIME: LazyLock<DOSDateTime> = LazyLock::new(|| {
    DOSDateTime::try_from(DateTime::from_timestamp(456789000, 0).unwrap().naive_utc()).unwrap()
});

const BATCH_SIZE: usize = 1024 * 1024 * 10; // 10 MiB
const BIG_FILE_THRESHOLD: usize = 1024 * 1024 * 2; // 2 MiB

fn add_ancestors(
    zip: &mut ZipArchive,
    included_directories: &mut HashSet<PathBuf>,
    dir: &Path,
) -> anyhow::Result<()> {
    if let Some(parent) = dir.parent() {
        add_ancestors(zip, included_directories, parent).context("recursing into add_ancestors")?;
    }

    if dir == Path::new("") {
        return Ok(());
    }

    if !included_directories.insert(dir.to_owned()) {
        return Ok(());
    }

    let mut dir = dir.to_owned().into_os_string().into_vec();
    dir.push(b'/');

    zip.add_dir_entry(
        CString::new(dir).context("constructing CString")?,
        Encoding::Guess,
        Some(0o755),
        Some((*SAFE_TIME).into()),
    )
    .context("adding dir_entry")?;

    Ok(())
}

pub async fn write_yarn_zip_async<R>(
    package_name: String,
    path: PathBuf,
    reader: R,
    compression: Option<u32>,
) -> anyhow::Result<R>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let reader = tokio::task::spawn_blocking({
        move || -> anyhow::Result<_> {
            let mut reader_sync = SyncIoBridge::new(reader);
            write_yarn_zip(&package_name, path, &mut reader_sync, compression)?;
            Ok(reader_sync.into_inner())
        }
    })
    .await
    .context("spawning write_yarn_zip task")??;

    Ok(reader)
}

/// Writes a specially crafted zipfile to the given destination.
/// The zipfile may not exist before calling this function.
/// This function is sync and does do blocking IO, it needs to be wrapped in a spawn_blocking call.
fn write_yarn_zip(
    package_name: &str,
    path: impl AsRef<Path>,
    reader: impl std::io::Read,
    compression: Option<u32>,
) -> anyhow::Result<()> {
    let mut tar = tar::Archive::new(AnyDecoder::new(reader));

    let mut included_directories = HashSet::new();

    let mut first_open = true;
    let mut entries_iter = tar.entries().context("iterating over tar entries")?;

    // libzip may access the buffers we pass until the archive has been closed, but we need
    // to prevent keeping the entire archive contents in-memory.
    //
    // We can work around this by closing and re-opening the archive, which will trigger
    // libzip to free the buffers. On the other hand, we don't want to close and re-open
    // the archive on each file, because this is slow for a lot of smaller files.
    // As a compromise, we add up to BATCH_SIZE of content to the archive before performing the flush.
    'outer: loop {
        let mut zip = {
            let src = Source::try_from(path.as_ref()).unwrap();
            if first_open {
                first_open = false;
                ZipArchive::open(src, [OpenFlag::Create, OpenFlag::Exclusive])
                    .context("opening ZipArchive for the first time")?
            } else {
                ZipArchive::open(src, []).context("opening ZipArchive once again")?
            }
        };

        let mut bytes_added = 0;

        while bytes_added < BATCH_SIZE {
            let Some(entry) = entries_iter.next() else {
                zip.close()
                    .map_err(|(_zip, err)| err)
                    .context("closing zip")?;
                break 'outer;
            };

            let mut entry = entry.context("getting entry")?;

            let path = entry.path().context("reading entry path")?;
            let mut path_iter = path.components();
            path_iter.next();
            // Collect remaining components, stripping CurDir (`.`) entries present in some
            // tarballs, to match yarn's path normalization behavior.
            let path: PathBuf = path_iter
                .filter(|c| !matches!(c, std::path::Component::CurDir))
                .collect();
            debug_assert!(!path.components().any(|c| c == std::path::Component::CurDir));
            // strip "package/" and add "node_modules/{package_name}/"
            let path = PathBuf::from("node_modules/").join(package_name).join(path);

            let header = entry.header();
            let mode = header.mode().context("reading mode bits")?;

            // Insert all parent directories of the new path
            if let Some(parent) = path.parent() {
                add_ancestors(&mut zip, &mut included_directories, parent)
                    .context("adding ancestors")?;
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
                        bytes_added += entry.read_to_end(&mut buf).context("reading from entry")?;
                        let src = Source::try_from(buf.into_boxed_slice())
                            .context("converting buffer into axfive_libzip::Source")?;
                        (src, false)
                    };
                    let path = CString::new(path.into_os_string().into_vec())
                        .context("constructing CString")?;
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
                            _ => return Err(e).context("adding to zip"),
                        }
                    }
                    if must_flush {
                        zip.close()
                            .map_err(|(_zip, err)| err)
                            .context("closing zip")?;
                        continue 'outer;
                    }
                }
                tar::EntryType::Directory => {
                    if !included_directories.insert(path.to_owned()) {
                        continue;
                    }

                    let path = path.to_owned().into_os_string().into_vec();

                    zip.add_dir_entry(
                        CString::new(path).context("constructing CString")?,
                        Encoding::Guess,
                        Some(0o755),
                        Some((*SAFE_TIME).into()),
                    )
                    .context("adding dir_entry")?;
                }
                other => {
                    anyhow::bail!("Unsupported tar entry: {path:?} {other:?}")
                }
            }
        }

        // At this point we have written enough data to the archive, so we start a new batch
        zip.close()
            .map_err(|(_zip, err)| err)
            .context("closing zip")?
    }

    Ok(())
}
