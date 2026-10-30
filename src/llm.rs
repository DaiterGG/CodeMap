//! Configures the OpenRouter client and sends chat-completion requests used to
//! generate and correct the graph.

use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::env;
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::step::{ChatClient, Message};

const OPENROUTER_CHAT_COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3600);

pub fn list_model_ids() -> Result<Vec<String>, reqwest::Error> {
    let response = Client::new()
        .get(OPENROUTER_MODELS_URL)
        .timeout(Duration::from_secs(30))
        .send()?
        .error_for_status()?;
    let catalog: ModelCatalog = response.json()?;
    let mut model_ids = catalog
        .data
        .into_iter()
        .map(|model| model.id)
        .collect::<Vec<_>>();
    model_ids.sort();
    Ok(model_ids)
}

pub struct OpenRouter {
    client: Client,
    api_key: String,
    model: String,
}

impl OpenRouter {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }

    pub fn from_env(model: impl Into<String>) -> Result<Self, OpenRouterConfigError> {
        let api_key = api_key_from_env()?;
        Ok(Self::new(api_key, model))
    }

    pub fn is_configured() -> Result<bool, OpenRouterConfigError> {
        Ok(Self::configured_api_key()?.is_some())
    }

    pub fn configured_api_key() -> Result<Option<String>, OpenRouterConfigError> {
        load_env_file()?;
        Ok(env::var("OPEN_ROUTER_KEY")
            .or_else(|_| env::var("OPEN_ROUTER"))
            .ok()
            .filter(|key| !key.trim().is_empty()))
    }

    pub fn complete(&self, messages: &[Message]) -> Result<Message, OpenRouterError> {
        let request = ChatCompletionRequest {
            model: &self.model,
            messages,
        };

        let response = self
            .client
            .post(OPENROUTER_CHAT_COMPLETIONS_URL)
            .timeout(REQUEST_TIMEOUT)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .map_err(OpenRouterError::Request)?;

        let status = response.status();
        if !status.is_success() {
            let body = read_response_body(response).map_err(OpenRouterError::Request)?;
            return Err(OpenRouterError::ApiStatus { status, body });
        }

        let body = read_response_body(response).map_err(OpenRouterError::Request)?;
        let response: ChatCompletionResponse =
            serde_json::from_str(&body).map_err(|error| OpenRouterError::Decode { error, body })?;
        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or(OpenRouterError::MissingChoice)?;
        let content = choice
            .message
            .content
            .ok_or(OpenRouterError::MissingContent)?;

        Ok(Message {
            role: choice.message.role,
            content,
            reasoning: choice.message.reasoning,
        })
    }
}

impl ChatClient for OpenRouter {
    fn complete(&self, messages: &[Message]) -> Result<Message, Box<dyn Error + Send + Sync>> {
        OpenRouter::complete(self, messages)
            .map_err(|error| Box::new(error) as Box<dyn Error + Send + Sync>)
    }
}

fn api_key_from_env() -> Result<String, OpenRouterConfigError> {
    load_env_file()?;
    env::var("OPEN_ROUTER_KEY")
        .or_else(|_| env::var("OPEN_ROUTER"))
        .map_err(|_| OpenRouterConfigError::MissingApiKey)
}

fn load_env_file() -> Result<(), OpenRouterConfigError> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root_env_path = manifest_dir.join(".env");
    let parent_env_path = manifest_dir.parent().map(|parent| parent.join(".env"));
    let env_path = if root_env_path.is_file() {
        Some(root_env_path)
    } else {
        parent_env_path.filter(|path| path.is_file())
    };

    if let Some(path) = env_path {
        dotenvy::from_path(&path).map_err(OpenRouterConfigError::Dotenv)?;
    }
    Ok(())
}

fn read_response_body(response: reqwest::blocking::Response) -> Result<String, reqwest::Error> {
    let started = Instant::now();
    let body = response.text();
    eprintln!(
        "OpenRouter response body fetch took {:.2?}",
        started.elapsed()
    );
    body
}

#[derive(Debug)]
pub enum OpenRouterConfigError {
    Dotenv(dotenvy::Error),
    MissingApiKey,
}

impl fmt::Display for OpenRouterConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dotenv(error) => write!(f, "could not load OpenRouter .env file: {error}"),
            Self::MissingApiKey => {
                write!(
                    f,
                    "set OPEN_ROUTER_KEY or OPEN_ROUTER in the environment or .env file"
                )
            }
        }
    }
}

impl Error for OpenRouterConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Dotenv(error) => Some(error),
            Self::MissingApiKey => None,
        }
    }
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
}

#[derive(Deserialize)]
struct ModelCatalog {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

#[derive(Deserialize)]
struct ChatCompletionChoice {
    message: ChatCompletionMessage,
}

#[derive(Deserialize)]
struct ChatCompletionMessage {
    role: crate::step::Role,
    content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
}

#[derive(Debug)]
pub enum OpenRouterError {
    Request(reqwest::Error),
    ApiStatus {
        status: StatusCode,
        body: String,
    },
    Decode {
        error: serde_json::Error,
        body: String,
    },
    MissingChoice,
    MissingContent,
}

impl fmt::Display for OpenRouterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(error) => write!(f, "OpenRouter request failed: {error:?}"),
            Self::ApiStatus { status, body } => {
                write!(f, "OpenRouter returned {status}: {body}")
            }
            Self::Decode { error, body } => {
                write!(
                    f,
                    "could not decode OpenRouter response: {error}\nResponse body:\n{body}"
                )
            }
            Self::MissingChoice => write!(f, "OpenRouter response contained no choices"),
            Self::MissingContent => write!(f, "OpenRouter response contained no message content"),
        }
    }
}

impl Error for OpenRouterError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Request(error) => Some(error),
            Self::Decode { error, .. } => Some(error),
            Self::ApiStatus { .. } | Self::MissingChoice | Self::MissingContent => None,
        }
    }
}
