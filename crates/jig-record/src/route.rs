//! Path → dialect → upstream routing for the recorder.
//!
//! The recorder reuses the *same* route table as the server (see
//! `jig_server`'s `dialect_for_path` and bootstrap.md "Why this shape"): the
//! integration seam is the path a client is pointed at. Each dialect knows the
//! real upstream host it forwards to, so the recorder can establish the HTTPS
//! leg without the caller having to spell out the destination.

use jig_core::Dialect;

/// The real upstream a dialect's traffic is forwarded to.
///
/// Only the host is dialect-fixed; the path is taken verbatim from the incoming
/// request so the recorder never rewrites what the client asked for. The host
/// for DeepSeek (and any other OpenAI-compatible backend) is configurable at the
/// proxy level — [`Route::with_upstream_host`] overrides the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The dialect this path serves (from the route table, not the body).
    pub dialect: Dialect,
    /// The upstream host to forward to, e.g. `api.openai.com`.
    pub upstream_host: String,
    /// The TLS port on the upstream (always 443 for the real providers).
    pub upstream_port: u16,
}

impl Route {
    /// Resolve a request path to its [`Route`], or `None` for an unknown path
    /// (which the proxy rejects rather than forwarding blindly).
    ///
    /// The default upstream host is the canonical provider for the dialect;
    /// callers targeting an OpenAI-compatible backend (DeepSeek, a gateway, …)
    /// override it with [`Route::with_upstream_host`].
    pub fn resolve(path: &str) -> Option<Route> {
        let dialect = dialect_for_path(path)?;
        Some(Route {
            dialect,
            upstream_host: default_upstream_host(dialect).to_string(),
            upstream_port: 443,
        })
    }

    /// Return a copy of this route forwarding to `host` instead of the dialect
    /// default. Used to point OpenAI-dialect traffic at DeepSeek or any other
    /// OpenAI-compatible backend without changing the path the client uses.
    pub fn with_upstream_host(mut self, host: impl Into<String>) -> Route {
        self.upstream_host = host.into();
        self
    }
}

/// Map a request path to the wire dialect it serves, or `None` for unknown
/// paths. This mirrors `jig_server`'s route table exactly — the route table is
/// the single source of dialect truth.
pub fn dialect_for_path(path: &str) -> Option<Dialect> {
    match path {
        "/chat/completions" => Some(Dialect::OpenAi),
        "/v1/messages" => Some(Dialect::Anthropic),
        "/backend-api/codex/responses" => Some(Dialect::Codex),
        _ => None,
    }
}

/// The canonical upstream host for a dialect.
fn default_upstream_host(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::OpenAi => "api.openai.com",
        Dialect::Anthropic => "api.anthropic.com",
        Dialect::Codex => "chatgpt.com",
    }
}

/// A short, lowercase slug for a dialect, used as the top-level fixture
/// directory (`fixtures/<dialect>/…`). Stable across runs.
pub fn dialect_slug(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::OpenAi => "openai",
        Dialect::Anthropic => "anthropic",
        Dialect::Codex => "codex",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_paths_resolve_to_their_dialect_and_default_host() {
        let route = Route::resolve("/chat/completions").unwrap();
        assert_eq!(route.dialect, Dialect::OpenAi);
        assert_eq!(route.upstream_host, "api.openai.com");
        assert_eq!(route.upstream_port, 443);

        assert_eq!(
            Route::resolve("/v1/messages").unwrap().dialect,
            Dialect::Anthropic
        );
        assert_eq!(
            Route::resolve("/backend-api/codex/responses")
                .unwrap()
                .dialect,
            Dialect::Codex
        );
    }

    #[test]
    fn unknown_path_does_not_resolve() {
        assert!(Route::resolve("/nope").is_none());
        assert!(dialect_for_path("/").is_none());
    }

    #[test]
    fn upstream_host_is_overridable_for_compatible_backends() {
        let route = Route::resolve("/chat/completions")
            .unwrap()
            .with_upstream_host("api.deepseek.com");
        assert_eq!(route.dialect, Dialect::OpenAi);
        assert_eq!(route.upstream_host, "api.deepseek.com");
    }

    #[test]
    fn dialect_slugs_are_stable() {
        assert_eq!(dialect_slug(Dialect::OpenAi), "openai");
        assert_eq!(dialect_slug(Dialect::Anthropic), "anthropic");
        assert_eq!(dialect_slug(Dialect::Codex), "codex");
    }
}
