//! SHA-256 integrity helpers.
//!
//! Mirrors `Pathname#verify_checksum` (`extend/pathname.rb:227`) and `Checksum` semantics: the
//! expected and actual digests are compared as lowercase hex strings. See `specs/download-ghcr.md`
//! §9.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{FerroError, Result};

/// Lowercase hex SHA-256 of the file at `path`, streamed so large bottles never load fully.
///
/// Equivalent to `Digest::SHA256.file(path).hexdigest.downcase`.
pub fn sha256_file(path: &Path) -> Result<String> {
    let file = File::open(path).map_err(|e| FerroError::io(path, e))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| FerroError::io(path, e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// Lowercase hex SHA-256 of an in-memory byte slice.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
}

/// Verify `path`'s SHA-256 equals `expected` (case-insensitive), erroring with
/// [`FerroError::ChecksumMismatch`] otherwise. Returns the computed lowercase-hex digest on success.
pub fn verify_sha256(path: &Path, expected: &str) -> Result<String> {
    let actual = sha256_file(path)?;
    if actual.eq_ignore_ascii_case(expected) {
        Ok(actual)
    } else {
        Err(FerroError::ChecksumMismatch {
            expected: expected.to_ascii_lowercase(),
            actual,
        })
    }
}

/// Encode bytes as a lowercase hex string without pulling in an extra crate.
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Known SHA-256 vectors (FIPS 180-2 / NIST examples).
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn sha256_bytes_matches_known_vectors() {
        assert_eq!(sha256_bytes(b""), EMPTY);
        assert_eq!(sha256_bytes(b"abc"), ABC);
    }

    #[test]
    fn sha256_file_matches_known_vectors() {
        let dir = unique_temp_dir("integrity-vectors");
        std::fs::create_dir_all(&dir).unwrap();

        let empty = dir.join("empty");
        File::create(&empty).unwrap();
        assert_eq!(sha256_file(&empty).unwrap(), EMPTY);

        let abc = dir.join("abc");
        File::create(&abc).unwrap().write_all(b"abc").unwrap();
        assert_eq!(sha256_file(&abc).unwrap(), ABC);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sha256_file_handles_chunk_boundary() {
        // Larger than the 64 KiB read buffer to exercise multi-chunk hashing.
        let dir = unique_temp_dir("integrity-large");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("blob");
        let data = vec![0xABu8; 200 * 1024];
        File::create(&path).unwrap().write_all(&data).unwrap();
        assert_eq!(sha256_file(&path).unwrap(), sha256_bytes(&data));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sha256_file_missing_path_is_io_error() {
        let err = sha256_file(Path::new("/nonexistent/ferrobrew/blob")).unwrap_err();
        assert!(matches!(err, FerroError::Io { .. }));
    }

    #[test]
    fn verify_sha256_is_case_insensitive_and_reports_mismatch() {
        let dir = unique_temp_dir("integrity-verify");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abc");
        File::create(&path).unwrap().write_all(b"abc").unwrap();

        assert_eq!(verify_sha256(&path, &ABC.to_uppercase()).unwrap(), ABC);

        let err = verify_sha256(&path, EMPTY).unwrap_err();
        match err {
            FerroError::ChecksumMismatch { expected, actual } => {
                assert_eq!(expected, EMPTY);
                assert_eq!(actual, ABC);
            }
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ferrobrew-{label}-{}-{nanos}", std::process::id()))
    }
}
