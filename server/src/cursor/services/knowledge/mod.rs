//! Serves Cursor user rules: upstream-first with an offline markdown cache.
//!
//! 每个请求先回放离线日志再尝试上游;上游成功时把结果写穿到本地镜像,
//! 上游不可达时降级为本地 md 存储并记录日志等待回放。
mod store;
#[allow(dead_code)]
mod sync;

use std::sync::Arc;

use axum::{
    body::{to_bytes, Body, Bytes},
    extract::Extension,
    http::{header, Request, Response},
};
use prost::Message;

use crate::{api::cursor::proxy, config, cursor::protocol::connect, Result};

pub(crate) use store::{RuleRecord, RuleStore};

#[derive(Clone, PartialEq, Message)]
pub(crate) struct KnowledgeBaseAddRequest {
    #[prost(string, tag = "1")]
    knowledge: String,
    #[prost(string, tag = "2")]
    title: String,
    #[prost(string, tag = "3")]
    git_origin: String,
    #[prost(string, optional, tag = "4")]
    composer_id: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub(crate) struct KnowledgeBaseAddResponse {
    #[prost(bool, tag = "1")]
    success: bool,
    #[prost(string, tag = "2")]
    id: String,
}

#[derive(Clone, PartialEq, Message)]
pub(crate) struct KnowledgeBaseListRequest {
    #[prost(int32, optional, tag = "1")]
    limit: Option<i32>,
    #[prost(string, optional, tag = "2")]
    git_origin: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub(crate) struct KnowledgeBaseListResponse {
    #[prost(bool, tag = "1")]
    success: bool,
    #[prost(message, repeated, tag = "2")]
    all_results: Vec<KnowledgeBaseListItem>,
}

#[derive(Clone, PartialEq, Message)]
pub(crate) struct KnowledgeBaseListItem {
    #[prost(string, tag = "1")]
    id: String,
    #[prost(string, tag = "2")]
    knowledge: String,
    #[prost(string, tag = "3")]
    title: String,
    #[prost(string, tag = "4")]
    created_at: String,
    #[prost(bool, tag = "5")]
    is_generated: bool,
}

#[derive(Clone, PartialEq, Message)]
pub(crate) struct KnowledgeBaseUpdateRequest {
    #[prost(string, tag = "1")]
    id: String,
    #[prost(string, tag = "2")]
    knowledge: String,
    #[prost(string, tag = "3")]
    title: String,
}

#[derive(Clone, PartialEq, Message)]
pub(crate) struct KnowledgeBaseUpdateResponse {
    #[prost(bool, tag = "1")]
    success: bool,
}

#[derive(Clone, PartialEq, Message)]
pub(crate) struct KnowledgeBaseRemoveRequest {
    #[prost(string, tag = "1")]
    id: String,
}

#[derive(Clone, PartialEq, Message)]
pub(crate) struct KnowledgeBaseRemoveResponse {
    #[prost(bool, tag = "1")]
    success: bool,
}

/// 规则存储与并发锁;经 axum Extension 注入四个 handler。
#[derive(Clone)]
pub struct KnowledgeService {
    inner: Arc<Inner>,
}

struct Inner {
    store: RuleStore,
    lock: tokio::sync::Mutex<()>,
}

impl KnowledgeService {
    pub fn managed() -> Result<Self> {
        Self::with_root(config::managed_data_dir()?.join("rules"))
    }

    /// 指定存储根目录构造;managed() 与集成测试共用。
    pub fn with_root(root: std::path::PathBuf) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                store: RuleStore::open(root)?,
                lock: tokio::sync::Mutex::new(()),
            }),
        })
    }
}

pub async fn add(
    _upstream: Extension<proxy::CursorProxy>,
    Extension(service): Extension<KnowledgeService>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    let (_parts, body) = buffered(request).await?;
    let message: KnowledgeBaseAddRequest = connect::decode_unary(&body)?;
    let _guard = service.inner.lock.lock().await;
    let store = &service.inner.store;

    let id = format!("{}{}", store::LOCAL_ID_PREFIX, uuid::Uuid::new_v4());
    store.upsert(&RuleRecord {
        id: id.clone(),
        knowledge: message.knowledge,
        title: message.title,
        created_at: now(),
        is_generated: false,
        git_origin: message.git_origin,
    })?;
    store.record_add(&id)?;
    proto(KnowledgeBaseAddResponse { success: true, id })
}

pub async fn list(
    _upstream: Extension<proxy::CursorProxy>,
    Extension(service): Extension<KnowledgeService>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    let (_parts, body) = buffered(request).await?;
    let message: KnowledgeBaseListRequest = connect::decode_unary(&body)?;
    let _guard = service.inner.lock.lock().await;
    let store = &service.inner.store;
    let git_origin = message.git_origin.unwrap_or_default();

    let mut records = store.list()?;
    if !git_origin.is_empty() {
        records.retain(|record| record.git_origin == git_origin);
    }
    if let Some(limit) = message.limit {
        if limit >= 0 {
            records.truncate(limit as usize);
        }
    }
    proto(KnowledgeBaseListResponse {
        success: true,
        all_results: records
            .into_iter()
            .map(|record| KnowledgeBaseListItem {
                id: record.id,
                knowledge: record.knowledge,
                title: record.title,
                created_at: record.created_at,
                is_generated: record.is_generated,
            })
            .collect(),
    })
}

pub async fn update(
    _upstream: Extension<proxy::CursorProxy>,
    Extension(service): Extension<KnowledgeService>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    let (_parts, body) = buffered(request).await?;
    let message: KnowledgeBaseUpdateRequest = connect::decode_unary(&body)?;
    let _guard = service.inner.lock.lock().await;
    let store = &service.inner.store;

    let Some(mut record) = store.get(&message.id)? else {
        return proto(KnowledgeBaseUpdateResponse { success: false });
    };
    record.knowledge = message.knowledge;
    record.title = message.title;
    store.upsert(&record)?;
    store.record_update(&message.id)?;
    proto(KnowledgeBaseUpdateResponse { success: true })
}

pub async fn remove(
    _upstream: Extension<proxy::CursorProxy>,
    Extension(service): Extension<KnowledgeService>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    let (_parts, body) = buffered(request).await?;
    let message: KnowledgeBaseRemoveRequest = connect::decode_unary(&body)?;
    let _guard = service.inner.lock.lock().await;
    let store = &service.inner.store;

    store.remove(&message.id)?;
    store.record_remove(&message.id)?;
    proto(KnowledgeBaseRemoveResponse { success: true })
}

async fn buffered(request: Request<Body>) -> Result<(axum::http::request::Parts, Bytes)> {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, usize::MAX)
        .await
        .map_err(|error| crate::Error::Protocol(format!("cannot read request body: {error}")))?;
    Ok((parts, body))
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn proto(message: impl Message) -> Result<Response<Body>> {
    let body = message.encode_to_vec();
    let length = body.len();
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/proto"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        length
            .to_string()
            .parse()
            .expect("body length is always a valid header value"),
    );
    Ok(response)
}
