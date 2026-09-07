//! Versioned authenticated credential files; no external keyring or runtime library.
use super::*;
use chacha20poly1305::{aead::{Aead, KeyInit, Payload}, XChaCha20Poly1305, XNonce};

const MAGIC: &[u8] = b"CRPKEY\x00\x01";
const MAX_FILE: u64 = 16432; // header + 24-byte nonce + 16384-byte key + 16-byte tag

fn users_dir() -> Result<PathBuf> {
    let home = env::var_os("HOME").ok_or("HOME is not set")?;
    let dir = PathBuf::from(home).join(".config/codex-rate-proxy/users");
    private_dir(&dir)?;
    Ok(dir)
}

fn profile_path(name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.len() > 64 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        return Err("user name must be 1-64 ASCII letters, digits, underscores or hyphens".into());
    }
    Ok(users_dir()?.join(format!("{name}.key")))
}

fn registry_lock(dir: &Path) -> Result<File> {
    let file = private_file(&dir.join("register.lock"), false)?;
    lock(&file, false)?;
    Ok(file)
}

fn read_private(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o777 != 0o600 {
        return Err("credential/master key must be a regular file with permissions 600".into());
    }
    if metadata.len() > limit { return Err("credential/master key file is too large".into()); }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit { return Err("credential/master key file is too large".into()); }
    Ok(bytes)
}

// Caller holds the registry lock, shared by registration, reads, migration and removal.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().ok_or("invalid credential directory")?;
    let temporary = dir.join(format!(".{}.tmp", random_id()?));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_file(temporary); }
    result
}

fn master_key(users: &Path, create: bool) -> Result<[u8; 32]> {
    let home = env::var_os("HOME").ok_or("HOME is not set")?;
    let dir = PathBuf::from(home).join(".local/share/codex-rate-proxy");
    private_dir(&dir)?;
    let path = dir.join("master.key");
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            let bytes = read_private(&path, 32)?;
            bytes.try_into().map_err(|_| "invalid master key length; restore the original master.key".into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if !create { return Err("master.key is missing; restore it to decrypt registered keys".into()); }
            // Never silently replace a lost master key while encrypted profiles remain.
            for entry in fs::read_dir(users)? {
                let p = entry?.path();
                if p.extension().is_some_and(|ext| ext == "key") {
                    let bytes = read_private(&p, MAX_FILE)?;
                    if bytes.starts_with(b"CRPKEY") {
                        return Err("master.key is missing but encrypted users remain; restore it or unregister those users before registering again".into());
                    }
                }
            }
            let mut bytes = [0u8; 32];
            File::open("/dev/urandom")?.read_exact(&mut bytes)?;
            atomic_write(&path, &bytes)?;
            Ok(bytes)
        }
        Err(error) => Err(error.into()),
    }
}

fn aad(name: &str) -> String { format!("codex-rate-proxy:credentials:v1:{name}") }

fn seal(name: &str, key: &str, master: &[u8; 32]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(master.into());
    let mut nonce = [0u8; 24];
    File::open("/dev/urandom")?.read_exact(&mut nonce)?;
    let encrypted = cipher.encrypt(XNonce::from_slice(&nonce), Payload { msg: key.as_bytes(), aad: aad(name).as_bytes() })
        .map_err(|_| "credential encryption failed")?;
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&nonce);
    bytes.extend_from_slice(&encrypted);
    Ok(bytes)
}

fn unseal(name: &str, bytes: &[u8], master: &[u8; 32]) -> Result<String> {
    if !bytes.starts_with(MAGIC) || bytes.len() < MAGIC.len() + 24 + 16 {
        return Err("unsupported or damaged encrypted credential file".into());
    }
    let cipher = XChaCha20Poly1305::new(master.into());
    let plain = cipher.decrypt(XNonce::from_slice(&bytes[MAGIC.len()..MAGIC.len()+24]),
        Payload { msg: &bytes[MAGIC.len()+24..], aad: aad(name).as_bytes() })
        .map_err(|_| "credential authentication failed: file/name/master key changed")?;
    let key = String::from_utf8(plain).map_err(|_| "decrypted credential is invalid")?;
    validate_key(&key)?;
    Ok(key)
}

fn read_or_migrate(name: &str, path: &Path, users: &Path) -> Result<String> {
    let bytes = read_private(path, MAX_FILE)?;
    if bytes.starts_with(b"CRPKEY") {
        return unseal(name, &bytes, &master_key(users, false)?);
    }
    let key = std::str::from_utf8(&bytes).map_err(|_| "invalid legacy credential file")?
        .trim_end_matches(['\r', '\n']);
    validate_key(key)?;
    let encrypted = seal(name, key, &master_key(users, true)?)?;
    atomic_write(path, &encrypted)?;
    eprintln!("Encrypted legacy registration: {name}");
    Ok(key.to_owned())
}

pub(super) fn registered_key(name: &str) -> Result<String> {
    let path = profile_path(name)?;
    let dir = path.parent().ok_or("invalid profile directory")?;
    let _guard = registry_lock(dir)?;
    read_or_migrate(name, &path, dir)
}

pub(super) fn register(name: &str, source: Option<(&str, &str)>, replace: bool) -> Result<()> {
    let path = profile_path(name)?;
    let dir = path.parent().ok_or("invalid profile directory")?;
    let _guard = registry_lock(dir)?;
    match fs::symlink_metadata(&path) {
        Ok(_) if !replace => return Err("user already registered; use --replace to update its key".into()),
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => (),
    }
    let key = read_key(source.or(Some(("ask", ""))))?;
    atomic_write(&path, &seal(name, &key, &master_key(dir, true)?)?)?;
    println!("registered {name} (encrypted); launch with --user {name}");
    Ok(())
}

pub(super) fn unregister(name: &str) -> Result<()> {
    let path = profile_path(name)?;
    let dir = path.parent().ok_or("invalid profile directory")?;
    let _guard = registry_lock(dir)?;
    match fs::remove_file(&path) {
        Ok(()) => {
            File::open(dir)?.sync_all()?;
            println!("unregistered {name}; existing sessions/proxies are unchanged");
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => println!("user {name} is not registered"),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(super) fn encrypt_all() -> Result<()> {
    let dir = users_dir()?;
    let _guard = registry_lock(&dir)?;
    let mut count = 0;
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "key") {
            let name = path.file_stem().and_then(|n| n.to_str()).ok_or("invalid registered user name")?;
            let checked = profile_path(name)?;
            read_or_migrate(name, &checked, &dir)?;
            count += 1;
        }
    }
    println!("verified encrypted registrations: {count}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authenticated_encryption_binds_name_and_rejects_tampering() {
        let master = [7u8; 32];
        let bytes = seal("alice", "fake-api-key", &master).unwrap();
        assert_eq!(unseal("alice", &bytes, &master).unwrap(), "fake-api-key");
        assert_ne!(bytes, seal("alice", "fake-api-key", &master).unwrap());
        assert!(unseal("bob", &bytes, &master).is_err());
        assert!(unseal("alice", &bytes, &[8u8; 32]).is_err());
        let mut corrupt = bytes.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(unseal("alice", &corrupt, &master).is_err());
        assert!(unseal("alice", &bytes[..20], &master).is_err());
    }
}
