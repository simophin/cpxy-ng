//! Read-only router monitoring API and a self-contained browser UI.
//!
//! This listener has no authentication. Bind to loopback for SSH forwarding, or explicitly to a
//! trusted management interface. It records only what the engine sees, not other router services.

use crate::{ConnectionPage, EngineHandle, TrafficStats};
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::net::TcpListener;

#[derive(Clone)]
struct Monitor {
    engine: Arc<EngineHandle>,
    session: String,
}

#[derive(Serialize)]
struct TrafficSnapshot {
    session: String,
    running: bool,
    #[serde(flatten)]
    traffic: TrafficStats,
}

#[derive(Serialize)]
struct Connections {
    session: String,
    #[serde(flatten)]
    page: ConnectionPage,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    since: Option<u64>,
    before: Option<u64>,
    limit: Option<u32>,
}

/// Serve until cancelled by the owner. The caller binds the listener before starting the engine
/// so that an occupied monitoring port can fail startup without leaving a worker running.
pub async fn serve(listener: TcpListener, engine: Arc<EngineHandle>) -> anyhow::Result<()> {
    tracing::info!(address = %listener.local_addr()?, "Traffic UI and API listening");
    axum::serve(listener, router(engine)).await?;
    Ok(())
}

fn router(engine: Arc<EngineHandle>) -> Router {
    let session = cpxy_ng::key_util::random_vec(16)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/style.css"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route("/api/traffic", get(traffic))
        .route("/api/connections", get(connections))
        .layer(middleware::from_fn(response_headers))
        .with_state(Monitor { engine, session })
}

async fn traffic(State(monitor): State<Monitor>) -> Json<TrafficSnapshot> {
    Json(TrafficSnapshot {
        session: monitor.session,
        running: monitor.engine.is_running(),
        traffic: monitor.engine.traffic(),
    })
}

async fn connections(
    State(monitor): State<Monitor>,
    Query(query): Query<PageQuery>,
) -> Result<Json<Connections>, (StatusCode, &'static str)> {
    if query.since.is_some() && query.before.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Use either since or before, not both",
        ));
    }
    let limit = query.limit.unwrap_or(200);
    if !(1..=1000).contains(&limit) {
        return Err((StatusCode::BAD_REQUEST, "limit must be between 1 and 1000"));
    }
    let page = match query.since {
        Some(since) => monitor.engine.connections_since(since, limit),
        None => monitor.engine.connections_before(query.before, limit),
    };
    Ok(Json(Connections {
        session: monitor.session,
        page,
    }))
}

async fn response_headers(request: axum::extract::Request, next: Next) -> Response {
    // A remote website must not read a local user's connection history. No CORS is enabled.
    if request
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|site| site == "cross-site")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert("content-security-policy", HeaderValue::from_static(
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src data:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"
    ));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConnectionEvent, connection_log::ConnectionLog, tun::Traffic};
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use serde_json::Value;
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;
    use tower::ServiceExt;

    fn engine() -> Arc<EngineHandle> {
        let traffic = Arc::new(Traffic::default());
        traffic.sent.store(123, Ordering::Relaxed);
        traffic.received.store(456, Ordering::Relaxed);
        Arc::new(EngineHandle {
            connections: Arc::new(ConnectionLog::new(3)),
            running: Mutex::new(None),
            traffic,
            dns: None,
            local_proxy_port: None,
        })
    }

    async fn request(app: &Router, path: &str) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn pages_history_and_identifies_eviction_and_restart() {
        let engine = engine();
        let app = router(engine.clone());
        let (_, empty) = request(&app, "/api/connections").await;
        assert_eq!(empty["records"], serde_json::json!([]));
        for port in 1..=4 {
            engine.connections.push(ConnectionEvent {
                host: "<script>example</script>".into(),
                ip: None,
                port,
                outbound: "proxy".into(),
                delay_millis: 12,
                time_millis: 34,
                error: Some("refused".into()),
            });
        }
        let (status, page) = request(&app, "/api/connections?limit=2").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["records"][0]["event"]["port"], 3);
        assert_eq!(page["records"][1]["event"]["port"], 4);
        assert_eq!(page["records"][0]["event"]["error"], "refused");
        let first = page["records"][0]["seq"].as_u64().unwrap();
        let (_, older) = request(&app, &format!("/api/connections?before={first}")).await;
        assert_eq!(older["records"].as_array().unwrap().len(), 1);
        assert_eq!(older["records"][0]["event"]["port"], 2);
        let (_, since) = request(&app, "/api/connections?since=0").await;
        assert_eq!(since["records"].as_array().unwrap().len(), 3);
        assert_eq!(since["oldest_seq"], older["records"][0]["seq"]);
        let (_, stats) = request(&app, "/api/traffic").await;
        assert_eq!(stats["session"], page["session"]);
        assert_eq!(stats["sent"], 123);
        assert_eq!(stats["received"], 456);
        assert_eq!(stats["running"], false);
        let (_, restarted) = request(&router(engine.clone()), "/api/traffic").await;
        assert_ne!(stats["session"], restarted["session"]);
    }

    #[tokio::test]
    async fn rejects_ambiguous_or_unbounded_queries() {
        let app = router(engine());
        for query in [
            "limit=0",
            "limit=1001",
            "limit=-1",
            "since=1&before=2",
            "since=no",
            "typo=1",
        ] {
            assert_eq!(
                request(&app, &format!("/api/connections?{query}")).await.0,
                StatusCode::BAD_REQUEST,
                "{query}"
            );
        }
        assert_eq!(
            request(&app, "/api/connections?limit=1000").await.0,
            StatusCode::OK
        );
        assert_eq!(request(&app, "/unknown").await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn serves_only_local_assets_with_no_cache_or_external_content() {
        let app = router(engine());
        for (path, content_type) in [
            ("/", "text/html"),
            ("/style.css", "text/css"),
            ("/app.js", "text/javascript"),
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(
                response.headers()[header::CONTENT_TYPE]
                    .to_str()
                    .unwrap()
                    .starts_with(content_type)
            );
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert!(
                response.headers()["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .contains("connect-src 'self'")
            );
            assert!(
                !response
                    .headers()
                    .contains_key("access-control-allow-origin")
            );
        }
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/traffic")
                    .header("sec-fetch-site", "cross-site")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        // Keep the entire frontend comfortably smaller than even a small framework download.
        let assets = [
            include_str!("../web/index.html"),
            include_str!("../web/style.css"),
            include_str!("../web/app.js"),
        ];
        assert!(assets.iter().map(|asset| asset.len()).sum::<usize>() < 20_000);
        for asset in assets {
            assert!(!asset.contains("https://") && !asset.contains("http://"));
        }
    }
}
