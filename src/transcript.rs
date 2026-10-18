//! Conversation turns, read from the files each tool already writes.
//!
//! Claude Code keeps one JSONL file per conversation under
//! `~/.claude/projects/<cwd slug>/<session id>.jsonl`; Codex keeps rollout
//! files under `~/.codex/sessions`. Only user and assistant text is kept.
//! Unknown records are skipped, so a newer format yields fewer turns rather
//! than an error.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::model::text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub text: String,
}

/// Messages the tools inject on the user's side — command wrappers,
/// reminders, environment blocks. They start with a tag.
fn is_injected(s: &str) -> bool {
    s.trim_start().starts_with('<')
}

/// Sanitize each line but keep the line breaks, which the panel wraps on.
fn push(turns: &mut Vec<Turn>, role: Role, raw: &str) {
    if role == Role::User && is_injected(raw) {
        return;
    }
    let text = raw
        .trim()
        .lines()
        .map(text::sanitize)
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() {
        turns.push(Turn { role, text });
    }
}

fn records(reader: impl BufRead) -> impl Iterator<Item = Value> {
    reader
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
}

/// Parse a Claude Code conversation file.
pub fn parse_claude(reader: impl BufRead) -> Vec<Turn> {
    let mut turns = Vec::new();
    for v in records(reader) {
        let role = match v.get("type").and_then(Value::as_str) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => continue,
        };
        // A subagent's turns and tool-injected messages are not the conversation.
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || v.get("isMeta").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        match v.pointer("/message/content") {
            Some(Value::String(s)) => push(&mut turns, role, s),
            // Only text blocks; tool calls, tool results and thinking are dropped.
            Some(Value::Array(blocks)) => blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .for_each(|s| push(&mut turns, role, s)),
            _ => {}
        }
    }
    turns
}

/// Parse a Codex rollout file from its `response_item` messages, the one
/// record kind every rollout format seen so far writes. (Newer CLI rollouts
/// no longer write the `event_msg` user/agent messages.)
pub fn parse_codex(reader: impl BufRead) -> Vec<Turn> {
    let mut turns = Vec::new();
    for v in records(reader) {
        if v.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }
        let Some(p) = v
            .get("payload")
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("message"))
        else {
            continue;
        };
        let role = match p.get("role").and_then(Value::as_str) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            // developer and system prompts are not the conversation.
            _ => continue,
        };
        for b in p.get("content").and_then(Value::as_array).into_iter().flatten() {
            push(
                &mut turns,
                role,
                b.get("text").and_then(Value::as_str).unwrap_or(""),
            );
        }
    }
    turns
}

/// Read `path` with `parse` and keep the last `max_turns` turns.
pub fn read_tail(
    path: &Path,
    parse: fn(std::io::BufReader<std::fs::File>) -> Vec<Turn>,
    max_turns: usize,
) -> std::io::Result<Vec<Turn>> {
    let mut turns = parse(std::io::BufReader::new(std::fs::File::open(path)?));
    Ok(turns.split_off(turns.len().saturating_sub(max_turns)))
}

pub fn default_claude_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude"))
}

/// Find `<session id>.jsonl` under any project directory. The directory is
/// named after the starting cwd, but a session that moved into a worktree
/// keeps its file elsewhere, so every directory is checked.
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

    fn pairs(turns: &[Turn]) -> Vec<(Role, &str)> {
        turns.iter().map(|t| (t.role, t.text.as_str())).collect()
    }

    #[test]
    fn claude_keeps_user_and_assistant_text() {
        let body = [
            r#"{"type":"permission-mode","permissionMode":"auto"}"#,
            r#"{"type":"user","message":{"role":"user","content":"fix the bug"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"hidden"}}"#,
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"..."}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking."},{"type":"tool_use","name":"Bash"}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":"subagent"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Fixed."}]}}"#,
            "not json",
        ]
        .join("\n");
        assert_eq!(
            pairs(&parse_claude(body.as_bytes())),
            vec![
                (Role::User, "fix the bug"),
                (Role::Assistant, "Looking."),
                (Role::Assistant, "Fixed.")
            ]
        );
    }

    /// Shapes from a codex-cli rollout captured on 2026-10-18.
    #[test]
    fn codex_reads_response_item_messages() {
        let body = [
            r#"{"type":"session_meta","payload":{"session_id":"t1"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"rules"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add a test"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Added."}]}}"#,
        ]
        .join("\n");
        assert_eq!(
            pairs(&parse_codex(body.as_bytes())),
            vec![(Role::User, "add a test"), (Role::Assistant, "Added.")]
        );
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
