use rusqlite::{Result, Row};

use super::{now_ts, AccountProxySettings, Storage};

impl Storage {
    pub fn upsert_account_proxy_settings(
        &self,
        account_id: &str,
        enabled: bool,
        proxy_url: Option<&str>,
        status: &str,
        latency_ms: Option<i64>,
        last_check_at: Option<i64>,
        last_error: Option<&str>,
    ) -> Result<()> {
        let now = now_ts();
        let created_at = self
            .find_account_proxy_settings(account_id)?
            .map(|settings| settings.created_at)
            .unwrap_or(now);

        self.conn.execute(
            "INSERT INTO account_proxy_settings (
                account_id,
                enabled,
                proxy_url,
                status,
                latency_ms,
                last_check_at,
                last_error,
                created_at,
                updated_at
            ) VALUES (
                ?1,
                ?2,
                ?3,
                ?4,
                ?5,
                ?6,
                ?7,
                ?8,
                ?9
            )
            ON CONFLICT(account_id) DO UPDATE SET
                enabled = excluded.enabled,
                proxy_url = excluded.proxy_url,
                status = excluded.status,
                latency_ms = excluded.latency_ms,
                last_check_at = excluded.last_check_at,
                last_error = excluded.last_error,
                updated_at = excluded.updated_at",
            (
                account_id,
                if enabled { 1 } else { 0 },
                normalize_optional_text(proxy_url),
                normalize_status(status),
                latency_ms,
                last_check_at,
                normalize_optional_text(last_error),
                created_at,
                now,
            ),
        )?;
        Ok(())
    }

    pub fn update_account_proxy_check_status(
        &self,
        account_id: &str,
        status: &str,
        latency_ms: Option<i64>,
        last_check_at: Option<i64>,
        last_error: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE account_proxy_settings
             SET status = ?2,
                 latency_ms = ?3,
                 last_check_at = ?4,
                 last_error = ?5,
                 updated_at = ?6
             WHERE account_id = ?1",
            (
                account_id,
                normalize_status(status),
                latency_ms,
                last_check_at,
                normalize_optional_text(last_error),
                now_ts(),
            ),
        )?;
        Ok(())
    }

    pub fn clear_account_proxy_settings(&self, account_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM account_proxy_settings WHERE account_id = ?1",
            [account_id],
        )?;
        Ok(())
    }

    pub fn find_account_proxy_settings(
        &self,
        account_id: &str,
    ) -> Result<Option<AccountProxySettings>> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, enabled, proxy_url, status, latency_ms, last_check_at, last_error, created_at, updated_at
             FROM account_proxy_settings
             WHERE account_id = ?1
             LIMIT 1",
        )?;
        let mut rows = stmt.query([account_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(map_account_proxy_settings_row(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn list_account_proxy_settings(&self) -> Result<Vec<AccountProxySettings>> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, enabled, proxy_url, status, latency_ms, last_check_at, last_error, created_at, updated_at
             FROM account_proxy_settings
             ORDER BY updated_at DESC, account_id ASC",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(map_account_proxy_settings_row(row)?);
        }
        Ok(out)
    }

    pub(super) fn ensure_account_proxy_settings_table(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS account_proxy_settings (
                account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
                enabled INTEGER NOT NULL DEFAULT 0,
                proxy_url TEXT,
                status TEXT NOT NULL DEFAULT 'unchecked',
                latency_ms INTEGER,
                last_check_at INTEGER,
                last_error TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_account_proxy_settings_updated_at
                ON account_proxy_settings(updated_at DESC, account_id ASC);",
        )?;
        self.ensure_column(
            "account_proxy_settings",
            "enabled",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        self.ensure_column("account_proxy_settings", "proxy_url", "TEXT")?;
        self.ensure_column(
            "account_proxy_settings",
            "status",
            "TEXT NOT NULL DEFAULT 'unchecked'",
        )?;
        self.ensure_column("account_proxy_settings", "latency_ms", "INTEGER")?;
        self.ensure_column("account_proxy_settings", "last_check_at", "INTEGER")?;
        self.ensure_column("account_proxy_settings", "last_error", "TEXT")?;
        self.ensure_column(
            "account_proxy_settings",
            "created_at",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        self.ensure_column(
            "account_proxy_settings",
            "updated_at",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        Ok(())
    }
}

fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

fn normalize_status(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "unchecked".to_string()
    } else {
        trimmed.to_string()
    }
}

fn map_account_proxy_settings_row(row: &Row<'_>) -> Result<AccountProxySettings> {
    Ok(AccountProxySettings {
        account_id: row.get(0)?,
        enabled: row.get::<_, i64>(1)? != 0,
        proxy_url: row.get(2)?,
        status: row.get(3)?,
        latency_ms: row.get(4)?,
        last_check_at: row.get(5)?,
        last_error: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}
