use jig_core::{PhaseSpec, ReplySpec, ScriptFile, StopSpec, TurnSpec, fixtures_root};
use serde_json::Value;

fn text_reply(reply: &ReplySpec) -> &str {
    match reply {
        ReplySpec::Text { text } => text.as_str(),
        ReplySpec::Full { turns, stop, .. } => {
            assert_eq!(*stop, StopSpec::Stop, "text replies should stop normally");
            match turns.as_slice() {
                [TurnSpec::Text(text)] => text.as_str(),
                _ => panic!("expected a single text turn"),
            }
        }
    }
}

fn workspace_result(text: &str) -> Value {
    let trimmed = text.trim();
    assert_eq!(
        trimmed, text,
        "workspace result must not include prose padding"
    );
    assert!(
        trimmed.starts_with('{') && trimmed.ends_with('}'),
        "workspace result must be a single JSON object: {text}"
    );

    let value: Value = serde_json::from_str(trimmed).expect("workspace result parses as JSON");
    assert!(value.is_object(), "workspace result is an object");
    value
}

fn phase_by_name<'a>(phases: &'a [PhaseSpec], name: &str) -> &'a PhaseSpec {
    phases
        .iter()
        .find(|phase| phase.name == name)
        .unwrap_or_else(|| panic!("missing phase {name}"))
}

#[test]
fn basic_delivery_fixture_matches_temper_phase_contract() {
    let file = ScriptFile::load(fixtures_root().join("basic-delivery.json"))
        .expect("basic-delivery fixture loads");
    let phases = match file {
        ScriptFile::Phases(phases) => phases,
        other => panic!("expected a phase script, got {other:?}"),
    };
    assert_eq!(phases.len(), 2, "fixture has architect and engineer phases");

    let architect_phase = phase_by_name(&phases, "architect-triage");
    assert_eq!(
        architect_phase.when.messages_contain,
        vec!["ROLE: architect".to_string()],
        "architect phase is selected from the Temper role prompt"
    );
    let replies = &architect_phase.sequence;
    assert_eq!(replies.len(), 2, "architect phase serves tool then result");

    let ReplySpec::Full {
        turns,
        stop,
        usage: _,
    } = &replies[0]
    else {
        panic!("architect turn must use full form for a tool-call stop");
    };
    assert_eq!(*stop, StopSpec::ToolCalls);
    let [TurnSpec::ToolCall(tool)] = turns.as_slice() else {
        panic!("architect turn must contain one tool call");
    };
    assert_eq!(tool.name, "bash");
    let args = tool.args.as_object().expect("bash args are an object");
    assert_eq!(args.len(), 1, "bash args contain only the command");
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .expect("bash command is a string");
    assert!(command.contains("jig-basic-delivery-inspection"));
    assert!(command.contains("[ -d service ]"));
    assert!(command.contains("find service"));

    let architect = workspace_result(text_reply(&replies[1]));
    assert_eq!(
        architect.get("verdict").and_then(Value::as_str),
        Some("ready_code")
    );
    assert!(
        architect
            .get("body")
            .and_then(Value::as_str)
            .is_some_and(|body| body.starts_with(
                "## Code spec\n\nImplement: Service banner should identify the environment"
            )),
        "architect body must begin with the code spec"
    );

    let engineer_phase = phase_by_name(&phases, "engineer-implementation");
    assert_eq!(
        engineer_phase.when.messages_contain,
        vec!["ROLE: engineer".to_string()],
        "engineer phase is selected from the Temper role prompt"
    );
    let replies = &engineer_phase.sequence;
    assert_eq!(replies.len(), 2, "engineer phase serves tool then result");

    let ReplySpec::Full {
        turns,
        stop,
        usage: _,
    } = &replies[0]
    else {
        panic!("engineer turn must use full form for a tool-call stop");
    };
    assert_eq!(*stop, StopSpec::ToolCalls);
    let [TurnSpec::ToolCall(tool)] = turns.as_slice() else {
        panic!("engineer turn must contain one tool call");
    };
    assert_eq!(tool.name, "write");
    let args = tool.args.as_object().expect("write args are an object");
    assert_eq!(
        args.get("path").and_then(Value::as_str),
        Some("service/src/banner.py")
    );
    assert!(args.contains_key("content"), "write args use content");
    assert!(
        !args.contains_key("contents"),
        "write args must not use contents"
    );
    assert_eq!(args.len(), 2, "write args contain only path and content");
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .expect("write content is a string");
    assert!(content.contains("def service_banner(environment=None, greeting=None):"));
    assert!(content.contains("SERVICE_ENVIRONMENT"));
    assert!(content.contains("SERVICE_BANNER_GREETING"));

    let engineer = workspace_result(text_reply(&replies[1]));
    assert!(
        engineer.get("verdict").is_none(),
        "engineer workspace result must not declare a verdict"
    );
    assert!(
        engineer
            .get("summary")
            .and_then(Value::as_str)
            .is_some_and(|summary| summary.contains("service/src/banner.py")),
        "engineer summary must list service/src/banner.py"
    );
}
