use axum::{
    body::{Body, Bytes},
    extract::Extension,
    http::{header, Request, Response},
};
use prost::Message;
use serde_json::{Map, Value};

use crate::{cursor::proxy, Result};

const LOCAL_AUTH_ID: &str = "local_ultra";
const LOCAL_EMAIL: &str = "cursor@ai.com";
const LOCAL_ULTRA_PLAN_INCLUDED_CENTS: i32 = 20_000;

#[derive(Clone, PartialEq, Message)]
struct GetEmailResponse {
    #[prost(string, tag = "1")]
    email: String,
    #[prost(int32, tag = "2")]
    sign_up_type: i32,
}

#[derive(Clone, PartialEq, Message)]
struct GetMeResponse {
    #[prost(string, tag = "1")]
    auth_id: String,
    #[prost(int32, tag = "2")]
    user_id: i32,
    #[prost(string, optional, tag = "3")]
    email: Option<String>,
    #[prost(string, optional, tag = "4")]
    first_name: Option<String>,
    #[prost(string, optional, tag = "5")]
    last_name: Option<String>,
    #[prost(string, optional, tag = "8")]
    created_at: Option<String>,
    #[prost(bool, optional, tag = "9")]
    is_enterprise_user: Option<bool>,
    #[prost(string, optional, tag = "11")]
    email_domain_type: Option<String>,
    #[prost(string, optional, tag = "12")]
    country: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
struct GetUserProfileResponse {
    #[prost(bool, optional, tag = "4")]
    public_visibility_allowed: Option<bool>,
    #[prost(string, optional, tag = "5")]
    max_visibility: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
struct GetCurrentPeriodUsageResponse {
    #[prost(int64, tag = "1")]
    billing_cycle_start: i64,
    #[prost(int64, tag = "2")]
    billing_cycle_end: i64,
    #[prost(message, optional, tag = "3")]
    plan_usage: Option<PlanUsage>,
    #[prost(message, optional, tag = "4")]
    spend_limit_usage: Option<SpendLimitUsage>,
    #[prost(int32, optional, tag = "5")]
    display_threshold: Option<i32>,
    #[prost(bool, tag = "6")]
    enabled: bool,
    #[prost(string, tag = "7")]
    display_message: String,
    #[prost(string, optional, tag = "11")]
    auto_model_selected_display_message: Option<String>,
    #[prost(string, optional, tag = "12")]
    named_model_selected_display_message: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
struct PlanUsage {
    #[prost(int32, tag = "1")]
    total_spend: i32,
    #[prost(int32, tag = "2")]
    included_spend: i32,
    #[prost(int32, tag = "4")]
    remaining: i32,
    #[prost(int32, tag = "5")]
    limit: i32,
    #[prost(bool, optional, tag = "6")]
    remaining_bonus: Option<bool>,
    #[prost(string, optional, tag = "7")]
    bonus_tooltip: Option<String>,
    #[prost(int32, optional, tag = "8")]
    auto_spend: Option<i32>,
    #[prost(int32, optional, tag = "9")]
    api_spend: Option<i32>,
    #[prost(double, optional, tag = "12")]
    auto_percent_used: Option<f64>,
    #[prost(double, optional, tag = "13")]
    api_percent_used: Option<f64>,
    #[prost(double, optional, tag = "14")]
    total_percent_used: Option<f64>,
}

#[derive(Clone, PartialEq, Message)]
struct SpendLimitUsage {
    #[prost(string, tag = "8")]
    limit_type: String,
}

#[derive(Clone, PartialEq, Message)]
struct GetUsageLimitStatusAndActiveGrantsResponse {
    #[prost(message, optional, tag = "1")]
    usage_limit_policy_status: Option<UsageLimitPolicyStatus>,
}

#[derive(Clone, PartialEq, Message)]
struct UsageLimitPolicyStatus {
    #[prost(bool, tag = "1")]
    is_in_slow_pool: bool,
    #[prost(map = "string, string", tag = "5")]
    features: std::collections::HashMap<String, String>,
    #[prost(bool, tag = "6")]
    can_configure_spend_limit: bool,
    #[prost(bool, tag = "8")]
    has_pending_request: bool,
    #[prost(string, repeated, tag = "9")]
    allowed_model_ids: Vec<String>,
    #[prost(string, repeated, tag = "10")]
    allowed_model_tags: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Message)]
struct Empty {}

pub async fn get_email(
    Extension(upstream): Extension<proxy::CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    forward_or(upstream, request, || {
        proto(GetEmailResponse {
            email: LOCAL_EMAIL.into(),
            sign_up_type: 3,
        })
    })
    .await
}

pub async fn get_me(
    Extension(upstream): Extension<proxy::CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    forward_or(upstream, request, || {
        proto(GetMeResponse {
            auth_id: LOCAL_AUTH_ID.into(),
            user_id: 1,
            email: Some(LOCAL_EMAIL.into()),
            first_name: Some("Cursor".into()),
            last_name: Some("Local".into()),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
            is_enterprise_user: Some(false),
            email_domain_type: Some("personal".into()),
            country: Some("US".into()),
        })
    })
    .await
}

pub async fn get_teams(
    Extension(upstream): Extension<proxy::CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    forward_or(upstream, request, || empty()).await
}

fn empty() -> Result<Response<Body>> {
    proto(Empty {})
}

pub async fn get_user_profile(
    Extension(upstream): Extension<proxy::CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    forward_or(upstream, request, || {
        proto(GetUserProfileResponse {
            public_visibility_allowed: Some(true),
            max_visibility: Some("PUBLIC".into()),
        })
    })
    .await
}

pub async fn current_period_usage() -> Result<Response<Body>> {
    let now = chrono::Utc::now();
    proto(GetCurrentPeriodUsageResponse {
        billing_cycle_start: (now - chrono::Duration::days(30)).timestamp_millis(),
        billing_cycle_end: (now + chrono::Duration::days(10 * 365)).timestamp_millis(),
        plan_usage: Some(PlanUsage {
            total_spend: 0,
            included_spend: LOCAL_ULTRA_PLAN_INCLUDED_CENTS,
            remaining: LOCAL_ULTRA_PLAN_INCLUDED_CENTS,
            limit: LOCAL_ULTRA_PLAN_INCLUDED_CENTS,
            remaining_bonus: Some(false),
            bonus_tooltip: Some("Ultra local account mock is active.".into()),
            auto_spend: Some(0),
            api_spend: Some(0),
            auto_percent_used: Some(0.0),
            api_percent_used: Some(0.0),
            total_percent_used: Some(0.0),
        }),
        spend_limit_usage: Some(SpendLimitUsage {
            limit_type: "user".into(),
        }),
        display_threshold: Some(99_999_999),
        enabled: true,
        display_message: "Ultra plan active".into(),
        auto_model_selected_display_message: Some("Ultra plan active".into()),
        named_model_selected_display_message: Some("Ultra plan active".into()),
    })
}

pub async fn usage_limit_status() -> Result<Response<Body>> {
    proto(GetUsageLimitStatusAndActiveGrantsResponse {
        usage_limit_policy_status: Some(UsageLimitPolicyStatus {
            is_in_slow_pool: false,
            features: Default::default(),
            can_configure_spend_limit: true,
            has_pending_request: false,
            allowed_model_ids: Vec::new(),
            allowed_model_tags: Vec::new(),
        }),
    })
}

pub async fn stripe_profile(
    Extension(upstream): Extension<proxy::CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    match proxy::forward_buffered(&upstream, request).await {
        Ok(response) if response.status.is_success() => {
            let mut profile = serde_json::from_slice::<Map<String, Value>>(&response.body)?;
            ultra(&mut profile);
            Ok(response.with_body(Bytes::from(serde_json::to_vec(&profile)?)))
        }
        Ok(response) => {
            tracing::warn!(status = %response.status, "Cursor account upstream rejected profile; using local Ultra identity");
            json(ultra_profile())
        }
        Err(error) => {
            tracing::warn!(%error, "Cursor account upstream unavailable; using local Ultra identity");
            json(ultra_profile())
        }
    }
}

async fn forward_or(
    upstream: proxy::CursorProxy,
    request: Request<Body>,
    fallback: impl FnOnce() -> Result<Response<Body>>,
) -> Result<Response<Body>> {
    match proxy::forward_buffered(&upstream, request).await {
        Ok(response) if response.status.is_success() => Ok(response.into_response()),
        Ok(response) => {
            tracing::warn!(status = %response.status, "Cursor identity upstream rejected request; using local identity");
            fallback()
        }
        Err(error) => {
            tracing::warn!(%error, "Cursor identity upstream unavailable; using local identity");
            fallback()
        }
    }
}

fn proto(message: impl Message) -> Result<Response<Body>> {
    response("application/proto", message.encode_to_vec())
}

fn json(value: Value) -> Result<Response<Body>> {
    response("application/json", serde_json::to_vec(&value)?)
}

fn response(content_type: &'static str, body: Vec<u8>) -> Result<Response<Body>> {
    let length = body.len();
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(content_type),
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

fn ultra(profile: &mut Map<String, Value>) {
    profile.insert("membershipType".into(), Value::String("ultra".into()));
    profile.insert(
        "individualMembershipType".into(),
        Value::String("ultra".into()),
    );
    profile.insert("subscriptionStatus".into(), Value::String("active".into()));
}

fn ultra_profile() -> Value {
    serde_json::json!({
        "membershipType": "ultra",
        "individualMembershipType": "ultra",
        "subscriptionStatus": "active",
        "lastPaymentFailed": false,
        "pendingCancellationDate": null,
        "daysRemainingOnTrial": 0,
        "paymentId": LOCAL_AUTH_ID,
        "isTeamMember": false
    })
}

#[cfg(test)]
mod tests {
    use axum::{
        body::to_bytes,
        http::StatusCode,
        routing::{get, post},
        Extension, Router,
    };
    use tower::ServiceExt;

    use super::*;

    async fn app(upstream: Router) -> (Router, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let proxy = proxy::CursorProxy::for_upstream(&format!("http://{address}")).unwrap();
        let app = Router::new()
            .route("/auth/full_stripe_profile", get(stripe_profile))
            .route("/aiserver.v1.DashboardService/GetMe", post(get_me))
            .route(
                "/aiserver.v1.DashboardService/GetCurrentPeriodUsage",
                post(current_period_usage),
            )
            .route(
                "/aiserver.v1.DashboardService/GetUsageLimitStatusAndActiveGrants",
                post(usage_limit_status),
            )
            .layer(Extension(proxy));
        (app, server)
    }

    #[tokio::test]
    async fn preserves_upstream_profile_and_overlays_ultra_membership() {
        let upstream = Router::new().route(
            "/auth/full_stripe_profile",
            get(|| async {
                axum::Json(serde_json::json!({
                    "membershipType": "pro",
                    "subscriptionStatus": "inactive",
                    "paymentId": "upstream-payment"
                }))
            }),
        );
        let (app, server) = app(upstream).await;
        let response = app
            .oneshot(
                Request::get("/auth/full_stripe_profile")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let profile: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(profile["membershipType"], "ultra");
        assert_eq!(profile["paymentId"], "upstream-payment");
        server.abort();
    }

    #[tokio::test]
    async fn upstream_error_uses_local_identity_without_reading_authorization() {
        let upstream = Router::new().route(
            "/aiserver.v1.DashboardService/GetMe",
            post(|| async { StatusCode::UNAUTHORIZED }),
        );
        let (app, server) = app(upstream).await;
        let response = app
            .oneshot(
                Request::post("/aiserver.v1.DashboardService/GetMe")
                    .header(header::AUTHORIZATION, "Bearer ignored")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let identity = GetMeResponse::decode(body).unwrap();
        assert_eq!(identity.auth_id, LOCAL_AUTH_ID);
        assert_eq!(identity.email.as_deref(), Some(LOCAL_EMAIL));
        server.abort();
    }

    #[tokio::test]
    async fn stripe_error_uses_the_complete_local_ultra_profile() {
        let upstream = Router::new().route(
            "/auth/full_stripe_profile",
            get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        );
        let (app, server) = app(upstream).await;
        let response = app
            .oneshot(
                Request::get("/auth/full_stripe_profile")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let profile: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(profile["membershipType"], "ultra");
        assert_eq!(profile["paymentId"], LOCAL_AUTH_ID);
        server.abort();
    }

    #[tokio::test]
    async fn current_period_usage_is_a_local_unused_ultra_allowance() {
        let (app, server) = app(Router::new()).await;
        let before = chrono::Utc::now().timestamp_millis();
        let response = app
            .oneshot(
                Request::post("/aiserver.v1.DashboardService/GetCurrentPeriodUsage")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let usage = GetCurrentPeriodUsageResponse::decode(body).unwrap();
        let plan = usage.plan_usage.unwrap();
        assert_eq!(plan.total_spend, 0);
        assert_eq!(plan.limit, LOCAL_ULTRA_PLAN_INCLUDED_CENTS);
        assert_eq!(plan.remaining, LOCAL_ULTRA_PLAN_INCLUDED_CENTS);
        assert_eq!(usage.display_message, "Ultra plan active");
        assert!(usage.billing_cycle_start < before);
        assert!(usage.billing_cycle_end > before);
        server.abort();
    }

    #[tokio::test]
    async fn usage_limit_status_is_local_and_unrestricted() {
        let (app, server) = app(Router::new()).await;
        let response = app
            .oneshot(
                Request::post("/aiserver.v1.DashboardService/GetUsageLimitStatusAndActiveGrants")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response = GetUsageLimitStatusAndActiveGrantsResponse::decode(body).unwrap();
        let policy = response.usage_limit_policy_status.unwrap();
        assert!(!policy.is_in_slow_pool);
        assert!(policy.can_configure_spend_limit);
        assert!(!policy.has_pending_request);
        assert!(policy.allowed_model_ids.is_empty());
        assert!(policy.allowed_model_tags.is_empty());
        server.abort();
    }
}
