//! Scripts: what jig serves for each request.
//!
//! A [`Script`] is what callers build. Everything in it is plain data except a
//! rule closure, so [`Script::split`] separates the two: a [`Plan`] that the
//! pure provider core owns and advances with `&mut self`, and an optional
//! [`Rule`] that stays with the embedder. The provider never calls a closure or
//! takes a lock; a [`Plan::External`] tells it to ask the embedder instead (see
//! `docs/explanation/sans-io-shell.md` §4.10).

use std::fmt;

use crate::{PhaseMatcher, ReferenceDeliverySpec, Reply, RequestView, ScriptAction};

/// Decides which [`ScriptAction`] to serve for each request.
///
/// Direct users call [`Script::next_action`]. A server splits the script with
/// [`Script::split`] so that its pure core owns only data.
#[derive(Debug)]
pub enum Script {
    /// Serve the same reply for every request.
    Fixed(Reply),
    /// Serve the same action for every request.
    FixedAction(ScriptAction),
    /// Serve actions in order, repeating the last once exhausted.
    Sequence(Sequence),
    /// Pick the first phase matching the request and advance that phase's own
    /// sequence (the script-file `phases` form).
    Phases(Phases),
    /// Temper's reference-delivery built-in, configured by its options.
    ReferenceDelivery(ReferenceDeliverySpec),
    /// Decide the action from the parsed request with a closure.
    Rule(Rule),
}

impl Script {
    /// Build a sequence of replies. An empty sequence serves an empty text
    /// reply, so a misconfigured script never panics the server.
    pub fn sequence(replies: Vec<Reply>) -> Self {
        Script::Sequence(Sequence::new(
            replies.into_iter().map(ScriptAction::Reply).collect(),
        ))
    }

    /// Build a rule script from a closure deciding the reply.
    pub fn rule(mut f: impl FnMut(&RequestView) -> Reply + Send + 'static) -> Self {
        Script::Rule(Rule::new(move |view| ScriptAction::Reply(f(view))))
    }

    /// Build a fixed script action.
    pub fn fixed_action(action: impl Into<ScriptAction>) -> Self {
        Script::FixedAction(action.into())
    }

    /// Build a sequence of actions, repeating the last once exhausted.
    pub fn action_sequence(actions: Vec<ScriptAction>) -> Self {
        Script::Sequence(Sequence::new(actions))
    }

    /// Build a rule script from a closure deciding the action.
    pub fn action_rule(f: impl FnMut(&RequestView) -> ScriptAction + Send + 'static) -> Self {
        Script::Rule(Rule::new(f))
    }

    /// Produce the action for the next request, calling the rule closure
    /// itself. A convenience for direct users; servers use [`Script::split`].
    pub fn next_action(&mut self, view: &RequestView) -> ScriptAction {
        match self {
            Script::Fixed(reply) => ScriptAction::Reply(reply.clone()),
            Script::FixedAction(action) => action.clone(),
            Script::Sequence(sequence) => sequence.next_action(),
            Script::Phases(phases) => phases.next_action(view),
            Script::ReferenceDelivery(spec) => ScriptAction::Reply(spec.reply(view)),
            Script::Rule(rule) => rule.decide(view),
        }
    }

    /// Separate the data from the closure: the [`Plan`] goes to the pure core,
    /// the [`Rule`] (present only for a rule script) to whoever answers the
    /// core's decision requests.
    pub fn split(self) -> (Plan, Option<Rule>) {
        match self {
            Script::Fixed(reply) => (Plan::Fixed(ScriptAction::Reply(reply)), None),
            Script::FixedAction(action) => (Plan::Fixed(action), None),
            Script::Sequence(sequence) => (Plan::Sequence(sequence), None),
            Script::Phases(phases) => (Plan::Phases(phases), None),
            Script::ReferenceDelivery(spec) => (Plan::ReferenceDelivery(spec), None),
            Script::Rule(rule) => (Plan::External, Some(rule)),
        }
    }
}

/// The data part of a [`Script`]: everything the pure provider core needs to
/// choose an action, with no closures and no locks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// The same action for every request.
    Fixed(ScriptAction),
    /// Actions in order, the last repeating.
    Sequence(Sequence),
    /// Per-phase sequences selected by the request.
    Phases(Phases),
    /// Temper's reference-delivery built-in.
    ReferenceDelivery(ReferenceDeliverySpec),
    /// The decision belongs to a [`Rule`] held outside the core.
    External,
}

/// What a [`Plan`] says to do with a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Serve this action.
    Action(ScriptAction),
    /// Ask the embedder, which holds the [`Rule`].
    External,
}

impl Plan {
    /// Choose the action for the next request, advancing any cursor.
    pub fn next(&mut self, view: &RequestView) -> Next {
        match self {
            Plan::Fixed(action) => Next::Action(action.clone()),
            Plan::Sequence(sequence) => Next::Action(sequence.next_action()),
            Plan::Phases(phases) => Next::Action(phases.next_action(view)),
            Plan::ReferenceDelivery(spec) => Next::Action(ScriptAction::Reply(spec.reply(view))),
            Plan::External => Next::External,
        }
    }
}

/// Actions served in order; once exhausted, the last repeats for every
/// further request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sequence {
    actions: Vec<ScriptAction>,
    cursor: usize,
}

impl Sequence {
    /// A sequence starting at its first action.
    pub fn new(actions: Vec<ScriptAction>) -> Self {
        Sequence { actions, cursor: 0 }
    }

    /// The action at the cursor, then advance, clamping at the last action. An
    /// empty sequence yields an empty text reply rather than panicking.
    pub fn next_action(&mut self) -> ScriptAction {
        let Some(chosen) = self.actions.get(self.cursor) else {
            return empty_reply();
        };
        let chosen = chosen.clone();
        if self.cursor + 1 < self.actions.len() {
            self.cursor += 1;
        }
        chosen
    }
}

/// One named phase of a [`Phases`] script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phase {
    /// For humans and diagnostics; selection is entirely by `when`.
    pub name: String,
    pub when: PhaseMatcher,
    pub sequence: Sequence,
}

/// Phases checked in order: the first whose matcher accepts the request serves
/// from its own sequence. Separate cursors are the point: an extra tool turn in
/// one phase must not consume another phase's replies.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Phases(pub Vec<Phase>);

impl Phases {
    /// The next action of the first matching phase, or an empty text reply
    /// when no phase matches.
    pub fn next_action(&mut self, view: &RequestView) -> ScriptAction {
        match self.0.iter_mut().find(|phase| phase.when.matches(view)) {
            Some(phase) => phase.sequence.next_action(),
            None => empty_reply(),
        }
    }
}

/// A rule closure: decides the action from the request.
///
/// It is `FnMut + Send` so state can live in the closure and the closure can
/// move to the loop thread. Pure code never holds one: [`Script::split`] hands
/// it to the embedder, which answers the core's decision requests with it.
pub struct Rule(Box<dyn FnMut(&RequestView) -> ScriptAction + Send>);

impl Rule {
    /// Wrap a decision closure.
    pub fn new(f: impl FnMut(&RequestView) -> ScriptAction + Send + 'static) -> Self {
        Rule(Box::new(f))
    }

    /// Decide the action for `view`.
    pub fn decide(&mut self, view: &RequestView) -> ScriptAction {
        (self.0)(view)
    }
}

impl fmt::Debug for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Rule(..)")
    }
}

fn empty_reply() -> ScriptAction {
    ScriptAction::Reply(Reply::text(""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dialect, HttpError, StopReason, Turn, Usage, ViewMessage};

    /// A minimal OpenAI view with the given prior-tool-result count, for
    /// exercising scripts without standing up a server.
    fn view_with_turns(prior_tool_results: usize) -> RequestView {
        RequestView::new(
            Dialect::OpenAi,
            Some("fake".to_string()),
            vec![],
            prior_tool_results,
        )
    }

    fn view_with_message(content: &str) -> RequestView {
        RequestView::new(
            Dialect::OpenAi,
            None,
            vec![ViewMessage {
                role: "user".to_string(),
                content: content.to_string(),
            }],
            0,
        )
    }

    fn text(content: &str) -> ScriptAction {
        ScriptAction::Reply(Reply::text(content))
    }

    fn phase(name: &str, needle: &str, replies: &[&str]) -> Phase {
        Phase {
            name: name.to_string(),
            when: PhaseMatcher {
                messages_contain: vec![needle.to_string()],
                ..PhaseMatcher::default()
            },
            sequence: Sequence::new(replies.iter().map(|r| text(r)).collect()),
        }
    }

    /// Drive a split plan the way the provider core does, answering nothing:
    /// every call must be decided by the plan itself.
    fn plan_action(plan: &mut Plan, view: &RequestView) -> ScriptAction {
        match plan.next(view) {
            Next::Action(action) => action,
            Next::External => panic!("expected the plan to decide"),
        }
    }

    #[test]
    fn fixed_script_repeats_the_same_reply() {
        let mut script = Script::Fixed(Reply::text("same"));
        let view = view_with_turns(0);
        assert_eq!(script.next_action(&view), script.next_action(&view));
        assert_eq!(script.next_action(&view), text("same"));
    }

    #[test]
    fn sequence_serves_in_order_then_repeats_the_last() {
        let mut script = Script::sequence(vec![
            Reply::text("first"),
            Reply::text("second"),
            Reply::text("third"),
        ]);
        let view = view_with_turns(0);
        assert_eq!(script.next_action(&view), text("first"));
        assert_eq!(script.next_action(&view), text("second"));
        assert_eq!(script.next_action(&view), text("third"));
        // Exhausted: the last reply repeats from here on.
        assert_eq!(script.next_action(&view), text("third"));
        assert_eq!(script.next_action(&view), text("third"));
    }

    #[test]
    fn empty_sequence_yields_an_empty_text_reply() {
        let mut script = Script::sequence(vec![]);
        let view = view_with_turns(0);
        assert_eq!(script.next_action(&view), text(""));
        let mut script = Script::action_sequence(vec![]);
        assert_eq!(script.next_action(&view), text(""));
    }

    #[test]
    fn action_sequence_serves_errors_and_replies_in_order() {
        let mut script = Script::action_sequence(vec![
            ScriptAction::HttpError(HttpError::provider(500, "server_error", "try again")),
            ScriptAction::Reply(Reply::text("success")),
        ]);
        let view = view_with_turns(0);

        match script.next_action(&view) {
            ScriptAction::HttpError(error) => {
                assert_eq!(error.status, 500);
                assert_eq!(
                    error.render_body(Dialect::OpenAi).body,
                    r#"{"error":{"code":"server_error","message":"try again"}}"#
                );
            }
            other => panic!("expected HTTP error action, got {other:?}"),
        }
        assert_eq!(script.next_action(&view), text("success"));
        assert_eq!(script.next_action(&view), text("success"));
    }

    #[test]
    fn rule_script_branches_on_the_request_view() {
        let mut script = Script::rule(|view| {
            if view.prior_tool_results == 0 {
                Reply {
                    turns: vec![Turn::ToolCall {
                        id: "call_1".to_string(),
                        name: "write".to_string(),
                        args: serde_json::json!({ "path": "x" }),
                    }],
                    usage: Usage::default(),
                    stop: StopReason::ToolCalls,
                }
            } else {
                Reply::text("done")
            }
        });

        // Turn 1: no prior tool results → a tool call.
        match script.next_action(&view_with_turns(0)) {
            ScriptAction::Reply(reply) => assert_eq!(reply.stop, StopReason::ToolCalls),
            other => panic!("expected a reply, got {other:?}"),
        }
        // Turn 2: one prior tool result → the final text.
        assert_eq!(script.next_action(&view_with_turns(1)), text("done"));
    }

    #[test]
    fn rule_closures_may_keep_state() {
        // FnMut: a counter lives in the closure instead of an AtomicUsize.
        let mut calls = 0;
        let mut script = Script::action_rule(move |_| {
            calls += 1;
            text(&format!("call {calls}"))
        });
        let view = view_with_turns(0);
        assert_eq!(script.next_action(&view), text("call 1"));
        assert_eq!(script.next_action(&view), text("call 2"));
    }

    #[test]
    fn phases_keep_independent_cursors_and_fall_back_to_an_empty_reply() {
        let mut script = Script::Phases(Phases(vec![
            phase("architect", "ROLE: architect", &["a1", "a2"]),
            phase("engineer", "ROLE: engineer", &["e1", "e2"]),
            phase("empty", "ROLE: empty", &[]),
        ]));
        let architect = view_with_message("ROLE: architect");
        let engineer = view_with_message("ROLE: engineer");

        assert_eq!(script.next_action(&architect), text("a1"));
        assert_eq!(script.next_action(&engineer), text("e1"));
        assert_eq!(script.next_action(&architect), text("a2"));
        assert_eq!(script.next_action(&architect), text("a2"));
        assert_eq!(script.next_action(&engineer), text("e2"));
        // A matching phase with no actions, and no matching phase at all.
        assert_eq!(
            script.next_action(&view_with_message("ROLE: empty")),
            text("")
        );
        assert_eq!(
            script.next_action(&view_with_message("ROLE: nobody")),
            text("")
        );
    }

    #[test]
    fn split_keeps_data_in_the_plan() {
        let view = view_with_turns(0);

        let (mut plan, rule) = Script::Fixed(Reply::text("same")).split();
        assert!(rule.is_none());
        assert_eq!(plan, Plan::Fixed(text("same")));
        assert_eq!(plan_action(&mut plan, &view), text("same"));
        assert_eq!(plan_action(&mut plan, &view), text("same"));

        let error = ScriptAction::HttpError(HttpError::provider(429, "slow_down", "wait"));
        let (mut plan, rule) = Script::fixed_action(error.clone()).split();
        assert!(rule.is_none());
        assert_eq!(plan_action(&mut plan, &view), error);

        let (mut plan, rule) =
            Script::sequence(vec![Reply::text("one"), Reply::text("two")]).split();
        assert!(rule.is_none());
        assert_eq!(plan_action(&mut plan, &view), text("one"));
        assert_eq!(plan_action(&mut plan, &view), text("two"));
        assert_eq!(plan_action(&mut plan, &view), text("two"));

        let (mut plan, rule) = Script::Phases(Phases(vec![
            phase("a", "ROLE: a", &["a1", "a2"]),
            phase("b", "ROLE: b", &["b1"]),
        ]))
        .split();
        assert!(rule.is_none());
        let (a, b) = (view_with_message("ROLE: a"), view_with_message("ROLE: b"));
        assert_eq!(plan_action(&mut plan, &a), text("a1"));
        assert_eq!(plan_action(&mut plan, &b), text("b1"));
        assert_eq!(plan_action(&mut plan, &a), text("a2"));
    }

    #[test]
    fn split_reference_delivery_is_data_and_matches_the_script() {
        let spec = ReferenceDeliverySpec::default();
        let view = view_with_message("ROLE: reviewer");
        let expected = Script::ReferenceDelivery(spec.clone()).next_action(&view);

        let (mut plan, rule) = Script::ReferenceDelivery(spec.clone()).split();
        assert!(rule.is_none());
        assert_eq!(plan, Plan::ReferenceDelivery(spec));
        assert_eq!(plan_action(&mut plan, &view), expected);
    }

    #[test]
    fn split_rule_leaves_the_decision_outside_the_plan() {
        let (mut plan, rule) = Script::rule(|_| Reply::text("ruled")).split();
        assert_eq!(plan, Plan::External);
        let view = view_with_turns(0);
        assert_eq!(plan.next(&view), Next::External);
        let mut rule = rule.expect("a rule script yields its rule");
        assert_eq!(rule.decide(&view), text("ruled"));
    }
}
