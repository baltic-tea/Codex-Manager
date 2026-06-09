use codexmanager_core::storage::{now_ts, AccountProxySettings, Storage};
use serde::Serialize;

use crate::storage_helpers::{open_storage, StorageHandle};

pub(crate) const STATUS_NOT_CONFIGURED: &str = "not_configured";
pub(crate) const STATUS_UNCHECKED: &str = "unchecked";
pub(crate) const STATUS_CHECKING: &str = "checking";
pub(crate) const STATUS_INVALID_URL: &str = "invalid_url";
const LOCAL_PROXY_EXPECTED_MESSAGE: &str = "Codex-Manager expects a local HTTP/SOCKS proxy URL. Start sing-box separately and paste the local mixed inbound address, for example http://127.0.0.1:7891.";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountProxySettingsResponse {
    pub account_id: String,
    pub enabled: bool,
    pub proxy_url: String,
    pub status: String,
    pub latency_ms: Option<i64>,
    pub last_check_at: Option<i64>,
    pub last_error: Option<String>,
}

pub(crate) fn get_account_proxy_settings(
    account_id: &str,
) -> Result<AccountProxySettingsResponse, String> {
    let storage = open_storage_for_account(account_id)?;
    let account_id = normalize_account_id(account_id)?;
    ensure_account_exists(&storage, account_id)?;
    read_or_default_response(&storage, account_id)
}

pub(crate) fn set_account_proxy_settings(
    account_id: &str,
    enabled: bool,
    proxy_url: Option<&str>,
) -> Result<AccountProxySettingsResponse, String> {
    let storage = open_storage_for_account(account_id)?;
    let account_id = normalize_account_id(account_id)?;
    ensure_account_exists(&storage, account_id)?;

    let normalized_proxy_url = normalize_proxy_url_for_setting(enabled, proxy_url)?;
    let status = if enabled {
        STATUS_UNCHECKED
    } else {
        STATUS_NOT_CONFIGURED
    };
    storage
        .upsert_account_proxy_settings(
            account_id,
            enabled,
            normalized_proxy_url.as_deref(),
            status,
            None,
            None,
            None,
        )
        .map_err(|err| format!("store account proxy settings failed: {err}"))?;
    crate::gateway::invalidate_account_proxy_cache(account_id);
    read_or_default_response(&storage, account_id)
}

pub(crate) fn clear_account_proxy_settings(
    account_id: &str,
) -> Result<AccountProxySettingsResponse, String> {
    let storage = open_storage_for_account(account_id)?;
    let account_id = normalize_account_id(account_id)?;
    ensure_account_exists(&storage, account_id)?;
    storage
        .clear_account_proxy_settings(account_id)
        .map_err(|err| format!("clear account proxy settings failed: {err}"))?;
    crate::gateway::invalidate_account_proxy_cache(account_id);
    Ok(default_response(account_id))
}

pub(crate) fn test_account_proxy_settings(
    account_id: &str,
) -> Result<AccountProxySettingsResponse, String> {
    let storage = open_storage_for_account(account_id)?;
    let account_id = normalize_account_id(account_id)?;
    ensure_account_exists(&storage, account_id)?;
    test_account_proxy_settings_with_checker(&storage, account_id, |proxy_url| {
        crate::account::proxy_health::check_account_proxy(proxy_url)
    })
}

fn open_storage_for_account(account_id: &str) -> Result<StorageHandle, String> {
    normalize_account_id(account_id)?;
    open_storage().ok_or_else(|| "storage unavailable".to_string())
}

fn normalize_account_id(account_id: &str) -> Result<&str, String> {
    let trimmed = account_id.trim();
    if trimmed.is_empty() {
        Err("missing accountId".to_string())
    } else {
        Ok(trimmed)
    }
}

fn ensure_account_exists(storage: &Storage, account_id: &str) -> Result<(), String> {
    let found = storage
        .find_account_by_id(account_id)
        .map_err(|err| format!("read account failed: {err}"))?
        .is_some();
    if found {
        Ok(())
    } else {
        Err("account not found".to_string())
    }
}

fn normalize_proxy_url_for_setting(
    enabled: bool,
    proxy_url: Option<&str>,
) -> Result<Option<String>, String> {
    let trimmed = proxy_url.map(str::trim).filter(|value| !value.is_empty());
    if !enabled {
        return Ok(trimmed.map(ToString::to_string));
    }
    let Some(proxy_url) = trimmed else {
        return Err("proxyUrl is required when account proxy is enabled".to_string());
    };
    normalize_supported_proxy_url(proxy_url).map(Some)
}

pub(crate) fn normalize_supported_proxy_url(proxy_url: &str) -> Result<String, String> {
    let parsed = url::Url::parse(proxy_url)
        .map_err(|err| format!("invalid proxyUrl: {err}. {LOCAL_PROXY_EXPECTED_MESSAGE}"))?;
    match parsed.scheme() {
        "http" | "https" | "socks5h" => Ok(proxy_url.trim().to_string()),
        "socks5" => Ok(format!("socks5h:{}", &proxy_url.trim()["socks5:".len()..])),
        "vless" | "trojan" | "ss" | "hysteria2" => Err(LOCAL_PROXY_EXPECTED_MESSAGE.to_string()),
        other => Err(format!(
            "unsupported proxy URL scheme: {other}. {LOCAL_PROXY_EXPECTED_MESSAGE}"
        )),
    }
}

fn test_account_proxy_settings_with_checker<F>(
    storage: &Storage,
    account_id: &str,
    checker: F,
) -> Result<AccountProxySettingsResponse, String>
where
    F: FnOnce(&str) -> crate::account::proxy_health::ProxyHealthCheckResult,
{
    let settings = storage
        .find_account_proxy_settings(account_id)
        .map_err(|err| format!("read account proxy settings failed: {err}"))?;
    let Some(settings) = settings else {
        return Ok(default_response(account_id));
    };

    let proxy_url = settings
        .proxy_url
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    if !settings.enabled || proxy_url.is_empty() {
        persist_check_status(storage, account_id, STATUS_NOT_CONFIGURED, None, None)?;
        crate::gateway::invalidate_account_proxy_cache(account_id);
        return read_or_default_response(storage, account_id);
    }

    let normalized_proxy_url = match normalize_supported_proxy_url(proxy_url) {
        Ok(normalized_proxy_url) => normalized_proxy_url,
        Err(err) => {
            persist_check_status(
                storage,
                account_id,
                STATUS_INVALID_URL,
                None,
                Some(err.as_str()),
            )?;
            crate::gateway::invalidate_account_proxy_cache(account_id);
            return read_or_default_response(storage, account_id);
        }
    };

    if normalized_proxy_url != proxy_url {
        storage
            .upsert_account_proxy_settings(
                account_id,
                settings.enabled,
                Some(normalized_proxy_url.as_str()),
                settings.status.as_str(),
                settings.latency_ms,
                settings.last_check_at,
                settings.last_error.as_deref(),
            )
            .map_err(|err| format!("store account proxy settings failed: {err}"))?;
    }

    persist_check_status(storage, account_id, STATUS_CHECKING, None, None)?;
    let outcome = checker(normalized_proxy_url.as_str());
    persist_check_status(
        storage,
        account_id,
        outcome.status,
        outcome.latency_ms,
        outcome.last_error.as_deref(),
    )?;
    crate::gateway::invalidate_account_proxy_cache(account_id);
    read_or_default_response(storage, account_id)
}

fn persist_check_status(
    storage: &Storage,
    account_id: &str,
    status: &str,
    latency_ms: Option<i64>,
    last_error: Option<&str>,
) -> Result<(), String> {
    storage
        .update_account_proxy_check_status(
            account_id,
            status,
            latency_ms,
            Some(now_ts()),
            last_error,
        )
        .map_err(|err| format!("update account proxy status failed: {err}"))
}

fn read_or_default_response(
    storage: &Storage,
    account_id: &str,
) -> Result<AccountProxySettingsResponse, String> {
    let settings = storage
        .find_account_proxy_settings(account_id)
        .map_err(|err| format!("read account proxy settings failed: {err}"))?;
    Ok(settings
        .map(account_proxy_settings_response)
        .unwrap_or_else(|| default_response(account_id)))
}

fn default_response(account_id: &str) -> AccountProxySettingsResponse {
    AccountProxySettingsResponse {
        account_id: account_id.to_string(),
        enabled: false,
        proxy_url: String::new(),
        status: STATUS_NOT_CONFIGURED.to_string(),
        latency_ms: None,
        last_check_at: None,
        last_error: None,
    }
}

fn account_proxy_settings_response(settings: AccountProxySettings) -> AccountProxySettingsResponse {
    AccountProxySettingsResponse {
        account_id: settings.account_id,
        enabled: settings.enabled,
        proxy_url: settings.proxy_url.unwrap_or_default(),
        status: settings.status,
        latency_ms: settings.latency_ms,
        last_check_at: settings.last_check_at,
        last_error: settings.last_error,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_supported_proxy_url, test_account_proxy_settings_with_checker,
        AccountProxySettingsResponse, STATUS_CHECKING, STATUS_INVALID_URL, STATUS_NOT_CONFIGURED,
        STATUS_UNCHECKED,
    };
    use codexmanager_core::storage::{now_ts, Account, Storage};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ACCOUNT_PROXY_TEST_DIR_SEQ: AtomicUsize = AtomicUsize::new(0);
    const STATUS_FAILED: &str = "failed";
    const STATUS_OK: &str = "ok";

    #[test]
    fn normalize_supported_proxy_url_rewrites_socks5_to_socks5h() {
        assert_eq!(
            normalize_supported_proxy_url("socks5://127.0.0.1:7891").expect("normalize"),
            "socks5h://127.0.0.1:7891"
        );
    }

    #[test]
    fn normalize_supported_proxy_url_rejects_server_links() {
        let err = normalize_supported_proxy_url("vless://example").expect_err("reject vless");
        assert!(err.contains("local HTTP/SOCKS proxy URL"));
    }

    #[test]
    fn test_account_proxy_settings_returns_not_configured_for_disabled_proxy() {
        let dir = new_test_dir("account-proxy-disabled");
        let storage = seed_storage(&dir, "acc-disabled");
        storage
            .upsert_account_proxy_settings(
                "acc-disabled",
                false,
                Some("http://127.0.0.1:7891"),
                STATUS_CHECKING,
                Some(42),
                Some(100),
                Some("stale"),
            )
            .expect("seed disabled proxy settings");

        let response = test_account_proxy_settings_with_checker(&storage, "acc-disabled", |_| {
            panic!("checker should not run for disabled proxy")
        })
        .expect("test disabled proxy");

        assert_status(&response, STATUS_NOT_CONFIGURED);
        assert_eq!(response.latency_ms, None);
        assert_eq!(response.last_error, None);
        assert!(response.last_check_at.is_some());

        let stored = storage
            .find_account_proxy_settings("acc-disabled")
            .expect("find stored disabled proxy")
            .expect("stored disabled proxy");
        assert_eq!(stored.status, STATUS_NOT_CONFIGURED);
        assert_eq!(stored.latency_ms, None);
        assert_eq!(stored.last_error, None);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_account_proxy_settings_persists_invalid_url_status() {
        let dir = new_test_dir("account-proxy-invalid");
        let storage = seed_storage(&dir, "acc-invalid");
        storage
            .upsert_account_proxy_settings(
                "acc-invalid",
                true,
                Some("http://"),
                STATUS_UNCHECKED,
                None,
                None,
                None,
            )
            .expect("seed invalid proxy settings");

        let response = test_account_proxy_settings_with_checker(&storage, "acc-invalid", |_| {
            panic!("checker should not run for invalid proxy URL")
        })
        .expect("test invalid proxy");

        assert_status(&response, STATUS_INVALID_URL);
        assert!(response
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("invalid proxyUrl"));
        assert!(response.last_check_at.is_some());

        let stored = storage
            .find_account_proxy_settings("acc-invalid")
            .expect("find stored invalid proxy")
            .expect("stored invalid proxy");
        assert_eq!(stored.status, STATUS_INVALID_URL);
        assert!(stored
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("invalid proxyUrl"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_account_proxy_settings_persists_checker_outcome() {
        let dir = new_test_dir("account-proxy-ok");
        let storage = seed_storage(&dir, "acc-ok");
        storage
            .upsert_account_proxy_settings(
                "acc-ok",
                true,
                Some("http://127.0.0.1:7891"),
                STATUS_UNCHECKED,
                None,
                None,
                None,
            )
            .expect("seed valid proxy settings");

        let response = test_account_proxy_settings_with_checker(&storage, "acc-ok", |_| {
            crate::account::proxy_health::ProxyHealthCheckResult {
                status: STATUS_OK,
                latency_ms: Some(184),
                last_error: None,
            }
        })
        .expect("test proxy success");

        assert_status(&response, STATUS_OK);
        assert_eq!(response.latency_ms, Some(184));
        assert_eq!(response.last_error, None);
        assert!(response.last_check_at.is_some());

        let response = test_account_proxy_settings_with_checker(&storage, "acc-ok", |_| {
            crate::account::proxy_health::ProxyHealthCheckResult {
                status: STATUS_FAILED,
                latency_ms: None,
                last_error: Some("proxy unreachable".to_string()),
            }
        })
        .expect("test proxy failure");

        assert_status(&response, STATUS_FAILED);
        assert_eq!(response.latency_ms, None);
        assert_eq!(response.last_error.as_deref(), Some("proxy unreachable"));

        let stored = storage
            .find_account_proxy_settings("acc-ok")
            .expect("find stored proxy")
            .expect("stored proxy");
        assert_eq!(stored.status, STATUS_FAILED);
        assert_eq!(stored.latency_ms, None);
        assert_eq!(stored.last_error.as_deref(), Some("proxy unreachable"));

        let _ = fs::remove_dir_all(dir);
    }

    fn assert_status(response: &AccountProxySettingsResponse, expected: &str) {
        assert_eq!(response.status, expected);
    }

    fn new_test_dir(prefix: &str) -> PathBuf {
        let seq = ACCOUNT_PROXY_TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let mut dir = std::env::temp_dir();
        dir.push(format!("{prefix}-{}-{seq}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    fn seed_storage(dir: &PathBuf, account_id: &str) -> Storage {
        let storage = Storage::open(dir.join("codexmanager.db")).expect("open test db");
        storage.init().expect("init test schema");
        let now = now_ts();
        storage
            .insert_account(&Account {
                id: account_id.to_string(),
                label: account_id.to_string(),
                issuer: "https://auth.openai.com".to_string(),
                chatgpt_account_id: Some(format!("chatgpt-{account_id}")),
                workspace_id: Some(format!("workspace-{account_id}")),
                group_name: None,
                sort: 0,
                status: "active".to_string(),
                created_at: now,
                updated_at: now,
            })
            .expect("insert account");
        storage
    }
}
