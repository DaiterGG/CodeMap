//! Configures the OpenRouter client and sends chat-completion requests used to
//! generate and correct the graph.

use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::env;
use std::error::Error;
use std::fmt;
use std::path::Path;

use crate::step::Message;

const OPENROUTER_CHAT_COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

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

        let api_key = env::var("OPEN_ROUTER_KEY")
            .or_else(|_| env::var("OPEN_ROUTER"))
            .map_err(|_| OpenRouterConfigError::MissingApiKey)?;
        Ok(Self::new(api_key, model))
    }

    pub fn complete(&self, messages: &[Message]) -> Result<Message, OpenRouterError> {
        let request = ChatCompletionRequest {
            model: &self.model,
            messages,
        };

        let response = self
            .client
            .post(OPENROUTER_CHAT_COMPLETIONS_URL)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .map_err(OpenRouterError::Request)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().map_err(OpenRouterError::Request)?;
            return Err(OpenRouterError::ApiStatus { status, body });
        }

        let body = response.text().map_err(OpenRouterError::Request)?;
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
            Self::Request(error) => write!(f, "OpenRouter request failed: {error}"),
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
