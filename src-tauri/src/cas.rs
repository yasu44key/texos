use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Root of the content-addressed store: <app_data>/texos/cas/
pub fn store_root(app_data: &Path) -> PathBuf {
    app_data.join("cas")
}

/// Compute SHA-256 of bytes and return lowercase hex string.
pub fn hash_bytes(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

/// Compute SHA-256 of a file without loading it all into memory.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// Object path within the CAS: <root>/<first2>/<rest>
pub fn object_path(root: &Path, hash: &str) -> PathBuf {
    root.join(&hash[..2]).join(&hash[2..])
}

/// Copy a file into the CAS. Returns its SHA-256 hash.
/// If the object already exists (same hash) it is not copied again.
pub fn store_file(root: &Path, src: &Path) -> std::io::Result<String> {
    let hash = hash_file(src)?;
    let dest = object_path(root, &hash);
    if !dest.exists() {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, &dest)?;
    }
    Ok(hash)
}

/// Store raw bytes into the CAS. Returns SHA-256 hash.
pub fn store_bytes(root: &Path, data: &[u8]) -> std::io::Result<String> {
    let hash = hash_bytes(data);
    let dest = object_path(root, &hash);
    if !dest.exists() {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, data)?;
    }
    Ok(hash)
}

/// Read object bytes from CAS.
pub fn read_object(root: &Path, hash: &str) -> std::io::Result<Vec<u8>> {
    fs::read(object_path(root, hash))
}

/// Check whether an object exists in the CAS.
pub fn exists(root: &Path, hash: &str) -> bool {
    object_path(root, hash).exists()
}
