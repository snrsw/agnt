use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    PullRequest,
    Issue,
    Other,
}

impl LinkKind {
    pub fn label(self) -> &'static str {
        match self {
            LinkKind::PullRequest => "PR",
            LinkKind::Issue => "issue",
            LinkKind::Other => "link",
        }
    }
}

/// Where a link came from. Priority for display: manual > provider > inferred.
/// Inferred links never overwrite manual ones and are shown as guesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkSource {
    Manual,
    Provider,
    Inferred,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub kind: LinkKind,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    pub source: LinkSource,
    /// Why this link was attached (branch name, provider field, ...).
    /// Kept so a later HEAD change in the same cwd does not silently re-link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkParseError {
    #[error("`{0}` is not an http(s) URL")]
    NotAUrl(String),
}

impl Link {
    /// Build a link from a URL. GitHub PR/issue URLs get kind, repo, and
    /// number; any other http(s) URL becomes `Other`.
    pub fn from_url(url: &str, source: LinkSource) -> Result<Link, LinkParseError> {
        let trimmed = url.trim();
        let rest = trimmed
            .strip_prefix("https://")
            .or_else(|| trimmed.strip_prefix("http://"))
            .ok_or_else(|| LinkParseError::NotAUrl(url.to_string()))?;
        if rest.is_empty() {
            return Err(LinkParseError::NotAUrl(url.to_string()));
        }

        let mut link = Link {
            kind: LinkKind::Other,
            url: trimmed.to_string(),
            repo: None,
            number: None,
            source,
            evidence: None,
        };

        let path = rest.trim_end_matches('/');
        let segments: Vec<&str> = path.split('/').collect();
        // github.com / owner / repo / (pull|issues) / number [/ ...]
        if segments.len() >= 5
            && segments[0].eq_ignore_ascii_case("github.com")
            && let Ok(number) = segments[4]
                .split(['#', '?'])
                .next()
                .unwrap_or_default()
                .parse::<u64>()
        {
            let kind = match segments[3] {
                "pull" => Some(LinkKind::PullRequest),
                "issues" => Some(LinkKind::Issue),
                _ => None,
            };
            if let Some(kind) = kind {
                link.kind = kind;
                link.repo = Some(format!("{}/{}", segments[1], segments[2]));
                link.number = Some(number);
            }
        }
        Ok(link)
    }

    /// Short form for the list column, e.g. `PR #123`, `issue #98`, `link`.
    pub fn short_label(&self) -> String {
        match self.number {
            Some(n) => format!("{} #{n}", self.kind.label()),
            None => self.kind.label().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_pull_request() {
        let l = Link::from_url("https://github.com/example/api/pull/123", LinkSource::Manual).unwrap();
        assert_eq!(l.kind, LinkKind::PullRequest);
        assert_eq!(l.repo.as_deref(), Some("example/api"));
        assert_eq!(l.number, Some(123));
        assert_eq!(l.short_label(), "PR #123");
    }

    #[test]
    fn parses_github_issue_with_fragment() {
        let l = Link::from_url(
            "https://github.com/example/api/issues/98#issuecomment-1",
            LinkSource::Provider,
        )
        .unwrap();
        assert_eq!(l.kind, LinkKind::Issue);
        assert_eq!(l.number, Some(98));
        assert_eq!(l.short_label(), "issue #98");
    }

    #[test]
    fn non_github_url_is_other() {
        let l = Link::from_url("https://example.com/ticket/5", LinkSource::Manual).unwrap();
        assert_eq!(l.kind, LinkKind::Other);
        assert_eq!(l.number, None);
    }

    #[test]
    fn rejects_non_url() {
        assert!(Link::from_url("123", LinkSource::Manual).is_err());
        assert!(Link::from_url("https://", LinkSource::Manual).is_err());
    }
}
