use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Which tool owns the conversation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Claude, Provider::Codex];

    /// Stable lowercase identifier used in JSON and on the command line.
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        }
    }

    /// Human-facing name shown in the list.
    pub fn label(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::Codex => "Codex",
        }
    }

    /// A one-cell mark that tells the two apart on a title line.
    ///
    /// Both come from Geometric Shapes, the block the state glyphs are drawn
    /// from, because that block is known to render at one cell on the
    /// terminals this has been tried on; a mark from a rarer block can fall
    /// back to another font and shift the row. The shapes are arbitrary —
    /// neither tool has a brand mark that survives as a single monospace
    /// cell — so the shortcuts sheet carries a legend, and the word stays on
    /// the line below.
    pub fn icon(self) -> &'static str {
        match self {
            Provider::Claude => "◆",
            Provider::Codex => "▲",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Provider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude" => Ok(Provider::Claude),
            "codex" => Ok(Provider::Codex),
            other => Err(format!("unknown provider `{other}` (expected claude or codex)")),
        }
    }
}

/// Permanent identity of a conversation: `provider + scope + native_session_id`.
///
/// `scope` names a stable local profile or storage area, never a PID or a
/// socket name. Runtime handles (job IDs, PIDs, server connections) live in
/// [`super::RuntimeHandle`], not here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionKey {
    pub provider: Provider,
    pub scope: String,
    pub native_session_id: String,
}

impl SessionKey {
    pub const DEFAULT_SCOPE: &'static str = "default";

    pub fn new(provider: Provider, native_session_id: impl Into<String>) -> Self {
        Self {
            provider,
            scope: Self::DEFAULT_SCOPE.to_string(),
            native_session_id: native_session_id.into(),
        }
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = scope.into();
        self
    }

    /// Display-only abbreviation. Never used for lookup on its own; see [`resolve_key`].
    pub fn short_id(&self) -> String {
        self.native_session_id.chars().take(8).collect()
    }

    /// Fully qualified textual form: `provider:scope:native_session_id`.
    pub fn qualified(&self) -> String {
        format!("{}:{}:{}", self.provider, self.scope, self.native_session_id)
    }

    /// Parse `provider:native_id` or `provider:scope:native_id`.
    /// Returns `None` when the text is not in a qualified form.
    pub fn parse_qualified(text: &str) -> Option<SessionKey> {
        let mut parts = text.splitn(3, ':');
        let provider = parts.next()?.parse::<Provider>().ok()?;
        let second = parts.next()?;
        match parts.next() {
            Some(native) if !native.is_empty() && !second.is_empty() => {
                Some(SessionKey::new(provider, native).with_scope(second))
            }
            None if !second.is_empty() => Some(SessionKey::new(provider, second)),
            _ => None,
        }
    }
}

impl fmt::Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.qualified())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdResolveError {
    #[error("no session matches `{0}`")]
    NotFound(String),
    #[error("`{query}` is ambiguous; candidates: {}", candidates.iter().map(SessionKey::qualified).collect::<Vec<_>>().join(", "))]
    Ambiguous {
        query: String,
        candidates: Vec<SessionKey>,
    },
}

/// Resolve user-typed text to exactly one key.
///
/// Accepts a qualified key, an exact native ID, or a unique prefix of a native
/// ID (optionally as `provider:prefix`). A prefix shared by several sessions is
/// an error rather than a guess.
pub fn resolve_key<'a>(
    known: impl IntoIterator<Item = &'a SessionKey>,
    query: &str,
) -> Result<SessionKey, IdResolveError> {
    let known: Vec<&SessionKey> = known.into_iter().collect();

    if let Some(full) = SessionKey::parse_qualified(query)
        && known.contains(&&full)
    {
        return Ok(full);
    }

    let (provider, id_part) = match query.split_once(':') {
        Some((p, rest)) => match p.parse::<Provider>() {
            Ok(provider) => (Some(provider), rest),
            Err(_) => (None, query),
        },
        None => (None, query),
    };
    if id_part.is_empty() {
        return Err(IdResolveError::NotFound(query.to_string()));
    }

    let scoped = known
        .iter()
        .copied()
        .filter(|k| provider.is_none_or(|p| k.provider == p));

    let exact: Vec<&SessionKey> = scoped
        .clone()
        .filter(|k| k.native_session_id == id_part)
        .collect();
    match exact.as_slice() {
        [one] => return Ok((*one).clone()),
        [_, _, ..] => {
            return Err(IdResolveError::Ambiguous {
                query: query.to_string(),
                candidates: exact.into_iter().cloned().collect(),
            });
        }
        [] => {}
    }

    let prefixed: Vec<&SessionKey> = scoped
        .filter(|k| k.native_session_id.starts_with(id_part))
        .collect();
    match prefixed.as_slice() {
        [] => Err(IdResolveError::NotFound(query.to_string())),
        [one] => Ok((*one).clone()),
        _ => Err(IdResolveError::Ambiguous {
            query: query.to_string(),
            candidates: prefixed.into_iter().cloned().collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Vec<SessionKey> {
        vec![
            SessionKey::new(Provider::Claude, "abc12345-aaaa"),
            SessionKey::new(Provider::Claude, "abc99999-bbbb"),
            SessionKey::new(Provider::Codex, "abc12345-aaaa"),
            SessionKey::new(Provider::Codex, "zzz00000-cccc"),
        ]
    }

    #[test]
    fn unique_prefix_resolves() {
        let k = keys();
        assert_eq!(resolve_key(&k, "zzz").unwrap(), k[3]);
    }

    #[test]
    fn shared_prefix_is_ambiguous_not_first_match() {
        let k = keys();
        let err = resolve_key(&k, "abc").unwrap_err();
        assert!(matches!(err, IdResolveError::Ambiguous { ref candidates, .. } if candidates.len() == 3));
    }

    #[test]
    fn provider_prefix_narrows_candidates() {
        let k = keys();
        assert_eq!(resolve_key(&k, "codex:abc1").unwrap(), k[2]);
        assert!(matches!(
            resolve_key(&k, "claude:abc").unwrap_err(),
            IdResolveError::Ambiguous { .. }
        ));
    }

    #[test]
    fn exact_native_id_across_providers_is_ambiguous() {
        let k = keys();
        assert!(matches!(
            resolve_key(&k, "abc12345-aaaa").unwrap_err(),
            IdResolveError::Ambiguous { .. }
        ));
        assert_eq!(resolve_key(&k, "claude:abc12345-aaaa").unwrap(), k[0]);
    }

    #[test]
    fn qualified_key_round_trips() {
        let k = SessionKey::new(Provider::Codex, "id-1").with_scope("work");
        assert_eq!(SessionKey::parse_qualified(&k.qualified()), Some(k.clone()));
        assert_eq!(
            SessionKey::parse_qualified("codex:id-1"),
            Some(SessionKey::new(Provider::Codex, "id-1"))
        );
        assert_eq!(SessionKey::parse_qualified("nope:id"), None);
        assert_eq!(SessionKey::parse_qualified("codex:"), None);
    }

    #[test]
    fn unknown_id_is_not_found() {
        assert_eq!(
            resolve_key(&keys(), "qqq"),
            Err(IdResolveError::NotFound("qqq".into()))
        );
    }
}
