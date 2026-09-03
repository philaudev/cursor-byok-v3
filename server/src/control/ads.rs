//! Implements advertisement configuration endpoints.
//! Advertisement service contract and desktop HTTP handler.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{Error, Result};

use super::ControlService;

// 此广告拉取不涉及用户隐私，用户id随机产生
// 开源项目广告为作者唯一收入来源，PR时请勿去除此广告

pub(super) const ADS_ENDPOINT: &str = "https://tab.leokun.cn/api/v1/ads?placement=menu";
// pub(super) const ADS_ENDPOINT: &str = "http://localhost:8080/api/v1/ads?placement=menu";
pub(super) const DEVICE_ID_HEADER: &str = "X-Cursor-Assistant-Device-ID";
pub(super) const OS_HEADER: &str = "X-Cursor-Assistant-OS";
pub(super) const APP_VERSION_HEADER: &str = "X-Cursor-Assistant-Version";
pub(super) const DISABLED_AD_IDS_HEADER: &str = "disable-ad-ids";
pub(super) const LANGUAGE_HEADER: &str = "accept-language";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdRuntime {
    pub slots: Vec<AdSlot>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdSlot {
    pub id: String,
    pub enabled: bool,
    pub placement: AdPlacement,
    pub target: AdTarget,
    pub content: AdContent,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdPlacement {
    Menu,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdTarget {
    pub title: String,
    pub description: String,
    pub image_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdContent {
    pub title: String,
    pub description: String,
    pub image_url: String,
    pub details: Vec<AdDetail>,
    pub button: AdButton,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdDetail {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdButton {
    pub label: String,
    pub action: AdAction,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdAction {
    #[serde(rename = "type")]
    pub action_type: AdActionType,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdDismissalInput {
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdActionType {
    OpenBrowser,
}

impl AdRuntime {
    pub(super) fn into_menu_slots(mut self) -> Result<Self> {
        self.slots
            .retain(|slot| slot.enabled && slot.placement == AdPlacement::Menu);
        for slot in &self.slots {
            validate_http_url(&slot.target.image_url, "target.imageUrl")?;
            validate_http_url(&slot.content.image_url, "content.imageUrl")?;
            validate_http_url(&slot.content.button.action.url, "content.button.action.url")?;
        }
        Ok(self)
    }
}

fn validate_http_url(value: &str, field: &str) -> Result<()> {
    let url = Url::parse(value)
        .map_err(|error| Error::Provider(format!("advertisement {field} is invalid: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(Error::Provider(format!(
            "advertisement {field} must be an absolute HTTP or HTTPS URL"
        )));
    }
    Ok(())
}

pub async fn get(
    State(service): State<ControlService>,
    headers: HeaderMap,
) -> Result<Json<AdRuntime>> {
    let disabled_ad_ids = headers
        .get(DISABLED_AD_IDS_HEADER)
        .and_then(|value| value.to_str().ok());
    Ok(Json(
        service.ads(disabled_ad_ids, ad_language(&headers)).await?,
    ))
}

fn ad_language(headers: &HeaderMap) -> &'static str {
    match headers
        .get(LANGUAGE_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        Some(value) if value.eq_ignore_ascii_case("zh-CN") => "zh-CN",
        _ => "en-US",
    }
}

pub async fn dismiss(
    State(service): State<ControlService>,
    Path(ad_id): Path<String>,
    Json(input): Json<AdDismissalInput>,
) -> Result<StatusCode> {
    service.dismiss_ad(&ad_id, &input).await?;
    Ok(StatusCode::NO_CONTENT)
}
