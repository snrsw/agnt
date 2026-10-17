//! Conversation transcripts, read from the files each tool already writes.
//!
//! Claude Code keeps one JSONL file per conversation under
//! `~/.claude/projects/<cwd slug>/<session id>.jsonl`. Codex keeps rollout
//! files under `~/.codex/sessions/<y>/<m>/<d>/rollout-*.jsonl`. Both are
//! read-only here, and only the user-visible turns are kept: tool output,
//! hidden reasoning and bookkeeping records are dropped, and a tool call is
//! reduced to its name.
//!
//! The record shapes were captured from real files on 2026-10-18 (Claude Code
//! with `ai-title` records, codex-cli rollouts with `event_msg` records).
//! Unknown records are skipped, so a newer format degrades to fewer turns
//! rather than an error.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::model::text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    /// A tool call by the assistant. `text` is the tool's name.
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub text: String,
}

/// The visible part of a conversation, newest turn last.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Transcript {
    pub turns: Vec<Turn>,
    /// A summary the tool itself wrote (Claude's `ai-title`), if any.
    pub summary: Option<String>,
}

impl Transcript {
    /// Keep only the last `n` turns.
    pub fn tail(mut self, n: usize) -> Self {
        let drop = self.turns.len().saturating_sub(n);
        self.turns.drain(..drop);
        self
    }

    /// The last thing the assistant said, for a one-line excerpt.
    pub fn last_assistant(&self) -> Option<&str> {
        self.turns
            .iter()
            .rev()
            .find(|t| t.role == Role::Assistant)
            .map(|t| t.text.as_str())
    }
}

/// Messages the tools inject into the user's side of the conversation —
/// command wrappers, reminders, environment blocks. They start with a tag.
fn is_injected(s: &str) -> bool {
    s.trim_start().starts_with('<')
}

/// Sanitize each line but keep the line breaks, which the panel wraps on;
/// runs of blank lines become one.
fn clean(raw: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in raw.trim().lines() {
        let line = text::sanitize(line);
        if line.is_empty() && out.last().is_some_and(|l| l.is_empty()) {
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

fn push(turns: &mut Vec<Turn>, role: Role, raw: &str) {
    let text = clean(raw);
    if text.is_empty() {
        return;
    }
    turns.push(Turn { role, text });
}

/// Parse a Claude Code conversation file.
pub fn parse_claude(reader: impl BufRead) -> Transcript {
    let mut out = Transcript::default();
    for line in reader.lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "ai-title" {
            out.summary = v.get("aiTitle").and_then(Value::as_str).map(text::sanitize);
            continue;
        }
        if kind != "user" && kind != "assistant" {
            continue;
        }
        // A subagent's turns and tool-injected messages are not the conversation.
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || v.get("isMeta").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let Some(content) = v.pointer("/message/content") else {
            continue;
        };
        let role = if kind == "user" {
            Role::User
        } else {
            Role::Assistant
        };
        match content {
            Value::String(s) if role == Role::User && is_injected(s) => {}
            Value::String(s) => push(&mut out.turns, role, s),
            Value::Array(blocks) => {
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let s = b.get("text").and_then(Value::as_str).unwrap_or("");
                            if !(role == Role::User && is_injected(s)) {
                                push(&mut out.turns, role, s);
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                            push(&mut out.turns, Role::Tool, name);
                        }
                        // tool_result, thinking, images: not shown.
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Parse a Codex rollout file from its `response_item` records.
///
/// These are the one record kind every rollout format seen so far writes.
/// The `event_msg` user/agent messages are cleaner but are missing from newer
/// CLI rollouts, which write `item_completed` events instead. The cost is
/// that injected context (`<environment_context>`, AGENTS.md) arrives as user
/// messages, and is dropped by its leading tag.
pub fn parse_codex(reader: impl BufRead) -> Transcript {
    let mut out = Transcript::default();
    for line in reader.lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }
        let Some(p) = v.get("payload") else { continue };
        match p.get("type").and_then(Value::as_str) {
            Some("message") => {
                let role = match p.get("role").and_then(Value::as_str) {
                    Some("user") => Role::User,
                    Some("assistant") => Role::Assistant,
                    // developer and system prompts are not the conversation.
                    _ => continue,
                };
                let Some(blocks) = p.get("content").and_then(Value::as_array) else {
                    continue;
                };
                for b in blocks {
                    let s = b.get("text").and_then(Value::as_str).unwrap_or("");
                    if !(role == Role::User && is_injected(s)) {
                        push(&mut out.turns, role, s);
                    }
                }
            }
            Some("function_call" | "custom_tool_call" | "local_shell_call") => {
                let name = p.get("name").and_then(Value::as_str).unwrap_or("shell");
                push(&mut out.turns, Role::Tool, name);
            }
            _ => {}
        }
    }
    out
}

pub fn read_file(
    path: &Path,
    parse: fn(std::io::BufReader<std::fs::File>) -> Transcript,
) -> std::io::Result<Transcript> {
    let file = std::fs::File::open(path)?;
    Ok(parse(std::io::BufReader::new(file)))
}

pub fn default_claude_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude"))
}

/// Find `<session id>.jsonl` under any project directory.
///
/// The directory is named after the cwd the session started in, but a session
/// that moved into a worktree keeps its file elsewhere, so every project
/// directory is checked rather than deriving one from the cwd.
pub fn find_claude(home: &Path, session_id: &str) -> Option<PathBuf> {
    let name = format!("{session_id}.jsonl");
    std::fs::read_dir(home.join("projects"))
        .ok()?
        .flatten()
        .map(|e| e.path().join(&name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_keeps_visible_turns_and_names_tools() {
        let body = [
            r#"{"type":"permission-mode","permissionMode":"auto"}"#,
            r#"{"type":"user","isSidechain":false,"message":{"role":"user","content":"fix the bug"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"hidden"}}"#,
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"..."}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Looking."}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{}}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":"subagent"}}"#,
            r#"{"type":"ai-title","aiTitle":"bug fix"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Fixed."}]}}"#,
            "not json",
        ]
        .join("\n");
        let t = parse_claude(body.as_bytes());
        let got: Vec<(Role, &str)> = t.turns.iter().map(|t| (t.role, t.text.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (Role::User, "fix the bug"),
                (Role::Assistant, "Looking."),
                (Role::Tool, "Bash"),
                (Role::Assistant, "Fixed."),
            ]
        );
        assert_eq!(t.summary.as_deref(), Some("bug fix"));
        assert_eq!(t.last_assistant(), Some("Fixed."));
        assert_eq!(t.tail(1).turns.len(), 1);
    }

    /// Shapes from a codex-cli rollout captured on 2026-10-18, which has no
    /// `event_msg` user/agent messages.
    #[test]
    fn codex_reads_response_items() {
        let body = [
            r#"{"type":"session_meta","payload":{"session_id":"t1"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"rules"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add a test"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"..."}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","output":[]}}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Added."}]}}"#,
        ]
        .join("\n");
        let t = parse_codex(body.as_bytes());
        let got: Vec<(Role, &str)> = t.turns.iter().map(|t| (t.role, t.text.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (Role::User, "add a test"),
                (Role::Tool, "exec"),
                (Role::Assistant, "Added.")
            ]
        );
        assert_eq!(t.summary, None);
    }

    #[test]
    fn claude_file_is_found_in_any_project_directory() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("projects").join("-somewhere-else");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("abc.jsonl"), "").unwrap();
        assert_eq!(find_claude(home.path(), "abc"), Some(dir.join("abc.jsonl")));
        assert_eq!(find_claude(home.path(), "missing"), None);
    }
}
