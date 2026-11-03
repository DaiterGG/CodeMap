//! Maintains the LLM conversation history, including graph-generation and
//! correction attempts, and supplies that context to each follow-up request.

use crate::files::{SourceFile, format_source_files};
use serde::{Deserialize, Serialize};
use std::error::Error;

pub trait ChatClient: Sync {
    fn complete(&self, messages: &[Message]) -> Result<Message, Box<dyn Error + Send + Sync>>;

    fn context_length(&self) -> Option<usize> {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StepId(pub usize);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(skip)]
    pub reasoning: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            reasoning: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            reasoning: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            reasoning: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub id: StepId,
    pub input: Vec<Message>,
    pub response: Message,
}

#[derive(Debug)]
pub struct StepStack {
    system: SystemContext,
    steps: Vec<Step>,
}

#[derive(Debug)]
enum SystemContext {
    Text(String),
    Files(Vec<SourceFile>),
}

impl StepStack {
    pub fn new(system_prompt: impl Into<String>) -> Self {
        Self {
            system: SystemContext::Text(system_prompt.into()),
            steps: Vec::new(),
        }
    }

    pub fn with_files(files: Vec<SourceFile>) -> Self {
        Self {
            system: SystemContext::Files(files),
            steps: Vec::new(),
        }
    }

    pub fn context(&self) -> Vec<Message> {
        let system_content = match &self.system {
            SystemContext::Text(prompt) => prompt.clone(),
            SystemContext::Files(files) => format_source_files(files),
        };
        let mut messages = vec![Message::system(system_content)];
        for step in &self.steps {
            messages.extend(step.input.iter().cloned());
            messages.push(step.response.clone());
        }

        messages
    }

    pub fn call(
        &mut self,
        llm: &dyn ChatClient,
        input: Vec<Message>,
    ) -> Result<StepId, Box<dyn Error + Send + Sync>> {
        let mut messages = self.context();
        messages.extend(input.iter().cloned());
        let response = llm.complete(&messages)?;

        let id = StepId(self.steps.len());
        self.steps.push(Step {
            id,
            input,
            response,
        });
        Ok(id)
    }

    pub fn undo(&mut self) -> Option<Step> {
        self.steps.pop()
    }

    pub fn step(&self, id: StepId) -> Option<&Step> {
        self.steps.get(id.0)
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    pub fn source_files(&self) -> Option<&[SourceFile]> {
        match &self.system {
            SystemContext::Files(files) => Some(files),
            SystemContext::Text(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn append(stack: &mut StepStack, content: &str) -> StepId {
        let id = StepId(stack.steps.len());
        stack.steps.push(Step {
            id,
            input: vec![Message::user(content)],
            response: Message::assistant(format!("response to {content}")),
        });
        id
    }

    #[test]
    fn undo_removes_the_latest_step_from_history_and_context() {
        let mut stack = StepStack::new("system");
        let first = append(&mut stack, "first");
        append(&mut stack, "second");

        let removed = stack.undo().unwrap();
        assert_eq!(removed.input, vec![Message::user("second")]);
        assert_eq!(stack.steps().len(), 1);
        assert_eq!(
            stack.step(first).unwrap().input,
            vec![Message::user("first")]
        );
        assert_eq!(
            stack.context(),
            vec![
                Message::system("system"),
                Message::user("first"),
                Message::assistant("response to first"),
            ]
        );
    }

    #[test]
    fn a_call_after_undo_continues_the_linear_stack() {
        let mut stack = StepStack::new("system");
        append(&mut stack, "first");
        append(&mut stack, "second");
        stack.undo();
        let next = append(&mut stack, "replacement");

        assert_eq!(next, StepId(1));
        assert_eq!(stack.steps().len(), 2);
        assert_eq!(
            stack.context().last(),
            Some(&Message::assistant("response to replacement"))
        );
    }

    #[test]
    fn file_context_is_formatted_when_request_context_is_built() {
        let stack = StepStack::with_files(vec![SourceFile {
            path: "src".into(),
            name: "main".to_owned(),
            extension: Some("rs".to_owned()),
            contents: "fn main() {}\n".to_owned(),
        }]);

        assert_eq!(
            stack.context(),
            vec![Message::system("``` src/main.rs\n1 fn main() {}\n```\n\n")]
        );
    }
}
