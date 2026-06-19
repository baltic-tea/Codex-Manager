ALTER TABLE account_proxy_settings ADD COLUMN proxy_source TEXT;
ALTER TABLE account_proxy_settings ADD COLUMN proxy_profile_id TEXT;
UPDATE account_proxy_settings
SET proxy_source = 'custom'
WHERE (proxy_source IS NULL OR TRIM(proxy_source) = '')
  AND proxy_url IS NOT NULL
  AND TRIM(proxy_url) <> '';
CREATE INDEX IF NOT EXISTS idx_account_proxy_settings_proxy_profile_id
    ON account_proxy_settings(proxy_profile_id);
