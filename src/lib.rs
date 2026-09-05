//! Collects Markdown from a set of git repositories and publishes it to Google
//! Drive as Google Docs, preserving the original directory structure as a
//! heading outline so the result is navigable in NotebookLM.

pub mod clone;
pub mod collect;
pub mod config;
pub mod drive;
pub mod render;
pub mod sync;
