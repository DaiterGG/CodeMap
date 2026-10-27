//! Local web mode that serves the graph page until browser polling stops.

use axum::Json;
use axum::Router;
use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, Redirect};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Client;
use serde::Deserialize;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io;
use std::path::PathBuf;
use std::process::Command;
#[cfg(not(target_os = "windows"))]
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;

const IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_secs(3);
const AUTH_CODE_TTL: Duration = Duration::from_secs(600);
const MAX_RETRIES: usize = 3;
const DEFAULT_WEB_PORT: u16 = 36800;

struct AppState {
    last_poll: Mutex<Instant>,
    auth: Mutex<AuthState>,
    generation: Mutex<GenerationState>,
    callback_url: String,
    server_mode: bool,
}

#[derive(Default)]
struct GenerationState {
    id: u64,
    status: GenerationStatus,
}

#[derive(Default)]
enum GenerationStatus {
    #[default]
    Idle,
    Running,
    Ready(serde_json::Value),
    Failed(String),
}

#[derive(Default)]
struct AuthState {
    user_api_key: Option<String>,
    pending: Option<PendingAuth>,
}

struct PendingAuth {
    verifier: String,
    state: String,
    started_at: Instant,
}

pub fn run() -> io::Result<()> {
    run_mode(false)
}

pub fn run_server_mode() -> io::Result<()> {
    run_mode(true)
}

fn run_mode(server_mode: bool) -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;
    runtime.block_on(run_server(server_mode))
}

async fn run_server(server_mode: bool) -> io::Result<()> {
    let listener = if server_mode {
        tokio::net::TcpListener::bind(("0.0.0.0", DEFAULT_WEB_PORT)).await?
    } else {
        bind_web_listener().await?
    };
    let address = listener.local_addr()?;
    let state = Arc::new(AppState {
        last_poll: Mutex::new(Instant::now()),
        auth: Mutex::new(AuthState::default()),
        generation: Mutex::new(GenerationState::default()),
        callback_url: format!("http://localhost:{}/api/auth/callback", address.port()),
        server_mode,
    });
    let app = Router::new()
        .route("/", get(index))
        .route("/api/poll", get(poll))
        .route("/api/browse-folder", post(browse_folder))
        .route("/api/generate", post(generate))
        .route("/api/auth-status", get(auth_status))
        .route("/api/auth/start", get(start_auth))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/callback", get(auth_callback))
        .route("/api/models", get(models))
        .with_state(Arc::clone(&state));

    if server_mode {
        println!("Server listening on http://0.0.0.0:{}/", address.port());
        axum::serve(listener, app).await.map_err(io::Error::other)
    } else {
        let url = format!("http://{address}/");
        println!("Web page: {url}");
        open_browser(&url)?;
        axum::serve(listener, app)
            .with_graceful_shutdown(stop_when_idle(state))
            .await
            .map_err(io::Error::other)
    }
}

async fn bind_web_listener() -> io::Result<tokio::net::TcpListener> {
    for port in DEFAULT_WEB_PORT..=u16::MAX {
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
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

async fn index(State(state): State<Arc<AppState>>) -> Html<String> {
    Html(crate::page::render_web_index(state.server_mode))
}

#[derive(Serialize)]
struct GenerationPoll {
    id: u64,
    status: &'static str,
    graph: Option<serde_json::Value>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct PollQuery {
    after: Option<u64>,
}

async fn poll(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PollQuery>,
) -> Result<Json<GenerationPoll>, (StatusCode, String)> {
    *state.last_poll.lock().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "poll timestamp unavailable".to_owned(),
        )
    })? = Instant::now();
    let generation = state.generation.lock().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "generation state unavailable".to_owned(),
        )
    })?;
    let (status, graph, error) = match &generation.status {
        GenerationStatus::Idle => ("idle", None, None),
        GenerationStatus::Running => ("running", None, None),
        GenerationStatus::Ready(graph) => (
            "ready",
            (query.after != Some(generation.id)).then(|| graph.clone()),
            None,
        ),
        GenerationStatus::Failed(error) => ("failed", None, Some(error.clone())),
    };
    Ok(Json(GenerationPoll {
        id: generation.id,
        status,
        graph,
        error,
    }))
}

#[derive(Serialize)]
struct BrowseFolderResponse {
    path: Option<String>,
}

async fn browse_folder() -> Result<Json<BrowseFolderResponse>, (StatusCode, String)> {
    let result = tokio::task::spawn_blocking(pick_folder)
        .await
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("folder picker failed: {error}"),
            )
        })?
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(BrowseFolderResponse {
        path: result.map(|path| path.display().to_string()),
    }))
}

fn pick_folder() -> io::Result<Option<PathBuf>> {
    #[cfg(target_os = "windows")]
    {
        return Ok(rfd::FileDialog::new()
            .set_title("Choose a code folder")
            .pick_folder());
    }

    #[cfg(not(target_os = "windows"))]
    {
        #[cfg(target_os = "macos")]
        let output = Command::new("osascript")
            .args([
                "-e",
                "POSIX path of (choose folder with prompt \"Choose a code folder\")",
            ])
            .stdin(Stdio::null())
            .output()?;

        #[cfg(all(unix, not(target_os = "macos")))]
        let output = Command::new("zenity")
            .args([
                "--file-selection",
                "--directory",
                "--title=Choose a code folder",
            ])
            .stdin(Stdio::null())
            .output()?;

        if !output.status.success() {
            #[cfg(target_os = "macos")]
            if output.status.code() == Some(1) {
                return Ok(None);
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            if output.status.code() == Some(1) {
                return Ok(None);
            }
            return Err(io::Error::other(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }

        let selected = String::from_utf8_lossy(&output.stdout)
            .trim()
            .trim_start_matches('\u{feff}')
            .trim()
            .to_owned();
        if selected.is_empty() {
            Ok(None)
        } else {
            Ok(Some(PathBuf::from(selected)))
        }
    }
}

fn is_url(source: &str) -> bool {
    let bytes = source.as_bytes();
    let is_windows_drive_path = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    !is_windows_drive_path && Url::parse(source).is_ok()
}

#[derive(Serialize)]
struct GenerateResponse {
    status: &'static str,
    id: u64,
}

#[derive(Deserialize)]
struct GenerateRequest {
    source: String,
    model: String,
}

async fn generate(
    State(state): State<Arc<AppState>>,
    Json(request): Json<GenerateRequest>,
) -> Result<Json<GenerateResponse>, (StatusCode, String)> {
    let source = request.source.trim();
    if source.is_empty() || source.len() > 4096 {
        return Err((
            StatusCode::BAD_REQUEST,
            "choose a folder before generating".to_owned(),
        ));
    }
    if is_url(source) {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "URL sources are not implemented yet".to_owned(),
        ));
    }
    let path = PathBuf::from(source).canonicalize().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "folder path is unavailable".to_owned(),
        )
    })?;
    if !path.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            "selected path is not a folder".to_owned(),
        ));
    }
    if request.model.trim().is_empty() || request.model.len() > 200 {
        return Err((
            StatusCode::BAD_REQUEST,
            "select a valid model before generating".to_owned(),
        ));
    }
    let model = request.model;
    let user_key = state
        .auth
        .lock()
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "auth state unavailable".to_owned(),
            )
        })?
        .user_api_key
        .clone();
    let id = {
        let mut generation = state.generation.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "generation state unavailable".to_owned(),
            )
        })?;
        if matches!(&generation.status, GenerationStatus::Running) {
            return Err((
                StatusCode::CONFLICT,
                "generation is already running".to_owned(),
            ));
        }
        generation.id += 1;
        generation.status = GenerationStatus::Running;
        generation.id
    };
    let task_state = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        let result = generate_graph(&path, &model, user_key.as_deref())
            .and_then(|graph| crate::page::serialize_positioned_graph(&graph))
            .and_then(|json| serde_json::from_str(&json).map_err(io::Error::other))
            .map_err(|error| error.to_string());
        if let Ok(mut generation) = task_state.generation.lock()
            && generation.id == id
        {
            generation.status = match result {
                Ok(graph) => GenerationStatus::Ready(graph),
                Err(error) => GenerationStatus::Failed(error),
            };
        }
    });
    Ok(Json(GenerateResponse {
        status: "running",
        id,
    }))
}

fn generate_graph(
    path: &std::path::Path,
    model: &str,
    user_key: Option<&str>,
) -> io::Result<crate::graph_parser::Graph> {
    let source_files = crate::files::read_directory_files(path)?;
    let api_key = match user_key {
        Some(key) => key.to_owned(),
        None => crate::llm::OpenRouter::configured_api_key()
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::other("configure OpenRouter authentication first"))?,
    };
    let llm = crate::llm::OpenRouter::new(api_key, model);
    let mut steps = crate::step::StepStack::with_files(source_files);
    let mut step_id = steps
        .call(
            &llm,
            vec![crate::step::Message::user(crate::prompts::nodegraph())],
        )
        .map_err(io::Error::other)?;
    let mut retries = 0;
    loop {
        let output = &steps
            .step(step_id)
            .expect("the latest generation step exists")
            .response
            .content;
        match crate::graph_parser::validate(output) {
            Ok(()) => {
                return crate::graph_parser::parse(output).map_err(|diagnostic| {
                    io::Error::new(io::ErrorKind::InvalidData, diagnostic.correction_details())
                });
            }
            Err(diagnostic) => {
                if retries == MAX_RETRIES {
                    return Err(io::Error::other(format!(
                        "could not generate a valid graph after {MAX_RETRIES} corrections: {}",
                        diagnostic.correction_details()
                    )));
                }
                let correction_prompt = match diagnostic.kind {
                    crate::graph_parser::DiagnosticKind::Syntax => {
                        include_str!("../prompts/syntax_error.txt")
                    }
                    crate::graph_parser::DiagnosticKind::RedundantConnection => {
                        include_str!("../prompts/redundant_connection.txt")
                    }
                };
                step_id = steps
                    .call(
                        &llm,
                        vec![crate::step::Message::user(format!(
                            "{correction_prompt}\n\n{}",
                            diagnostic.correction_details()
                        ))],
                    )
                    .map_err(io::Error::other)?;
                retries += 1;
            }
        }
    }
}

#[derive(Serialize)]
struct AuthStatus {
    status: &'static str,
}

async fn auth_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<AuthStatus>, (StatusCode, String)> {
    auth_status_response(&state).map(Json)
}

async fn logout(
    State(state): State<Arc<AppState>>,
) -> Result<Json<AuthStatus>, (StatusCode, String)> {
    {
        let mut auth = state.auth.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "auth state unavailable".to_owned(),
            )
        })?;
        auth.user_api_key = None;
        auth.pending = None;
    }
    *state.last_poll.lock().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "poll state unavailable".to_owned(),
        )
    })? = Instant::now();
    auth_status_response(&state).map(Json)
}

fn auth_status_response(state: &AppState) -> Result<AuthStatus, (StatusCode, String)> {
    let has_user_key = state
        .auth
        .lock()
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "auth state unavailable".to_owned(),
            )
        })?
        .user_api_key
        .is_some();
    let has_default_key = crate::llm::OpenRouter::is_configured()
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(AuthStatus {
        status: if has_user_key {
            "user"
        } else if has_default_key {
            "default"
        } else {
            "not found"
        },
    })
}

async fn start_auth(State(state): State<Arc<AppState>>) -> Result<Redirect, (StatusCode, String)> {
    let verifier = random_token()?;
    let challenge = code_challenge(&verifier);
    let oauth_state = random_token()?;
    state
        .auth
        .lock()
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "auth state unavailable".to_owned(),
            )
        })?
        .pending = Some(PendingAuth {
        verifier,
        state: oauth_state.clone(),
        started_at: Instant::now(),
    });

    let mut url = Url::parse("https://openrouter.ai/auth")
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    url.query_pairs_mut()
        .append_pair("callback_url", &state.callback_url)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &oauth_state)
        .append_pair("key_label", "CodeMap");
    Ok(Redirect::temporary(url.as_str()))
}

#[derive(Deserialize)]
struct AuthCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
struct AuthKeyRequest<'a> {
    code: &'a str,
    code_verifier: &'a str,
    code_challenge_method: &'static str,
}

#[derive(Deserialize)]
struct AuthKeyResponse {
    key: String,
}

async fn auth_callback(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<AuthCallback>,
) -> Result<Redirect, (StatusCode, String)> {
    if query.error.is_some() {
        let mut auth = state.auth.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "auth state unavailable".to_owned(),
            )
        })?;
        auth.pending = None;
        *state.last_poll.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "poll state unavailable".to_owned(),
            )
        })? = Instant::now();
        return Ok(Redirect::temporary("/?auth=denied"));
    }
    let Some(callback_state) = query.state.as_deref() else {
        return Err((StatusCode::BAD_REQUEST, "missing OAuth state".to_owned()));
    };
    let (verifier, expected_state) = {
        let auth = state.auth.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "auth state unavailable".to_owned(),
            )
        })?;
        let Some(pending) = auth.pending.as_ref() else {
            return Err((
                StatusCode::BAD_REQUEST,
                "no OAuth request is pending".to_owned(),
            ));
        };
        if pending.started_at.elapsed() >= AUTH_CODE_TTL || pending.state != callback_state {
            return Err((
                StatusCode::BAD_REQUEST,
                "OAuth state is invalid or expired".to_owned(),
            ));
        }
        (pending.verifier.clone(), pending.state.clone())
    };

    let code = query
        .code
        .as_deref()
        .filter(|code| !code.is_empty())
        .ok_or((StatusCode::BAD_REQUEST, "missing OAuth code".to_owned()))?;

    let response = Client::new()
        .post("https://openrouter.ai/api/v1/auth/keys")
        .json(&AuthKeyRequest {
            code,
            code_verifier: &verifier,
            code_challenge_method: "S256",
        })
        .send()
        .await
        .map_err(|_| {
            (
                StatusCode::BAD_GATEWAY,
                "OpenRouter key exchange failed".to_owned(),
            )
        })?;
    if !response.status().is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            "OpenRouter rejected the authorization code".to_owned(),
        ));
    }
    let key: AuthKeyResponse = response.json().await.map_err(|_| {
        (
            StatusCode::BAD_GATEWAY,
            "invalid OpenRouter key response".to_owned(),
        )
    })?;
    if key.key.trim().is_empty() {
        return Err((
            StatusCode::BAD_GATEWAY,
            "OpenRouter returned an empty API key".to_owned(),
        ));
    }

    let mut auth = state.auth.lock().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "auth state unavailable".to_owned(),
        )
    })?;
    if !auth
        .pending
        .as_ref()
        .is_some_and(|pending| pending.state == expected_state)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "OAuth request was replaced".to_owned(),
        ));
    }
    auth.user_api_key = Some(key.key);
    auth.pending = None;
    *state.last_poll.lock().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "poll state unavailable".to_owned(),
        )
    })? = Instant::now();
    Ok(Redirect::temporary("/?auth=success"))
}

fn random_token() -> Result<String, (StatusCode, String)> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "secure randomness unavailable".to_owned(),
        )
    })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[derive(Serialize, serde::Deserialize)]
struct ModelOption {
    id: String,
    name: String,
    is_free: bool,
}

#[derive(serde::Deserialize)]
struct ModelCatalog {
    data: Vec<ApiModel>,
}

#[derive(serde::Deserialize)]
struct ApiModel {
    id: String,
    name: String,
    architecture: ModelArchitecture,
    #[serde(default)]
    pricing: ModelPricing,
}

#[derive(serde::Deserialize)]
struct ModelArchitecture {
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
}

impl ApiModel {
    fn is_regular_text_model(&self) -> bool {
        self.architecture
            .input_modalities
            .iter()
            .any(|modality| modality == "text")
            && self
                .architecture
                .output_modalities
                .iter()
                .any(|modality| modality == "text")
            && !self.id.ends_with(":batch")
            && !self.id.ends_with(":completion")
    }
}

#[derive(Default, serde::Deserialize)]
struct ModelPricing {
    prompt: Option<String>,
    completion: Option<String>,
}

impl ModelPricing {
    fn is_free(&self) -> bool {
        [self.prompt.as_deref(), self.completion.as_deref()]
            .into_iter()
            .all(|price| price.and_then(|price| price.parse::<f64>().ok()) == Some(0.0))
    }
}

async fn models(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Vec<ModelOption>>, (StatusCode, String)> {
    if query.get("api").map(String::as_str) != Some("openrouter") {
        return Err((StatusCode::BAD_REQUEST, "unsupported API".to_owned()));
    }

    let user_key = state
        .auth
        .lock()
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "auth state unavailable".to_owned(),
            )
        })?
        .user_api_key
        .clone();
    let api_key = match user_key {
        Some(key) => Some(key),
        None => crate::llm::OpenRouter::configured_api_key()
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?,
    };
    let client = Client::new();
    let mut request = client
        .get("https://openrouter.ai/api/v1/models")
        .query(&[("input_modalities", "text"), ("output_modalities", "text")]);
    if let Some(api_key) = api_key {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error.to_string()))?;
    if !response.status().is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("OpenRouter returned {}", response.status()),
        ));
    }

    let mut catalog: ModelCatalog = response
        .json()
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error.to_string()))?;
    let mut models = catalog
        .data
        .drain(..)
        .filter(ApiModel::is_regular_text_model)
        .map(|model| ModelOption {
            id: model.id,
            name: model.name,
            is_free: model.pricing.is_free(),
        })
        .collect::<Vec<_>>();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(Json(models))
}

async fn stop_when_idle(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    loop {
        interval.tick().await;
        let last_poll = *state
            .last_poll
            .lock()
            .expect("poll timestamp lock poisoned");
        let auth_pending = state
            .auth
            .lock()
            .expect("auth state lock poisoned")
            .pending
            .as_ref()
            .is_some_and(|pending| pending.started_at.elapsed() < AUTH_CODE_TTL);
        if last_poll.elapsed() >= IDLE_TIMEOUT && !auth_pending {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ApiModel, AppState, GenerationState, GenerationStatus, ModelArchitecture, ModelPricing,
        PollQuery, code_challenge, is_url, poll,
    };
    use axum::extract::{Query, State};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    #[test]
    fn creates_the_rfc_7636_s256_challenge() {
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn distinguishes_urls_from_windows_paths() {
        assert!(is_url("https://example.com/repo"));
        assert!(!is_url(r"C:\Users\user\project"));
        assert!(!is_url("/home/user/project"));
    }

    #[tokio::test]
    async fn polling_returns_a_completed_graph_once_per_generation_id() {
        let graph = serde_json::json!({"nodes": [], "edges": []});
        let state = Arc::new(AppState {
            last_poll: Mutex::new(Instant::now()),
            auth: Mutex::new(super::AuthState::default()),
            generation: Mutex::new(GenerationState {
                id: 4,
                status: GenerationStatus::Ready(graph.clone()),
            }),
            callback_url: String::new(),
            server_mode: false,
        });

        let first = poll(State(Arc::clone(&state)), Query(PollQuery { after: None }))
            .await
            .unwrap()
            .0;
        assert_eq!(first.status, "ready");
        assert_eq!(first.graph, Some(graph));

        let repeated = poll(State(state), Query(PollQuery { after: Some(4) }))
            .await
            .unwrap()
            .0;
        assert_eq!(repeated.status, "ready");
        assert_eq!(repeated.graph, None);
    }

    #[test]
    fn detects_free_models_from_both_token_prices() {
        assert!(
            ModelPricing {
                prompt: Some("0".to_owned()),
                completion: Some("0.0".to_owned()),
            }
            .is_free()
        );
        assert!(
            !ModelPricing {
                prompt: Some("0".to_owned()),
                completion: Some("0.000001".to_owned()),
            }
            .is_free()
        );
        assert!(
            !ModelPricing {
                prompt: None,
                completion: Some("0".to_owned()),
            }
            .is_free()
        );
    }

    #[test]
    fn keeps_only_regular_text_chat_models() {
        let regular_model = ApiModel {
            id: "example/text-chat".to_owned(),
            name: "Text chat".to_owned(),
            architecture: ModelArchitecture {
                input_modalities: vec!["text".to_owned()],
                output_modalities: vec!["text".to_owned()],
            },
            pricing: ModelPricing::default(),
        };
        assert!(regular_model.is_regular_text_model());

        let multimodal_model = ApiModel {
            id: "example/multimodal-chat".to_owned(),
            name: "Multimodal chat".to_owned(),
            architecture: ModelArchitecture {
                input_modalities: vec!["text".to_owned(), "image".to_owned()],
                output_modalities: vec!["text".to_owned()],
            },
            pricing: ModelPricing::default(),
        };
        assert!(multimodal_model.is_regular_text_model());

        let batch_model = ApiModel {
            id: "example/text-chat:batch".to_owned(),
            name: "Text chat batch".to_owned(),
            architecture: regular_model.architecture,
            pricing: ModelPricing::default(),
        };
        assert!(!batch_model.is_regular_text_model());
    }
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
