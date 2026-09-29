//! Built-in Temper reference-delivery fixture behavior.
//!
//! The public script-file format covers fixed, sequence, and phase fixtures. The
//! reference-delivery demo also needs a small amount of request-derived data: the
//! configured target repositories, checkout directory, and role. Keeping that
//! logic here lets the demo use the vanilla `jig` binary without carrying a
//! Temper-side helper process. It is a pure function of the request and the
//! options, so the script carries it as data ([`crate::Script::ReferenceDelivery`])
//! rather than as a closure.

use std::collections::BTreeSet;

use serde_json::json;

use crate::{ReferenceDeliverySpec, Reply, RequestView, StopReason, Turn, Usage};

impl ReferenceDeliverySpec {
    /// The reply the reference-delivery built-in serves for `view`.
    pub fn reply(&self, view: &RequestView) -> Reply {
        reference_delivery_reply(view, &self.greeting_file)
    }
}

fn reference_delivery_reply(view: &RequestView, greeting_file: &str) -> Reply {
    let text = conversation_text(view);
    if has_role(&text, "architect") {
        architect_reply(&text, greeting_file)
    } else if has_role(&text, "engineer") {
        engineer_reply(view, &text, greeting_file)
    } else if has_role(&text, "reviewer") {
        reviewer_reply()
    } else {
        Reply::text(json!({ "summary": "No reference-delivery role was detected." }).to_string())
    }
}

fn conversation_text(view: &RequestView) -> String {
    view.messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_role(text: &str, role: &str) -> bool {
    text.contains(&format!("Role: {role}"))
        || text.contains(&format!("ROLE: {role}"))
        || text.contains(&format!("\"role\":\"{role}\""))
        || text.contains(&format!("\"role\": \"{role}\""))
}

fn architect_reply(text: &str, greeting_file: &str) -> Reply {
    let target_repos = target_repos_from_intake(text);
    if target_repos.len() > 1 {
        let children = target_repos
            .iter()
            .map(|repo| {
                json!({
                    "slug": slug_for_repo(repo),
                    "title": format!("Add reference-delivery greeting to {repo}"),
                    "body": child_spec(repo, greeting_file),
                    "labels": ["code", "ready"],
                    "depends_on": [],
                    "target_repo": repo,
                })
            })
            .collect::<Vec<_>>();
        Reply::text(
            json!({
                "verdict": "needs_breakdown",
                "summary": "Split the coordinated intake into one ready code issue per target repository.",
                "children": children,
            })
            .to_string(),
        )
    } else {
        let repo = target_repos
            .first()
            .cloned()
            .or_else(|| first_repo_path(text))
            .unwrap_or_else(|| "acme/service".to_string());
        Reply::text(
            json!({
                "verdict": "ready_code",
                "summary": "Rewrote the intake as a deterministic code specification.",
                "body": child_spec(&repo, greeting_file),
            })
            .to_string(),
        )
    }
}

fn engineer_reply(view: &RequestView, text: &str, greeting_file: &str) -> Reply {
    let repo = first_repo_path(text).unwrap_or_else(|| "acme/service".to_string());
    let dir = first_repo_dir(text).unwrap_or_else(|| repo_name(&repo).to_string());
    if view.prior_tool_results == 0 {
        let path = format!("{dir}/{greeting_file}");
        let content = format!(
            "# Reference delivery greeting\n\nHello from {repo} via Temper reference delivery.\n"
        );
        Reply {
            turns: vec![Turn::ToolCall {
                id: format!(
                    "call_write_reference_delivery_greeting_{}",
                    slug_for_repo(&repo)
                ),
                name: "write".to_string(),
                args: json!({ "path": path, "content": content }),
            }],
            usage: Usage {
                prompt_tokens: 16,
                completion_tokens: 8,
            },
            stop: StopReason::ToolCalls,
        }
    } else {
        Reply::text(
            json!({
                "summary": format!("Created {greeting_file} with the deterministic reference-delivery greeting for {repo}."),
            })
            .to_string(),
        )
    }
}

fn reviewer_reply() -> Reply {
    Reply::text(
        json!({
            "verdict": "approve",
            "summary": "Approved the deterministic reference-delivery product diff.",
            "review_body": "Approved: the PR contains the expected reference-delivery greeting file and CI is green.",
        })
        .to_string(),
    )
}

fn target_repos_from_intake(text: &str) -> Vec<String> {
    let mut repos = BTreeSet::new();
    let mut rest = text;
    while let Some(index) = rest.find("`target_repo`: `") {
        rest = &rest[index + "`target_repo`: `".len()..];
        if let Some(end) = rest.find('`') {
            let repo = &rest[..end];
            if is_repo_path(repo) {
                repos.insert(repo.to_string());
            }
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    repos.into_iter().collect()
}

fn first_repo_path(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("- ") || !trimmed.contains("(dir:") {
            continue;
        }
        let repo = trimmed[2..].split_whitespace().next().unwrap_or_default();
        if is_repo_path(repo) {
            return Some(repo.to_string());
        }
    }
    None
}

fn first_repo_dir(text: &str) -> Option<String> {
    for line in text.lines() {
        let Some(start) = line.find("(dir: ") else {
            continue;
        };
        let rest = &line[start + "(dir: ".len()..];
        let dir = rest.split('/').next().unwrap_or_default().trim();
        if !dir.is_empty() && is_safe_relative_component(dir) {
            return Some(dir.to_string());
        }
    }
    None
}

fn child_spec(repo: &str, greeting_file: &str) -> String {
    format!(
        "Create `{greeting_file}` in `{repo}` containing the exact line \
         `Hello from {repo} via Temper reference delivery.`. This is the \
         deterministic product diff used by the reference-delivery demo."
    )
}

fn slug_for_repo(repo: &str) -> String {
    repo_name(repo)
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn repo_name(repo: &str) -> &str {
    repo.rsplit_once('/').map(|(_, name)| name).unwrap_or(repo)
}

fn is_repo_path(value: &str) -> bool {
    let Some((owner, name)) = value.split_once('/') else {
        return false;
    };
    !owner.is_empty() && !name.is_empty() && !name.contains('/')
}

fn is_safe_relative_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use crate::{Dialect, StopReason, ViewMessage};

    use super::*;

    fn view(content: &str, prior_tool_results: usize) -> RequestView {
        RequestView::new(
            Dialect::OpenAi,
            Some("deepseek-chat".to_string()),
            vec![ViewMessage {
                role: "user".to_string(),
                content: content.to_string(),
            }],
            prior_tool_results,
        )
    }

    fn text_reply_value(reply: Reply) -> Value {
        let text = match reply.turns.as_slice() {
            [Turn::Text(text)] => text,
            other => panic!("expected one text turn, got {other:?}"),
        };
        serde_json::from_str(text).expect("reply text is JSON")
    }

    #[test]
    fn architect_breaks_down_each_target_repo_from_intake() {
        let reply = text_reply_value(reference_delivery_reply(
            &view(
                "ROLE: architect\n- `one/service` (`target_repo`: `one/service`, child `slug`: `service`)\n- `two/api` (`target_repo`: `two/api`, child `slug`: `api`)",
                0,
            ),
            "GREETING.md",
        ));

        assert_eq!(reply["verdict"], "needs_breakdown");
        assert_eq!(reply["children"].as_array().unwrap().len(), 2);
        assert_eq!(reply["children"][0]["target_repo"], "one/service");
        assert_eq!(reply["children"][1]["target_repo"], "two/api");
        assert!(
            reply["children"][1]["body"]
                .as_str()
                .unwrap()
                .contains("GREETING.md")
        );
    }

    #[test]
    fn engineer_writes_to_the_checked_out_repo_dir() {
        let reply = reference_delivery_reply(
            &view(
                "ROLE: engineer\nRepositories:\n- org/widget (dir: widget-dir/, access: writable, default branch: main, base branch: main, work branch: agent/widget)",
                0,
            ),
            "GREETING.md",
        );

        assert_eq!(reply.stop, StopReason::ToolCalls);
        let [Turn::ToolCall { name, args, .. }] = reply.turns.as_slice() else {
            panic!("expected one tool call");
        };
        assert_eq!(name, "write");
        assert_eq!(args["path"], "widget-dir/GREETING.md");
        assert!(args["content"].as_str().unwrap().contains("org/widget"));
    }
}
