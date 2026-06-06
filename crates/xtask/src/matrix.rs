//! The recording scenario matrix and the planner that expands it.
//!
//! P5 (#19) makes the record→fixtures loop **repeatable with one command**. The
//! matrix here is the declarative source of truth for *what* gets recorded: for
//! each wire dialect, the scenarios from the issue #13 matrix, and the clients
//! that drive them. `xtask record` expands the matrix into a list of concrete
//! [`RecordInvocation`]s — one `jig record` call each — after applying the
//! caller's `--dialect` / `--scenario` / `--client` selection.
//!
//! This module is **pure**: it neither spawns a process nor touches the network.
//! Expanding and filtering the matrix and rendering the argv for each invocation
//! are all data transforms, so the whole planner is unit-tested offline and runs
//! under `cargo test` in CI — only the actual spawning in `main` is manual and
//! online (it needs a live API key, exactly like `jig record` itself).

use crate::Provenance;

/// A client that drives the recorder for a scenario, with its fixed [`Role`].
///
/// The `client` label and `role` are stamped verbatim into `meta.json` by
/// `jig record` (see `jig_record::Provenance`); the matrix only decides which
/// combinations exist. Official clients are `authoritative` (the spec); the
/// pi-SDK driver is `subject` (measured against the spec — P6, #17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Client {
    /// Free-form client label, e.g. `openai-sdk`, `claude-code`, `codex`.
    pub label: &'static str,
    /// `authoritative` for official clients, `subject` for the SDK under test.
    pub role: Role,
}

/// The role a recording plays, mirroring `jig_record::Role` without taking a
/// dependency on the recorder crate (the matrix is wire-format-agnostic and only
/// passes the slug through to the `jig record` CLI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Produced by an official client — the reference shape.
    Authoritative,
    /// Produced by the SDK under test — compared against `authoritative`.
    Subject,
}

impl Role {
    /// The lowercase slug `jig record --role` accepts and `meta.json` stores.
    pub fn slug(self) -> &'static str {
        match self {
            Role::Authoritative => "authoritative",
            Role::Subject => "subject",
        }
    }
}

/// One dialect's slice of the matrix: its slug, the scenarios it covers, and the
/// clients that can drive it.
#[derive(Debug, Clone, Copy)]
pub struct DialectMatrix {
    /// The dialect slug — the top-level `fixtures/<dialect>/` directory.
    pub dialect: &'static str,
    /// The scenarios captured for this dialect (issue #13 matrix).
    pub scenarios: &'static [&'static str],
    /// The clients that drive this dialect, in priority order.
    pub clients: &'static [Client],
}

/// The OpenAI/DeepSeek SDK driving the chat-completions dialect.
const OPENAI_SDK: Client = Client {
    label: "openai-sdk",
    role: Role::Authoritative,
};
/// Claude Code driving the Anthropic messages dialect.
const CLAUDE_CODE: Client = Client {
    label: "claude-code",
    role: Role::Authoritative,
};
/// The Codex CLI driving the Codex responses dialect.
const CODEX_CLI: Client = Client {
    label: "codex",
    role: Role::Authoritative,
};
/// The pi-SDK driver, used directly as a library. Drives every dialect as the
/// `subject` under test; wired up in P6 (#17) but listed here so the matrix is
/// the single source of truth and `--client pi-sdk` already resolves.
const PI_SDK: Client = Client {
    label: "pi-sdk",
    role: Role::Subject,
};

/// Scenarios that exist for the OpenAI/DeepSeek chat-completions dialect.
const OPENAI_SCENARIOS: &[&str] = &["single-text", "tool-call", "tool-result-final"];
/// Anthropic adds a thinking+text scenario on top of the shared three.
const ANTHROPIC_SCENARIOS: &[&str] = &[
    "single-text",
    "tool-call",
    "tool-result-final",
    "thinking-text",
];
/// Codex mirrors Anthropic's thinking-capable set.
const CODEX_SCENARIOS: &[&str] = &[
    "single-text",
    "tool-call",
    "tool-result-final",
    "thinking-text",
];

/// The full recording matrix: every dialect, its scenarios, and its drivers.
///
/// This is the authoritative list `xtask record --all` walks. Adding a dialect,
/// scenario, or client is a one-line edit here and needs no orchestrator change.
pub const MATRIX: &[DialectMatrix] = &[
    DialectMatrix {
        dialect: "openai",
        scenarios: OPENAI_SCENARIOS,
        clients: &[OPENAI_SDK, PI_SDK],
    },
    DialectMatrix {
        dialect: "anthropic",
        scenarios: ANTHROPIC_SCENARIOS,
        clients: &[CLAUDE_CODE, PI_SDK],
    },
    DialectMatrix {
        dialect: "codex",
        scenarios: CODEX_SCENARIOS,
        clients: &[CODEX_CLI, PI_SDK],
    },
];

/// The known dialect slugs, derived from [`MATRIX`] so the two never drift.
pub fn known_dialects() -> Vec<&'static str> {
    MATRIX.iter().map(|d| d.dialect).collect()
}

/// A caller's selection of which slice of the matrix to record.
///
/// Each field, when `Some`, restricts to that exact value; `None` means "all".
/// `--all` is simply every field left `None`. An empty selection (the default
/// with no flags) records nothing and is reported as such, so a bare
/// `xtask record` never silently spawns the whole online matrix.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub dialect: Option<String>,
    pub scenario: Option<String>,
    pub client: Option<String>,
}

impl Selection {
    /// Whether this selection picks the entire matrix (no field constrained).
    pub fn is_all(&self) -> bool {
        self.dialect.is_none() && self.scenario.is_none() && self.client.is_none()
    }
}

/// One concrete `jig record` invocation produced by expanding the matrix.
///
/// Holds everything needed to build the argv; [`argv`](RecordInvocation::argv)
/// renders the flags `src/main.rs`'s `RecordOpts::parse` accepts. The
/// `upstream_host` is carried for DeepSeek-style overrides on the openai dialect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordInvocation {
    pub dialect: String,
    pub scenario: String,
    pub client: String,
    pub role: Role,
}

impl RecordInvocation {
    /// The `jig record` argv (after the `record` subcommand word) for this
    /// invocation, stamping provenance so re-runs are reproducible.
    ///
    /// `fixtures_root` is where the four-file recording is written; the layout
    /// underneath (`<dialect>/<scenario>/recordings/<client>/`) is decided by the
    /// recorder from the request path (the dialect) and the `meta`, not spelled
    /// out here — the orchestrator's `dialect`/`scenario` only pick *which* client
    /// to drive at *which* base URL, exactly as the how-to documents.
    pub fn argv(&self, fixtures_root: &str, provenance: &Provenance) -> Vec<String> {
        let mut argv = vec![
            "record".to_string(),
            "--client".to_string(),
            self.client.clone(),
            "--role".to_string(),
            self.role.slug().to_string(),
            "--scenario".to_string(),
            self.scenario.clone(),
            "--fixtures-root".to_string(),
            fixtures_root.to_string(),
            "--captured".to_string(),
            provenance.captured.clone(),
            "--recorder-sha".to_string(),
            provenance.recorder_sha.clone(),
        ];
        if let Some(host) = &provenance.upstream_host {
            argv.push("--upstream-host".to_string());
            argv.push(host.clone());
        }
        argv
    }
}

/// Expand [`MATRIX`] into the invocations a [`Selection`] picks, in matrix order.
///
/// Pure: the result is a deterministic function of the matrix and the selection,
/// so `xtask record` prints exactly what it will run before running anything, and
/// the expansion is unit-tested without spawning a single process.
pub fn plan(selection: &Selection) -> Vec<RecordInvocation> {
    let want = |sel: &Option<String>, value: &str| sel.as_deref().is_none_or(|s| s == value);

    let mut out = Vec::new();
    for d in MATRIX {
        if !want(&selection.dialect, d.dialect) {
            continue;
        }
        for &scenario in d.scenarios {
            if !want(&selection.scenario, scenario) {
                continue;
            }
            for client in d.clients {
                if !want(&selection.client, client.label) {
                    continue;
                }
                out.push(RecordInvocation {
                    dialect: d.dialect.to_string(),
                    scenario: scenario.to_string(),
                    client: client.label.to_string(),
                    role: client.role,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance() -> Provenance {
        Provenance {
            captured: "2026-06-06".to_string(),
            recorder_sha: "deadbee".to_string(),
            upstream_host: None,
        }
    }

    #[test]
    fn all_selection_covers_the_whole_matrix() {
        let expected: usize = MATRIX
            .iter()
            .map(|d| d.scenarios.len() * d.clients.len())
            .sum();
        let plan = plan(&Selection::default());
        assert_eq!(plan.len(), expected);
        assert!(Selection::default().is_all());
    }

    #[test]
    fn dialect_filter_restricts_to_one_dialect() {
        let sel = Selection {
            dialect: Some("openai".to_string()),
            ..Default::default()
        };
        let plan = plan(&sel);
        assert!(!plan.is_empty());
        assert!(plan.iter().all(|i| i.dialect == "openai"));
        // openai has 3 scenarios × 2 clients.
        assert_eq!(plan.len(), 6);
    }

    #[test]
    fn scenario_and_client_filters_compose() {
        let sel = Selection {
            dialect: Some("anthropic".to_string()),
            scenario: Some("thinking-text".to_string()),
            client: Some("claude-code".to_string()),
        };
        let plan = plan(&sel);
        assert_eq!(plan.len(), 1);
        let only = &plan[0];
        assert_eq!(only.dialect, "anthropic");
        assert_eq!(only.scenario, "thinking-text");
        assert_eq!(only.client, "claude-code");
        assert_eq!(only.role, Role::Authoritative);
    }

    #[test]
    fn unknown_selection_yields_an_empty_plan() {
        let sel = Selection {
            dialect: Some("gemini".to_string()),
            ..Default::default()
        };
        assert!(plan(&sel).is_empty());
    }

    #[test]
    fn pi_sdk_is_subject_on_every_dialect() {
        let sel = Selection {
            client: Some("pi-sdk".to_string()),
            ..Default::default()
        };
        let plan = plan(&sel);
        assert!(!plan.is_empty());
        assert!(plan.iter().all(|i| i.role == Role::Subject));
        // One pi-sdk subject recording per (dialect, scenario).
        let scenario_total: usize = MATRIX.iter().map(|d| d.scenarios.len()).sum();
        assert_eq!(plan.len(), scenario_total);
    }

    #[test]
    fn argv_round_trips_through_record_flags() {
        let inv = RecordInvocation {
            dialect: "openai".to_string(),
            scenario: "single-text".to_string(),
            client: "openai-sdk".to_string(),
            role: Role::Authoritative,
        };
        let argv = inv.argv("fixtures", &provenance());

        assert_eq!(argv[0], "record");
        // Spot-check the flag/value pairing the binary's parser expects.
        let pos = |flag: &str| {
            argv.iter()
                .position(|a| a == flag)
                .map(|i| argv[i + 1].clone())
        };
        assert_eq!(pos("--client").as_deref(), Some("openai-sdk"));
        assert_eq!(pos("--role").as_deref(), Some("authoritative"));
        assert_eq!(pos("--scenario").as_deref(), Some("single-text"));
        // `--dialect` is orchestrator-only metadata (the recorder derives the
        // dialect from the request path), so it is not in the `jig record` argv.
        assert!(!argv.iter().any(|a| a == "--dialect"));
        assert_eq!(pos("--fixtures-root").as_deref(), Some("fixtures"));
        assert_eq!(pos("--captured").as_deref(), Some("2026-06-06"));
        assert_eq!(pos("--recorder-sha").as_deref(), Some("deadbee"));
        // No upstream override unless one is supplied.
        assert!(!argv.iter().any(|a| a == "--upstream-host"));
    }

    #[test]
    fn argv_includes_upstream_host_override_when_set() {
        let inv = RecordInvocation {
            dialect: "openai".to_string(),
            scenario: "single-text".to_string(),
            client: "openai-sdk".to_string(),
            role: Role::Authoritative,
        };
        let provenance = Provenance {
            upstream_host: Some("api.deepseek.com".to_string()),
            ..provenance()
        };
        let argv = inv.argv("fixtures", &provenance);
        let pos = argv.iter().position(|a| a == "--upstream-host").unwrap();
        assert_eq!(argv[pos + 1], "api.deepseek.com");
    }

    #[test]
    fn known_dialects_match_the_matrix() {
        assert_eq!(known_dialects(), vec!["openai", "anthropic", "codex"]);
    }
}
