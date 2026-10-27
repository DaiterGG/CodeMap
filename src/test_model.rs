//! Client for the dedicated test model served by the compatible Chat Completions API.

use crate::step::{ChatClient, Message};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::env;
use std::error::Error;
use std::io;
use std::path::Path;
use std::time::Duration;

const CHAT_COMPLETIONS_URL: &str = "https://ai.dfjk.ru/v1/chat/completions";
const MODEL_ID: &str = "hyperqwen/qwen3.8-27b";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

pub struct TestModel {
    client: Client,
    api_key: String,
}

impl TestModel {
    pub fn from_env() -> io::Result<Self> {
        load_env_file()?;
        let api_key = env::var("TEST_TOKEN")
            .map_err(|_| io::Error::other("set TEST_TOKEN in the environment or .env file"))?;
        if api_key.trim().is_empty() {
            return Err(io::Error::other("TEST_TOKEN is empty"));
        }

        Ok(Self {
            client: Client::new(),
            api_key,
        })
    }

    pub fn complete(&self, messages: &[Message]) -> Result<Message, Box<dyn Error + Send + Sync>> {
        let request = ChatCompletionRequest {
            model: MODEL_ID,
            messages,
        };
        let response = self
            .client
            .post(CHAT_COMPLETIONS_URL)
            .timeout(REQUEST_TIMEOUT)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .map_err(|error| {
                let kind = if error.is_timeout() {
                    "timed out"
                } else if error.is_connect() {
                    "could not connect"
                } else if error.is_request() {
                    "could not send request"
                } else {
                    "transport failed"
                };
                io::Error::other(format!("test model request {kind}: {error:?}"))
            })?;

        let status = response.status();
        let body = response.text()?;
        if !status.is_success() {
            return Err(Box::new(io::Error::other(format!(
                "test model returned {status}: {body}"
            ))));
        }

        let response: ChatCompletionResponse = serde_json::from_str(&body)?;
        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::other("test model response contained no choices"))?;
        let content = choice
            .message
            .content
            .ok_or_else(|| io::Error::other("test model response contained no message content"))?;

        Ok(Message {
            role: choice.message.role,
            content,
            reasoning: choice.message.reasoning,
        })
    }
}

impl ChatClient for TestModel {
    fn complete(&self, messages: &[Message]) -> Result<Message, Box<dyn Error + Send + Sync>> {
        TestModel::complete(self, messages)
    }
}

fn load_env_file() -> io::Result<()> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root_env_path = manifest_dir.join(".env");
    let parent_env_path = manifest_dir.parent().map(|parent| parent.join(".env"));
    let env_path = if root_env_path.is_file() {
        Some(root_env_path)
    } else {
        parent_env_path.filter(|path| path.is_file())
    };

    if let Some(path) = env_path {
        dotenvy::from_path(path).map_err(io::Error::other)?;
    }
    Ok(())
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'static str,
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
    #[serde(default = "assistant_role")]
    role: crate::step::Role,
    content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
}

fn assistant_role() -> crate::step::Role {
    crate::step::Role::Assistant
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_chat_completion_content_and_reasoning() {
        let response: ChatCompletionResponse = serde_json::from_str(
            r#"{"choices":[{"message":{"role":"assistant","content":"graph","reasoning":"plan"}}]}"#,
        )
        .unwrap();

        let message = response.choices.into_iter().next().unwrap().message;
        assert_eq!(message.role, crate::step::Role::Assistant);
        assert_eq!(message.content.as_deref(), Some("graph"));
        assert_eq!(message.reasoning.as_deref(), Some("plan"));
    }

    #[test]
    fn decodes_chat_completion_without_role() {
        let response: ChatCompletionResponse =
            serde_json::from_str(r#"{"choices":[{"message":{"content":"graph"}}]}"#).unwrap();

        let message = response.choices.into_iter().next().unwrap().message;
        assert_eq!(message.role, crate::step::Role::Assistant);
        assert_eq!(message.content.as_deref(), Some("graph"));
    }
}
