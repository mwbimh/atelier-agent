use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_SHA256_ALGORITHM, BCryptCloseAlgorithmProvider, BCryptHash, BCryptOpenAlgorithmProvider,
};

const SHA256_LEN: usize = 32;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn runner_source() -> Result<PathBuf> {
    let source = std::env::var_os("ATELIER_SANDBOX_RUNNER")
        .or_else(|| std::env::var_os("ATE_BINARY"))
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(std::env::current_exe)?;
    if !source.is_file() {
        return Err(anyhow!(
            "Atelier Windows sandbox runner source is missing: {}",
            source.display()
        ));
    }
    dunce::canonicalize(&source)
        .with_context(|| format!("canonicalize sandbox runner source {}", source.display()))
}

pub fn materialize(source: &Path, home: &Path) -> Result<PathBuf> {
    let source_hash = sha256_file(source)?;
    let source_hash_hex = hex_digest(&source_hash);
    let destination_dir = crate::setup::sandbox_bin_dir(home);
    std::fs::create_dir_all(&destination_dir)?;
    let destination = destination_dir.join(format!("ate-sandbox-{source_hash_hex}.exe"));
    if destination.exists() {
        verify_hash(&destination, &source_hash)?;
        return Ok(destination);
    }

    let temporary_id = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = destination_dir.join(format!(
        ".ate-sandbox-{}-{}-{}.tmp",
        std::process::id(),
        temporary_id,
        source_hash_hex
    ));
    std::fs::copy(source, &temporary).with_context(|| {
        format!(
            "copy sandbox runner from {} to {}",
            source.display(),
            temporary.display()
        )
    })?;
    if let Err(error) = verify_hash(&temporary, &source_hash) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.context("verify copied sandbox runner before install"));
    }
    match std::fs::rename(&temporary, &destination) {
        Ok(()) => {
            verify_hash(&destination, &source_hash)?;
            Ok(destination)
        }
        Err(_error) if destination.is_file() => {
            let _ = std::fs::remove_file(&temporary);
            verify_hash(&destination, &source_hash)?;
            Ok(destination)
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "install materialized sandbox runner {}",
                destination.display()
            )
        }),
    }
}

fn verify_hash(path: &Path, expected: &[u8; SHA256_LEN]) -> Result<()> {
    let actual = sha256_file(path)?;
    if &actual == expected {
        Ok(())
    } else {
        Err(anyhow!(
            "sandbox runner hash mismatch for {}: expected {}, got {}",
            path.display(),
            hex_digest(expected),
            hex_digest(&actual)
        ))
    }
}

fn sha256_file(path: &Path) -> Result<[u8; SHA256_LEN]> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read sandbox runner for hashing {}", path.display()))?;
    let byte_len = u32::try_from(bytes.len())
        .with_context(|| format!("sandbox runner is too large to hash: {}", path.display()))?;
    let mut algorithm = ptr::null_mut();
    let status = unsafe {
        BCryptOpenAlgorithmProvider(&mut algorithm, BCRYPT_SHA256_ALGORITHM, ptr::null(), 0)
    };
    if status < 0 || algorithm.is_null() {
        return Err(anyhow!(
            "BCryptOpenAlgorithmProvider(SHA256) failed with NTSTATUS {status:#x}"
        ));
    }

    let mut digest = [0u8; SHA256_LEN];
    let status = unsafe {
        BCryptHash(
            algorithm,
            ptr::null(),
            0,
            bytes.as_ptr(),
            byte_len,
            digest.as_mut_ptr(),
            digest.len() as u32,
        )
    };
    let close_status = unsafe { BCryptCloseAlgorithmProvider(algorithm, 0) };
    if status < 0 {
        return Err(anyhow!(
            "BCryptHash(SHA256) failed with NTSTATUS {status:#x}"
        ));
    }
    if close_status < 0 {
        return Err(anyhow!(
            "BCryptCloseAlgorithmProvider failed with NTSTATUS {close_status:#x}"
        ));
    }
    Ok(digest)
}

fn hex_digest(digest: &[u8; SHA256_LEN]) -> String {
    use std::fmt::Write as _;

    let mut value = String::with_capacity(SHA256_LEN * 2);
    for byte in digest {
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    value
}

pub fn remap_program(program: &Path, source: &Path, materialized: &Path) -> PathBuf {
    if same_file(program, source) {
        materialized.to_path_buf()
    } else {
        program.to_path_buf()
    }
}

fn same_file(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (dunce::canonicalize(left), dunce::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{hex_digest, materialize, remap_program, sha256_file};

    #[test]
    fn sha256_uses_the_standard_digest() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.exe");
        std::fs::write(&source, b"abc").unwrap();

        let digest = sha256_file(&source).unwrap();

        assert_eq!(
            hex_digest(&digest),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn embedded_worker_program_is_remapped_to_materialized_runner() {
        let source = std::path::Path::new(r"C:\bin\ate.exe");
        let materialized = std::path::Path::new(r"C:\Users\user\.atelier\.sandbox-bin\ate.exe");
        assert_eq!(remap_program(source, source, materialized), materialized);
        assert_eq!(
            remap_program(
                std::path::Path::new(r"C:\Windows\System32\cmd.exe"),
                source,
                materialized
            ),
            std::path::Path::new(r"C:\Windows\System32\cmd.exe")
        );
    }

    #[test]
    fn materialized_runner_is_written_under_atelier_home() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.exe");
        std::fs::write(&source, b"runner-bytes").unwrap();
        let home = temp.path().join("home");

        let destination = materialize(&source, &home).unwrap();

        assert!(destination.starts_with(home.join(".sandbox-bin")));
        assert_eq!(std::fs::read(destination).unwrap(), b"runner-bytes");
    }

    #[test]
    fn concurrent_materialization_does_not_share_a_temporary_path() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.exe");
        std::fs::write(&source, vec![0x5a; 8 * 1024 * 1024]).unwrap();
        let home = temp.path().join("home");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));

        let workers = (0..12)
            .map(|_| {
                let source = source.clone();
                let home = home.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    materialize(&source, &home)
                })
            })
            .collect::<Vec<_>>();
        let destinations = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Result<Vec<_>, _>>()
            .expect("concurrent materialization");

        assert!(
            destinations
                .iter()
                .all(|destination| destination == &destinations[0])
        );
        assert_eq!(
            std::fs::read(&destinations[0]).unwrap(),
            std::fs::read(&source).unwrap()
        );
    }

    #[test]
    fn corrupted_materialized_runner_fails_closed_even_when_size_matches() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.exe");
        std::fs::write(&source, b"runner-good").unwrap();
        let home = temp.path().join("home");
        let destination = materialize(&source, &home).unwrap();
        std::fs::write(&destination, b"runner-evil").unwrap();

        let error = materialize(&source, &home).expect_err("hash mismatch must fail closed");

        assert!(error.to_string().contains("hash mismatch"), "{error:#}");
        assert_eq!(std::fs::read(destination).unwrap(), b"runner-evil");
    }
}
