use eyre::Result;
use std::fs as sfs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_ARCHIVE_DIR: &str = "./.local/archives";
pub const PARTIAL_SUFFIX: &str = ".part";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ArchiveKind {
    Zip,
    Rar,
}

#[derive(Debug, Clone)]
pub struct ExtractStats {
    pub files_extracted: usize,
    pub output_dir: PathBuf,
}

pub fn default_archive_dir() -> String {
    DEFAULT_ARCHIVE_DIR.to_string()
}

pub fn detect_archive_kind(path: &Path) -> Option<ArchiveKind> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "zip" => Some(ArchiveKind::Zip),
        "rar" => Some(ArchiveKind::Rar),
        _ => None,
    }
}

pub fn is_archive_file(path: &Path) -> bool {
    path.is_file() && detect_archive_kind(path).is_some() && !is_partial_file(path)
}

pub fn is_partial_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(PARTIAL_SUFFIX))
}

pub fn partial_archive_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("{name}{PARTIAL_SUFFIX}"))
        .unwrap_or_else(|| format!("archive{PARTIAL_SUFFIX}"));

    path.with_file_name(file_name)
}

pub fn sanitize_filename(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());

    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }

    let sanitized = sanitized.trim_matches('_');
    if sanitized.is_empty() {
        "archive".to_string()
    } else {
        sanitized.to_string()
    }
}

pub fn build_archive_filename(
    message_id: i32,
    original_name: Option<&str>,
    kind: ArchiveKind,
) -> String {
    let fallback = match kind {
        ArchiveKind::Zip => "archive.zip",
        ArchiveKind::Rar => "archive.rar",
    };
    let original_name = original_name.unwrap_or(fallback);
    let safe_name = sanitize_filename(original_name);

    format!("{message_id:010}-{safe_name}")
}

pub fn archive_output_dir(output_root: &Path, archive_path: &Path) -> PathBuf {
    let archive_stem = archive_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(sanitize_filename)
        .unwrap_or_else(|| "archive".to_string());

    output_root.join(archive_stem)
}

pub fn extract_archive(archive_path: &Path, output_root: &Path) -> Result<ExtractStats> {
    let kind = detect_archive_kind(archive_path)
        .ok_or_else(|| eyre::eyre!("Unsupported archive type: {}", archive_path.display()))?;

    sfs::create_dir_all(output_root)?;

    let final_output_dir = archive_output_dir(output_root, archive_path);
    if final_output_dir.exists() {
        return Err(eyre::eyre!(
            "Output directory already exists: {}",
            final_output_dir.display()
        ));
    }

    let archive_stem = final_output_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("archive");
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let temp_output_dir = output_root.join(format!(".extracting-{archive_stem}-{suffix}"));

    let extract_result = match kind {
        ArchiveKind::Zip => extract_zip(archive_path, &temp_output_dir),
        ArchiveKind::Rar => extract_rar(archive_path, &temp_output_dir),
    };

    match extract_result {
        Ok(stats) => {
            sfs::rename(&temp_output_dir, &final_output_dir)?;
            Ok(ExtractStats {
                files_extracted: stats.files_extracted,
                output_dir: final_output_dir,
            })
        }
        Err(error) => {
            let _ = sfs::remove_dir_all(&temp_output_dir);
            Err(error)
        }
    }
}

fn extract_zip(archive_path: &Path, output_dir: &Path) -> Result<ExtractStats> {
    let file = sfs::File::open(archive_path)?;
    let mut zip = zip::ZipArchive::new(io::BufReader::new(file))?;
    let mut count = 0usize;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let entry_name = entry.mangled_name();

        let rel: PathBuf = entry_name
            .components()
            .filter(|component| matches!(component, std::path::Component::Normal(_)))
            .collect();

        if rel.as_os_str().is_empty() {
            continue;
        }

        let dest = output_dir.join(&rel);

        if entry.is_dir() {
            sfs::create_dir_all(&dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                sfs::create_dir_all(parent)?;
            }

            let mut out = sfs::File::create(&dest)?;
            io::copy(&mut entry, &mut out)?;
            count += 1;
        }
    }

    Ok(ExtractStats {
        files_extracted: count,
        output_dir: output_dir.to_path_buf(),
    })
}

fn extract_rar(archive_path: &Path, output_dir: &Path) -> Result<ExtractStats> {
    sfs::create_dir_all(output_dir)?;

    let status = match Command::new("7z")
        .arg("x")
        .arg("-y")
        .arg("-bso0")
        .arg("-bsp0")
        .arg(format!("-o{}", output_dir.display()))
        .arg(archive_path)
        .status()
    {
        Ok(status) => status,
        Err(_) => Command::new("unrar")
            .arg("x")
            .arg("-y")
            .arg("-o+")
            .arg("-inul")
            .arg(archive_path)
            .arg(output_dir)
            .status()
            .map_err(|error| {
                eyre::eyre!("Failed to run `7z` or `unrar`: {error}. Is an extractor installed?")
            })?,
    };

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        return Err(eyre::eyre!("RAR extractor exited with code {code}"));
    }

    let count = walkdir::WalkDir::new(output_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .count();

    Ok(ExtractStats {
        files_extracted: count,
        output_dir: output_dir.to_path_buf(),
    })
}
