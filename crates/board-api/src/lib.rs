//! Optional loopback-only HTTP adapter. The native clients do not call this server.
mod access_api;
mod catalog;
mod files_api;
mod messages;
pub use catalog::document as discovery_document;
mod sources;
mod sse;
#[cfg(test)]
mod tests;
mod work;

use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use board_local::{
    Board,
    errors::{Kind, classify},
};
use serde_json::{Value, json};
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    sync::{Arc, Mutex},
};
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouterExt};

#[derive(Clone)]
struct App {
    db: Arc<Mutex<Board>>,
    actor: Arc<str>,
    token: Arc<str>,
}
#[derive(Clone, Default)]
struct RequestIdentity {
    delegated: Option<String>,
    scope: Option<String>,
    profile: Option<String>,
}
tokio::task_local! {static IDENTITY:RequestIdentity;}

#[derive(serde::Serialize, utoipa::ToSchema)]
struct Problem<'a> {
    status: u16,
    code: &'a str,
    detail: &'a str,
}

struct ApiError {
    status: StatusCode,
    code: &'static str,
    detail: String,
}
type ApiResult<T> = std::result::Result<T, ApiError>;

impl ApiError {
    fn new(status: StatusCode, detail: impl Into<String>) -> Self {
        let code = match status {
            StatusCode::UNAUTHORIZED => "unauthorized",
            StatusCode::FORBIDDEN => "forbidden",
            StatusCode::NOT_FOUND => "not_found",
            StatusCode::CONFLICT => "conflict",
            StatusCode::PAYLOAD_TOO_LARGE => "too_large",
            StatusCode::METHOD_NOT_ALLOWED => "method_not_allowed",
            StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
            _ if status.is_server_error() => "internal_error",
            _ => "invalid_request",
        };
        Self {
            status,
            code,
            detail: detail.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(Problem {
                status: self.status.as_u16(),
                code: self.code,
                detail: &self.detail,
            }),
        )
            .into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            "application/problem+json".parse().unwrap(),
        );
        response
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        let (kind, code) = classify(&err);
        let status = if code == "invalid_credential" {
            StatusCode::UNAUTHORIZED
        } else if code == "invalid_scope" {
            StatusCode::BAD_REQUEST
        } else {
            match kind {
                Kind::Missing => StatusCode::NOT_FOUND,
                Kind::Conflict => StatusCode::CONFLICT,
                Kind::Forbidden => StatusCode::FORBIDDEN,
                Kind::Gone => StatusCode::GONE,
                Kind::Busy => StatusCode::SERVICE_UNAVAILABLE,
                Kind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
                Kind::Timeout => StatusCode::REQUEST_TIMEOUT,
                Kind::Invalid => StatusCode::UNPROCESSABLE_ENTITY,
            }
        };
        let detail = if status.is_server_error() {
            eprintln!("board-api: {err:#}");
            "database unavailable".to_owned()
        } else {
            err.to_string()
        };
        Self {
            status,
            code,
            detail,
        }
    }
}

impl App {
    async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Board, &str) -> Result<T> + Send + 'static,
    ) -> ApiResult<T> {
        let app = self.clone();
        let identity = IDENTITY
            .try_with(Clone::clone)
            .map_err(|_| ApiError::new(StatusCode::UNAUTHORIZED, "request identity missing"))?;
        tokio::task::spawn_blocking(move || {
            let mut db = app.db.lock().map_err(|_| {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "database unavailable")
            })?;
            Ok::<T, ApiError>(db.with_profile_request(
                identity.delegated.as_deref(),
                &app.actor,
                identity.scope.as_deref(),
                identity.profile.as_deref(),
                f,
            )?)
        })
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "database worker stopped"))?
    }
}

async fn authorize(State(app): State<App>, req: Request, next: Next) -> Response {
    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));
    let mut r = if provided.is_none() {
        let mut r =
            ApiError::new(StatusCode::UNAUTHORIZED, "local bearer token required").into_response();
        r.headers_mut()
            .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
        r
    } else if req.headers().contains_key(header::ORIGIN) {
        ApiError::new(StatusCode::FORBIDDEN, "browser origins are not accepted").into_response()
    } else if ["x-board-can", "x-board-profile"].into_iter().any(|k| {
        req.headers().get_all(k).iter().count() > 1
            || req.headers().get(k).is_some_and(|v| v.to_str().is_err())
    }) {
        ApiError::new(StatusCode::BAD_REQUEST, "invalid identity header").into_response()
    } else {
        let identity = RequestIdentity {
            profile: req
                .headers()
                .get("x-board-profile")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
            delegated: provided
                .filter(|v| *v != app.token.as_ref())
                .map(str::to_owned),
            scope: req
                .headers()
                .get("x-board-can")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
        };
        let mut r = IDENTITY
            .scope(identity, async {
                if let Err(e) = app.call(|_, _| Ok(())).await {
                    return e.into_response();
                }
                next.run(req).await
            })
            .await;
        let status = r.status();
        if (status.is_client_error() || status.is_server_error())
            && !r
                .headers()
                .get(header::CONTENT_TYPE)
                .is_some_and(|h| h == "application/problem+json")
        {
            // Axum also rejects invalid path/query/JSON, lengths and methods before a handler runs.
            let allow = r.headers().get(header::ALLOW).cloned();
            r = ApiError::new(
                status,
                status.canonical_reason().unwrap_or("request rejected"),
            )
            .into_response();
            if let Some(allow) = allow {
                r.headers_mut().insert(header::ALLOW, allow);
            }
        }
        r
    };
    if r.status() == StatusCode::UNAUTHORIZED {
        r.headers_mut()
            .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
    }
    r.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    r
}

fn routes(app: App) -> Router {
    use messages::*;
    let (api, contract) = OpenApiRouter::new()
        .routes(utoipa_axum::routes!(threads))
        .routes(utoipa_axum::routes!(thread))
        .routes(utoipa_axum::routes!(messages))
        .routes(utoipa_axum::routes!(channels, channel))
        .routes(utoipa_axum::routes!(one_channel))
        .routes(utoipa_axum::routes!(actors))
        .routes(utoipa_axum::routes!(save_profile))
        .routes(utoipa_axum::routes!(post_message).layer(DefaultBodyLimit::max(56 * 1024 * 1024)))
        .routes(utoipa_axum::routes!(message, edit, remove))
        .routes(utoipa_axum::routes!(posted))
        .routes(utoipa_axum::routes!(react, unreact))
        .routes(utoipa_axum::routes!(attachment))
        .routes(utoipa_axum::routes!(events))
        .routes(utoipa_axum::routes!(schemas))
        .routes(utoipa_axum::routes!(schema, register_schema))
        .merge(access_api::routes())
        .merge(catalog::routes())
        .merge(files_api::routes())
        .merge(sources::routes())
        .merge(work::routes())
        .split_for_parts();
    let api = api.layer(axum::Extension(Arc::new(catalog::spec(contract))));
    Router::new()
        .nest("/v1", api)
        .layer(middleware::from_fn_with_state(app.clone(), authorize))
        .with_state(app)
}

/// Serve until interrupted. No public bind option, cookies, CORS or outbound HTTP.
pub fn serve(board: Board, actor: String, port: u16) -> Result<()> {
    let token = match std::env::var("BOARD_API_TOKEN") {
        Ok(t) => {
            ensure!(
                t.len() >= 16 && t.bytes().all(|b| b.is_ascii_graphic()),
                "BOARD_API_TOKEN must be at least 16 printable non-space ASCII characters"
            );
            t
        }
        Err(std::env::VarError::NotPresent) => {
            let t = uuid::Uuid::new_v4().to_string();
            eprintln!("Session token: {t}");
            t
        }
        Err(e) => return Err(e.into()),
    };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            let listener =
                tokio::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).await?;
            eprintln!("Board API: http://{}/v1", listener.local_addr()?);
            let app = App {
                db: Arc::new(Mutex::new(board)),
                actor: actor.into(),
                token: token.into(),
            };
            axum::serve(listener, routes(app))
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
            Ok(())
        })
}

fn created(location: String, body: Value) -> Response {
    (
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(body),
    )
        .into_response()
}

fn idempotency(headers: &HeaderMap) -> ApiResult<Option<String>> {
    headers
        .get("idempotency-key")
        .map(|v| {
            v.to_str()
                .map(str::to_owned)
                .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid Idempotency-Key"))
        })
        .transpose()
}
