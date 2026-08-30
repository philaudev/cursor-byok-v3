//! Persists application settings.
use serde::{Deserialize, Serialize};

use crate::Result;

use super::{now_ms, Store};

const PORT_SETTINGS_KEY: &str = "network_ports";
const PROXY_SETTINGS_KEY: &str = "outbound_proxy";
const TAB_SETTINGS_KEY: &str = "cursor_tab";
const INSTALLATION_ID_KEY: &str = "installation_id";
const DESKTOP_SETTINGS_KEY: &str = "desktop_lifecycle";

pub const PUBLIC_TAB_SERVICE_URL: &str = "https://tab.leokun.cn";

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct PortSettings {
    pub proxy_port: u16,
    pub service_port: u16,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    #[default]
    System,
    Custom,
}

impl ProxyMode {
    pub fn is_custom(self) -> bool {
        self == Self::Custom
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TabMode {
    #[default]
    Public,
    Direct,
    Custom,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct TabSettings {
    pub mode: TabMode,
    pub address: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct DesktopSettings {
    #[serde(default)]
    pub silent_start: bool,
    #[serde(default = "default_true")]
    pub show_dock_icon: bool,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            silent_start: false,
            show_dock_icon: true,
        }
    }
}

fn default_true() -> bool {
    true
}

impl TabSettings {
    pub fn service_url(&self) -> Option<&str> {
        match self.mode {
            TabMode::Public => Some(PUBLIC_TAB_SERVICE_URL),
            TabMode::Direct => None,
            TabMode::Custom => Some(&self.address),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct ProxySettingsInput {
    pub mode: ProxyMode,
    pub address: String,
    pub auth_enabled: bool,
    pub username: String,
    pub password: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ProxySettings {
    pub mode: ProxyMode,
    pub address: String,
    pub auth_enabled: bool,
    pub username: String,
    pub has_password: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct ProxySettingsSecret {
    pub mode: ProxyMode,
    pub address: String,
    pub auth_enabled: bool,
    pub username: String,
    pub password: String,
}

impl Store {
    pub(crate) async fn installation_id(&self) -> Result<String> {
        let generated = uuid::Uuid::new_v4().to_string();
        let _write = self.writes.lock().await;
        sqlx::query(
            "INSERT INTO service_settings(setting_key, value_json, updated_at_ms) VALUES (?, ?, ?) ON CONFLICT(setting_key) DO NOTHING",
        )
        .bind(INSTALLATION_ID_KEY)
        .bind(serde_json::to_string(&generated)?)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        let value = sqlx::query_scalar::<_, String>(
            "SELECT value_json FROM service_settings WHERE setting_key = ?",
        )
        .bind(INSTALLATION_ID_KEY)
        .fetch_one(&self.pool)
        .await?;
        let installation_id = serde_json::from_str::<String>(&value)?;
        uuid::Uuid::parse_str(&installation_id).map_err(|error| {
            crate::Error::Store(format!("invalid persisted installation ID: {error}"))
        })?;
        Ok(installation_id)
    }

    pub(crate) async fn proxy_settings_secret(&self) -> Result<ProxySettingsSecret> {
        let value = sqlx::query_scalar::<_, String>(
            "SELECT value_json FROM service_settings WHERE setting_key = ?",
        )
        .bind(PROXY_SETTINGS_KEY)
        .fetch_optional(&self.pool)
        .await?;
        value
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .unwrap_or_else(|| Ok(ProxySettingsSecret::default()))
    }

    pub async fn proxy_settings(&self) -> Result<ProxySettings> {
        let settings = self.proxy_settings_secret().await?;
        Ok(ProxySettings {
            mode: settings.mode,
            address: settings.address,
            auth_enabled: settings.auth_enabled,
            username: settings.username,
            has_password: !settings.password.is_empty(),
        })
    }

    pub async fn set_proxy_settings(&self, input: ProxySettingsInput) -> Result<ProxySettings> {
        let existing = self.proxy_settings_secret().await?;
        let address = input.address.trim().to_owned();
        if input.mode.is_custom() {
            let parsed = url::Url::parse(&address)
                .map_err(|error| crate::Error::Config(format!("invalid proxy address: {error}")))?;
            if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h") {
                return Err(crate::Error::Config(
                    "proxy address must use http, https, socks5, or socks5h".into(),
                ));
            }
            reqwest::Proxy::all(&address)?;
        }
        let password = if input.auth_enabled {
            input
                .password
                .filter(|password| !password.is_empty())
                .unwrap_or(existing.password)
        } else {
            String::new()
        };
        let settings = ProxySettingsSecret {
            mode: input.mode,
            address,
            auth_enabled: input.auth_enabled,
            username: input.username.trim().to_owned(),
            password,
        };
        let value_json = serde_json::to_string(&settings)?;
        let _write = self.writes.lock().await;
        sqlx::query("INSERT INTO service_settings(setting_key, value_json, updated_at_ms) VALUES (?, ?, ?) ON CONFLICT(setting_key) DO UPDATE SET value_json = excluded.value_json, updated_at_ms = excluded.updated_at_ms")
            .bind(PROXY_SETTINGS_KEY)
            .bind(value_json)
            .bind(now_ms())
            .execute(&self.pool)
            .await?;
        self.proxy_settings().await
    }

    pub async fn tab_settings(&self) -> Result<TabSettings> {
        let value = sqlx::query_scalar::<_, String>(
            "SELECT value_json FROM service_settings WHERE setting_key = ?",
        )
        .bind(TAB_SETTINGS_KEY)
        .fetch_optional(&self.pool)
        .await?;
        value
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .unwrap_or_else(|| Ok(TabSettings::default()))
    }

    pub async fn set_tab_settings(&self, mut settings: TabSettings) -> Result<TabSettings> {
        settings.address = settings.address.trim().trim_end_matches('/').to_owned();
        if settings.mode == TabMode::Custom {
            let parsed = url::Url::parse(&settings.address).map_err(|error| {
                crate::Error::Config(format!("invalid TAB service address: {error}"))
            })?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(crate::Error::Config(
                    "TAB service address must use http or https".into(),
                ));
            }
            if parsed.host_str().is_none()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err(crate::Error::Config(
                    "TAB service address must be a base URL without a query or fragment".into(),
                ));
            }
        }
        let value_json = serde_json::to_string(&settings)?;
        let _write = self.writes.lock().await;
        sqlx::query("INSERT INTO service_settings(setting_key, value_json, updated_at_ms) VALUES (?, ?, ?) ON CONFLICT(setting_key) DO UPDATE SET value_json = excluded.value_json, updated_at_ms = excluded.updated_at_ms")
            .bind(TAB_SETTINGS_KEY)
            .bind(value_json)
            .bind(now_ms())
            .execute(&self.pool)
            .await?;
        Ok(settings)
    }

    pub async fn port_settings(&self) -> Result<PortSettings> {
        let value = sqlx::query_scalar::<_, String>(
            "SELECT value_json FROM service_settings WHERE setting_key = ?",
        )
        .bind(PORT_SETTINGS_KEY)
        .fetch_optional(&self.pool)
        .await?;
        value
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .unwrap_or_else(|| Ok(PortSettings::default()))
    }

    pub async fn set_port_settings(&self, settings: PortSettings) -> Result<()> {
        let value_json = serde_json::to_string(&settings)?;
        let _write = self.writes.lock().await;
        sqlx::query(
            "INSERT INTO service_settings(setting_key, value_json, updated_at_ms) VALUES (?, ?, ?) ON CONFLICT(setting_key) DO UPDATE SET value_json = excluded.value_json, updated_at_ms = excluded.updated_at_ms",
        )
        .bind(PORT_SETTINGS_KEY)
        .bind(value_json)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_service_port(&self, port: u16) -> Result<()> {
        let mut settings = self.port_settings().await?;
        settings.service_port = port;
        self.set_port_settings(settings).await
    }

    pub async fn set_proxy_port(&self, port: u16) -> Result<()> {
        let mut settings = self.port_settings().await?;
        settings.proxy_port = port;
        self.set_port_settings(settings).await
    }

    pub async fn desktop_settings(&self) -> Result<DesktopSettings> {
        let value = sqlx::query_scalar::<_, String>(
            "SELECT value_json FROM service_settings WHERE setting_key = ?",
        )
        .bind(DESKTOP_SETTINGS_KEY)
        .fetch_optional(&self.pool)
        .await?;
        value
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .unwrap_or_else(|| Ok(DesktopSettings::default()))
    }

    pub async fn set_desktop_settings(&self, settings: DesktopSettings) -> Result<()> {
        let value_json = serde_json::to_string(&settings)?;
        let _write = self.writes.lock().await;
        sqlx::query(
            "INSERT INTO service_settings(setting_key, value_json, updated_at_ms) VALUES (?, ?, ?) ON CONFLICT(setting_key) DO UPDATE SET value_json = excluded.value_json, updated_at_ms = excluded.updated_at_ms",
        )
        .bind(DESKTOP_SETTINGS_KEY)
        .bind(value_json)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
