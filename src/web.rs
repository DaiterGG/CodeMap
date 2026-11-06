use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use reqwest::Client;
use serde::Serialize;
use std::io;
use std::process::Command;
use std::sync::Arc;
use tokio::net::TcpListener;

const DEFAULT_WEB_PORT: u16 = 36800;
const OPENROUTER_CHAT_COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const MAX_PROXY_BODY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone)]
struct AppState {
    client: Client,
}

#[derive(Serialize)]
struct ApiStatus {
    status: &'static str,
}

pub fn run() -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;
    runtime.block_on(async {
        let listener = bind_listener().await?;
        let address = listener.local_addr()?;
        let url = format!("http://localhost:{}/", address.port());
        if std::env::var_os("CODEMAP_NO_BROWSER").is_none() {
            open_browser(&url)?;
        }
        println!("CodeMap: {url}");
        let app = Router::new()
            .route("/", get(index))
            .route("/api/auth-status", get(auth_status))
            .route(
                "/api/openrouter/chat/completions",
                post(openrouter_chat_completions),
            )
            .layer(DefaultBodyLimit::max(MAX_PROXY_BODY_BYTES))
            .with_state(Arc::new(AppState {
                client: Client::new(),
            }));
        axum::serve(listener, app).await.map_err(io::Error::other)
    })
}

async fn bind_listener() -> io::Result<TcpListener> {
    for port in DEFAULT_WEB_PORT..=u16::MAX {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {}
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        format!("no available localhost ports from {DEFAULT_WEB_PORT} onward"),
    ))
}

async fn index() -> Html<String> {
    Html(crate::page::render_web_index())
}

async fn auth_status() -> Result<Json<ApiStatus>, (StatusCode, String)> {
    let status = if crate::llm::OpenRouter::is_configured()
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
    {
        "default"
    } else {
        "not found"
    };
    Ok(Json(ApiStatus { status }))
}

async fn openrouter_chat_completions(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Result<Response, (StatusCode, String)> {
    if payload
        .get("model")
        .and_then(serde_json::Value::as_str)
        .is_none_or(str::is_empty)
        || !payload
            .get("messages")
            .is_some_and(serde_json::Value::is_array)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "OpenRouter request must include a model and messages.".to_owned(),
        ));
    }

    let user_key = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    let api_key = match user_key {
        Some(key) => Some(key),
        None => crate::llm::OpenRouter::configured_api_key()
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?,
    }
    .ok_or((
        StatusCode::UNAUTHORIZED,
        "Sign in to OpenRouter or configure the server's default API key.".to_owned(),
    ))?;

    let response = state
        .client
        .post(OPENROUTER_CHAT_COMPLETIONS_URL)
        .bearer_auth(api_key)
        .header(header::CONTENT_TYPE, "application/json")
        .header("X-OpenRouter-Title", "CodeMap")
        .json(&payload)
        .send()
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error.to_string()))?;
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let body = response
        .bytes()
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error.to_string()))?;
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body))
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
        .map(IntoResponse::into_response)
}

fn open_browser(url: &str) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd").args(["/C", "start", "", url]).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(url).spawn()?;
    }
    Ok(())
}
