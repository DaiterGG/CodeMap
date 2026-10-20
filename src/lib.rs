//! Core modules for collecting code, generating and validating a custom graph,
//! and exporting it as a Mermaid source file and a custom rendered HTML page.

pub mod files;
pub mod graph_file;
pub mod graph_parser;
pub mod llm;
pub mod mermaid;
pub mod page;
pub mod prompts;
pub mod step;
