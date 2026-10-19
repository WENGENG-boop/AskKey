use std::path::Path;

use rusqlite::Connection;

use crate::VaultError;

const MIGRATIONS: &[(&str, &str)] = &[(
    "askkey-windows-0001-baseline",
    "CREATE TABLE credentials (
        id TEXT PRIMARY KEY NOT NULL,
        name_key BLOB NOT NULL UNIQUE,
        permission TEXT NOT NULL,
        meta BLOB NOT NULL,
        secret BLOB NOT NULL,
        expires_at INTEGER,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        tag BLOB NOT NULL
    );
    CREATE TABLE settings (
        key TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    );
    CREATE TABLE access_records (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        at INTEGER NOT NULL,
        record BLOB NOT NULL
    );
    CREATE INDEX access_records_at ON access_records(at);",
)];

pub(crate) fn open_connection(path: &Path) -> Result<Connection, VaultError> {
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA foreign_keys = ON;
         PRAGMA trusted_schema = OFF;",
    )?;
    Ok(connection)
}

pub(crate) fn migrate(connection: &Connection) -> Result<(), VaultError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (id TEXT PRIMARY KEY NOT NULL);",
    )?;
    for (id, statements) in MIGRATIONS {
        let applied: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE id = ?1)",
            [id],
            |row| row.get(0),
        )?;
        if !applied {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute_batch(statements)?;
            transaction.execute("INSERT INTO schema_migrations (id) VALUES (?1)", [id])?;
            transaction.commit()?;
        }
    }
    Ok(())
}

/// Accepts only a library whose migration history is exactly the known one.
pub(crate) fn verify_schema(connection: &Connection) -> Result<(), VaultError> {
    let mut statement = connection
        .prepare("SELECT id FROM schema_migrations ORDER BY rowid")
        .map_err(|_| VaultError::UnsupportedSchema)?;
    let applied: Vec<String> = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let expected: Vec<&str> = MIGRATIONS.iter().map(|(id, _)| *id).collect();
    if applied != expected {
        return Err(VaultError::UnsupportedSchema);
    }
    Ok(())
}
