use std::fs as sfs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_ARCHIVE_DIR: &str = "./.local/archives";
pub const PARTIAL_SUFFIX: &str = ".part";
pub const ARCHIVE_PASSWORD_SUFFIX: &str = ".pass";
pub const ARCHIVE_NEEDS_PASSWORD_SUFFIX: &str = ".needs-password";

/// Hard cap on total bytes written per archive extraction, as a guard against zip bombs
/// (a small compressed archive that inflates to consume all available disk space).
const MAX_EXTRACTED_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// Hard cap on the number of entries extracted per archive, as a guard against archives
/// with millions of tiny/empty entries that exhaust inodes or extraction time.
const MAX_EXTRACTED_ENTRIES: usize = 200_000;

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("archive is password protected")]
    PasswordRequired,
    #[error("extraction failed: {0}")]
    Failed(String),
    #[error("archive exceeds the {MAX_EXTRACTED_BYTES} byte extraction limit (possible zip bomb)")]
    TooLarge,
    #[error("archive exceeds the {MAX_EXTRACTED_ENTRIES} entry extraction limit")]
    TooManyEntries,
}

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

pub fn archive_password_path(archive_path: &Path) -> PathBuf {
    let file_name = archive_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("{name}{ARCHIVE_PASSWORD_SUFFIX}"))
        .unwrap_or_else(|| format!("archive{ARCHIVE_PASSWORD_SUFFIX}"));
    archive_path.with_file_name(file_name)
}

pub fn archive_needs_password_path(archive_path: &Path) -> PathBuf {
    let file_name = archive_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("{name}{ARCHIVE_NEEDS_PASSWORD_SUFFIX}"))
        .unwrap_or_else(|| format!("archive{ARCHIVE_NEEDS_PASSWORD_SUFFIX}"));
    archive_path.with_file_name(file_name)
}

pub fn find_archive_by_message_id(archive_dir: &Path, message_id: i32) -> Option<PathBuf> {
    let prefix = format!("{:010}-", message_id);
    let reader = match sfs::read_dir(archive_dir) {
        Ok(reader) => reader,
        Err(error) => {
            tracing::warn!(
                error = %error,
                archive_dir = %archive_dir.display(),
                "failed to list archive directory while looking up archive by message id"
            );
            return None;
        }
    };
    reader.filter_map(Result::ok).find_map(|entry| {
        let path = entry.path();
        let name = path.file_name()?.to_str()?;
        if name.starts_with(&prefix) && detect_archive_kind(&path).is_some() {
            Some(path)
        } else {
            None
        }
    })
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

pub fn extract_archive(
    archive_path: &Path,
    output_root: &Path,
    password: Option<&str>,
) -> Result<ExtractStats, ExtractError> {
    let kind = detect_archive_kind(archive_path).ok_or_else(|| {
        ExtractError::Failed(format!("Unsupported archive type: {}", archive_path.display()))
    })?;

    sfs::create_dir_all(output_root)
        .map_err(|e| ExtractError::Failed(e.to_string()))?;

    let final_output_dir = archive_output_dir(output_root, archive_path);
    if final_output_dir.exists() {
        return Err(ExtractError::Failed(format!(
            "Output directory already exists: {}",
            final_output_dir.display()
        )));
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
        ArchiveKind::Zip => extract_zip(
            archive_path,
            &temp_output_dir,
            password,
            MAX_EXTRACTED_BYTES,
            MAX_EXTRACTED_ENTRIES,
        ),
        ArchiveKind::Rar => extract_rar(archive_path, &temp_output_dir, password),
    };

    match extract_result {
        Ok(stats) => {
            sfs::rename(&temp_output_dir, &final_output_dir)
                .map_err(|e| ExtractError::Failed(e.to_string()))?;
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

fn extract_zip(
    archive_path: &Path,
    output_dir: &Path,
    password: Option<&str>,
    max_bytes: u64,
    max_entries: usize,
) -> Result<ExtractStats, ExtractError> {
    let file =
        sfs::File::open(archive_path).map_err(|e| ExtractError::Failed(e.to_string()))?;
    let mut zip = zip::ZipArchive::new(io::BufReader::new(file))
        .map_err(|e| ExtractError::Failed(e.to_string()))?;
    let mut count = 0usize;
    let mut total_bytes = 0u64;

    for i in 0..zip.len() {
        let mut entry = if let Some(pwd) = password {
            zip.by_index_decrypt(i, pwd.as_bytes())
                .map_err(|e| ExtractError::Failed(e.to_string()))?
        } else {
            match zip.by_index(i) {
                Ok(entry) => entry,
                Err(zip::result::ZipError::UnsupportedArchive(msg))
                    if msg == zip::result::ZipError::PASSWORD_REQUIRED =>
                {
                    return Err(ExtractError::PasswordRequired);
                }
                Err(e) => return Err(ExtractError::Failed(e.to_string())),
            }
        };

        let entry_name = entry.mangled_name();
        let rel: PathBuf = entry_name
            .components()
            .filter(|c| matches!(c, std::path::Component::Normal(_)))
            .collect();

        if rel.as_os_str().is_empty() {
            continue;
        }

        let dest = output_dir.join(&rel);

        if entry.is_dir() {
            sfs::create_dir_all(&dest).map_err(|e| ExtractError::Failed(e.to_string()))?;
        } else {
            if count >= max_entries {
                return Err(ExtractError::TooManyEntries);
            }

            if let Some(parent) = dest.parent() {
                sfs::create_dir_all(parent)
                    .map_err(|e| ExtractError::Failed(e.to_string()))?;
            }
            let mut out = sfs::File::create(&dest)
                .map_err(|e| ExtractError::Failed(e.to_string()))?;
            total_bytes += copy_limited(&mut entry, &mut out, max_bytes - total_bytes)?;
            count += 1;
        }
    }

    Ok(ExtractStats {
        files_extracted: count,
        output_dir: output_dir.to_path_buf(),
    })
}

/// Copies from `src` to `dest`, returning the number of bytes copied. Errors with
/// [`ExtractError::TooLarge`] once more than `limit` bytes have been written, so
/// callers can detect archives that decompress far beyond their compressed size
/// (zip bombs) instead of writing until the disk fills up.
fn copy_limited<R: io::Read, W: io::Write>(
    src: &mut R,
    dest: &mut W,
    limit: u64,
) -> Result<u64, ExtractError> {
    let mut buf = [0u8; 64 * 1024];
    let mut copied = 0u64;
    loop {
        let n = src
            .read(&mut buf)
            .map_err(|e| ExtractError::Failed(e.to_string()))?;
        if n == 0 {
            break;
        }
        copied += n as u64;
        if copied > limit {
            return Err(ExtractError::TooLarge);
        }
        dest.write_all(&buf[..n])
            .map_err(|e| ExtractError::Failed(e.to_string()))?;
    }
    Ok(copied)
}

fn extract_rar(
    archive_path: &Path,
    output_dir: &Path,
    password: Option<&str>,
) -> Result<ExtractStats, ExtractError> {
    sfs::create_dir_all(output_dir).map_err(|e| ExtractError::Failed(e.to_string()))?;

    let mut cmd_7z = Command::new("7z");
    cmd_7z.arg("x").arg("-y");
    if let Some(pwd) = password {
        cmd_7z.arg(format!("-p{pwd}"));
    }
    cmd_7z
        .arg(format!("-o{}", output_dir.display()))
        .arg(archive_path);

    let output = match cmd_7z.output() {
        Ok(out) => out,
        Err(_) => {
            let mut cmd_unrar = Command::new("unrar");
            cmd_unrar.arg("x").arg("-y").arg("-o+");
            if let Some(pwd) = password {
                cmd_unrar.arg(format!("-p{pwd}"));
            }
            cmd_unrar.arg("-inul").arg(archive_path).arg(output_dir);
            cmd_unrar.output().map_err(|e| {
                ExtractError::Failed(format!(
                    "Failed to run `7z` or `unrar`: {e}. Is an extractor installed?"
                ))
            })?
        }
    };

    if !output.status.success() {
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if password.is_none() {
            let lower = combined.to_lowercase();
            if lower.contains("wrong password")
                || lower.contains("encrypted")
                || lower.contains("enter password")
            {
                return Err(ExtractError::PasswordRequired);
            }
        }
        let code = output.status.code().unwrap_or(-1);
        return Err(ExtractError::Failed(format!(
            "RAR extractor exited with code {code}"
        )));
    }

    let mut count = 0usize;
    let mut total_bytes = 0u64;
    for entry in walkdir::WalkDir::new(output_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
    {
        count += 1;
        total_bytes += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
    }

    // 7z/unrar already wrote the files to disk by the time we can inspect them, so this
    // is a post-hoc guard against zip/rar bombs rather than a preventive one: it stops a
    // runaway extraction from being imported and leaves the oversized output cleaned up.
    if count > MAX_EXTRACTED_ENTRIES {
        let _ = sfs::remove_dir_all(output_dir);
        return Err(ExtractError::TooManyEntries);
    }
    if total_bytes > MAX_EXTRACTED_BYTES {
        let _ = sfs::remove_dir_all(output_dir);
        return Err(ExtractError::TooLarge);
    }

    Ok(ExtractStats {
        files_extracted: count,
        output_dir: output_dir.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        std::env::temp_dir().join(format!("logzz-test-{label}-{pid}-{n}"))
    }

    #[test]
    fn sanitize_filename_replaces_unsafe_characters() {
        assert_eq!(sanitize_filename("report.zip"), "report.zip");
        assert_eq!(sanitize_filename("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(sanitize_filename("a b\tc/d\\e"), "a_b_c_d_e");
        assert_eq!(sanitize_filename("///"), "archive");
        assert_eq!(sanitize_filename(""), "archive");
    }

    #[test]
    fn build_archive_filename_prefixes_message_id_and_sanitizes() {
        let name = build_archive_filename(42, Some("../evil name.zip"), ArchiveKind::Zip);
        assert_eq!(name, "0000000042-.._evil_name.zip");
        assert!(!name.contains('/'));

        let fallback = build_archive_filename(7, None, ArchiveKind::Rar);
        assert_eq!(fallback, "0000000007-archive.rar");
    }

    #[test]
    fn path_helpers_append_expected_suffixes() {
        let archive = Path::new("/tmp/archives/0000000001-sample.zip");
        assert_eq!(
            partial_archive_path(archive),
            Path::new("/tmp/archives/0000000001-sample.zip.part")
        );
        assert_eq!(
            archive_password_path(archive),
            Path::new("/tmp/archives/0000000001-sample.zip.pass")
        );
        assert_eq!(
            archive_needs_password_path(archive),
            Path::new("/tmp/archives/0000000001-sample.zip.needs-password")
        );
    }

    #[test]
    fn find_archive_by_message_id_matches_only_numeric_prefix() {
        let dir = unique_temp_dir("find-by-id");
        sfs::create_dir_all(&dir).unwrap();
        let target = dir.join("0000000123-sample.zip");
        sfs::write(&target, b"data").unwrap();
        sfs::write(dir.join("0000000123-sample.zip.part"), b"data").unwrap();
        sfs::write(dir.join("0000000999-other.zip"), b"data").unwrap();

        assert_eq!(find_archive_by_message_id(&dir, 123), Some(target));
        assert!(find_archive_by_message_id(&dir, 999).is_some());
        assert_eq!(find_archive_by_message_id(&dir, 1), None);

        let _ = sfs::remove_dir_all(&dir);
    }

    #[test]
    fn find_archive_by_message_id_returns_none_for_missing_dir() {
        let dir = unique_temp_dir("missing");
        assert_eq!(find_archive_by_message_id(&dir, 1), None);
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = sfs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, contents) in entries {
            writer.start_file(*name, options).unwrap();
            io::Write::write_all(&mut writer, contents).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn extract_zip_writes_entries_into_output_dir() {
        let dir = unique_temp_dir("extract-ok");
        sfs::create_dir_all(&dir).unwrap();
        let archive_path = dir.join("0000000001-sample.zip");
        write_zip(&archive_path, &[("hello.txt", b"hi there")]);

        let output_root = dir.join("out");
        let stats = extract_archive(&archive_path, &output_root, None).unwrap();

        assert_eq!(stats.files_extracted, 1);
        let extracted = stats.output_dir.join("hello.txt");
        assert_eq!(sfs::read(&extracted).unwrap(), b"hi there");

        let _ = sfs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_zip_defeats_zip_slip_path_traversal() {
        let dir = unique_temp_dir("zip-slip");
        sfs::create_dir_all(&dir).unwrap();
        let archive_path = dir.join("0000000002-evil.zip");
        write_zip(
            &archive_path,
            &[("../../outside.txt", b"should not escape output dir")],
        );

        let output_root = dir.join("out");
        let stats = extract_archive(&archive_path, &output_root, None).unwrap();

        // The traversal components are stripped, so the entry lands inside output_dir
        // under its base name instead of escaping to a parent directory.
        assert!(!dir.join("outside.txt").exists());
        assert!(stats.output_dir.join("outside.txt").exists());

        let _ = sfs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_zip_rejects_archive_exceeding_byte_limit() {
        let dir = unique_temp_dir("zip-bomb");
        sfs::create_dir_all(&dir).unwrap();
        let archive_path = dir.join("0000000003-bomb.zip");
        write_zip(&archive_path, &[("huge.bin", &[0u8; 1024])]);

        let output_dir = dir.join("out");
        // Exercise the same code path as production with a tiny limit instead of
        // allocating a multi-gigabyte payload to trip the real production limit.
        let result = extract_zip(&archive_path, &output_dir, None, 100, MAX_EXTRACTED_ENTRIES);

        assert!(matches!(result, Err(ExtractError::TooLarge)));

        let _ = sfs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_zip_rejects_archive_exceeding_entry_limit() {
        let dir = unique_temp_dir("zip-too-many-entries");
        sfs::create_dir_all(&dir).unwrap();
        let archive_path = dir.join("0000000004-many.zip");
        write_zip(&archive_path, &[("a.bin", b"a"), ("b.bin", b"b"), ("c.bin", b"c")]);

        let output_dir = dir.join("out");
        let result = extract_zip(&archive_path, &output_dir, None, MAX_EXTRACTED_BYTES, 2);

        assert!(matches!(result, Err(ExtractError::TooManyEntries)));

        let _ = sfs::remove_dir_all(&dir);
    }
}
