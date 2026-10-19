//! Opening or creating the library. Mirrors the fail-closed rules of the
//! macOS edition (its ADR 0001): an existing key requires its database, an
//! existing database requires its key, and only a provably unfinished first
//! creation is resumed.

use std::fs;
use std::path::Path;

use rusqlite::Connection;
use zeroize::Zeroizing;

use crate::crypto::{VaultKeys, KEY_BYTES};
use crate::storage::{migrate, open_connection, verify_schema};
use crate::VaultError;

pub(crate) const DATABASE: &str = "credentials.db";
const CREATING: &str = "credentials.db.creating";
const KEY: &str = "vault.key";
const PENDING_KEY: &str = "vault.key.pending";

pub(crate) struct Opened {
    pub connection: Connection,
    pub keys: VaultKeys,
}

fn read_key(path: &Path, entropy: &[u8]) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    let sealed = fs::read(path)?;
    let key = Zeroizing::new(
        askkey_system::unprotect(&sealed, entropy).map_err(|_| VaultError::Authentication)?,
    );
    if key.len() != KEY_BYTES {
        return Err(VaultError::Authentication);
    }
    Ok(key)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), VaultError> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    askkey_system::protect_private_path(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn remove_creation_files(directory: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = fs::remove_file(directory.join(format!("{CREATING}{suffix}")));
    }
}

/// Opens the library in `directory`, creating it when neither key nor
/// database exists. `key_namespace` is the DPAPI entropy, which keeps
/// development and production keys apart.
pub(crate) fn open_library(directory: &Path, key_namespace: &str) -> Result<Opened, VaultError> {
    fs::create_dir_all(directory)?;
    askkey_system::protect_private_path(directory)?;
    let entropy = key_namespace.as_bytes();
    let database = directory.join(DATABASE);
    let key_path = directory.join(KEY);
    let pending_path = directory.join(PENDING_KEY);

    if key_path.exists() {
        if !database.is_file() {
            return Err(VaultError::MissingDatabase);
        }
        let keys = VaultKeys::new(&read_key(&key_path, entropy)?)?;
        let connection = open_connection(&database)?;
        verify_schema(&connection)?;
        askkey_system::protect_private_path(&database)?;
        let _ = fs::remove_file(&pending_path);
        return Ok(Opened { connection, keys });
    }
    if database.exists() {
        // A crash between publishing the database and promoting the key
        // leaves an empty library and its pending key: finish that creation.
        // Any other database without its key is never replaced or adopted.
        if pending_path.exists() {
            let key = read_key(&pending_path, entropy)?;
            let connection = open_connection(&database)?;
            verify_schema(&connection)?;
            let count: i64 =
                connection.query_row("SELECT COUNT(*) FROM credentials", [], |row| row.get(0))?;
            if count == 0 {
                fs::rename(&pending_path, &key_path)?;
                return Ok(Opened {
                    connection,
                    keys: VaultKeys::new(&key)?,
                });
            }
        }
        return Err(VaultError::MissingKey);
    }

    let key = if pending_path.exists() {
        read_key(&pending_path, entropy)?
    } else {
        let key = VaultKeys::generate_key();
        let sealed = askkey_system::protect(&key, entropy)?;
        write_private(&pending_path, &sealed)?;
        key
    };

    remove_creation_files(directory);
    let creating = directory.join(CREATING);
    {
        let connection = open_connection(&creating)?;
        migrate(&connection)?;
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    }
    askkey_system::protect_private_path(&creating)?;
    fs::OpenOptions::new()
        .write(true)
        .open(&creating)?
        .sync_all()?;
    // Publish the database, then promote the key. A crash in between is
    // finished by the empty-library branch above.
    fs::rename(&creating, &database)?;
    fs::rename(&pending_path, &key_path)?;
    remove_creation_files(directory);

    let keys = VaultKeys::new(&key)?;
    let connection = open_connection(&database)?;
    verify_schema(&connection)?;
    Ok(Opened { connection, keys })
}
