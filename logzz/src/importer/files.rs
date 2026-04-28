use eyre::Result;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;
use walkdir::WalkDir;

use crate::archive::{is_archive_file, is_partial_file};
use crate::importer::FileHash;

pub fn iter_files(input_dir: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    WalkDir::new(input_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            let path = entry.path();
            path.is_file() && !is_archive_file(path) && !is_partial_file(path)
        })
        .map(|entry| entry.into_path())
}

pub async fn file_hash(path: &Path) -> Result<FileHash> {
    let file = tokio::fs::File::open(path).await?;
    let mut reader = tokio::io::BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];

    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(FileHash(hex::encode(hasher.finalize())))
}
