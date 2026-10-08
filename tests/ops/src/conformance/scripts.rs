//! Only the two alert batches' finite model responses, over skein's fake LLM.
use jig_conformance::{Clock, Input};
use jig_ops_domain as root;
use skein_fake_llm_domain::{
    self as provider,
    api::{Finish, Line, Script, Turn},
};
use skein_lib::{Duration, Env, Queue, ReplyTo, Time, Token, Wall};

pub struct Llm {
    provider: provider::Domain,
    limits: provider::Config,
}
impl Llm {
    pub fn new(seed: u64) -> Self {
        let mut limits = smith_agent_world::Settings::calm(seed).provider;
        limits.latency_min = Duration::ZERO;
        limits.latency_max = Duration::ZERO;
        limits.script_bytes = 65536;
        limits.query_bytes = 131_072;
        limits.answer_bytes = 8192;
        Self {
            provider: provider::Domain::configured(
                &limits,
                seed,
                scripts(),
                provider::api::Menu { arguments: Box::new([]), invalid: Box::new([]) },
            )
            .expect("finite ops scripts"),
            limits,
        }
    }
    pub fn answer(
        &mut self,
        clock: Clock,
        client: Token,
        request: smith_domain::Request,
    ) -> Vec<Input<root::Event, root::Record>> {
        let smith_domain::Request::Complete { owner, prompt, .. } = request else {
            panic!("alert model only receives completions")
        };
        let grants = prompt.tools;
        let served = prompt.served.clone();
        let mut query = smith_agent_world::translate::query(prompt);
        let has = |needle: &[u8]| query.system.windows(needle.len()).any(|part| part == needle);
        let lead = has(b"@incident_lead");
        let triage = has(b"Alerts: 12");
        let result = query.messages.iter().flat_map(|message| message.parts.iter()).any(|part| match part {
            provider::api::Part::Text { text } => {
                text.windows(b"Result from task ".len()).any(|bytes| bytes == b"Result from task ")
            }
            provider::api::Part::Opaque { .. }
            | provider::api::Part::ToolCall { .. }
            | provider::api::Part::ToolOutput { .. } => false,
        });
        let acted = query.messages.iter().flat_map(|message| message.parts.iter()).any(|part| match part {
            provider::api::Part::ToolCall { name, .. } => {
                name.as_ref() == if lead { b"propose".as_slice() } else { b"delegate".as_slice() }
            }
            provider::api::Part::Text { text } => {
                let needle =
                    if lead { b"tool=propose result:".as_slice() } else { b"tool=delegate result:".as_slice() };
                text.windows(needle.len()).any(|bytes| bytes == needle)
            }
            provider::api::Part::Opaque { .. } | provider::api::Part::ToolOutput { .. } => false,
        });
        let cue = if lead && result {
            Some(b"@lead_complete".as_slice())
        } else if triage && result {
            Some(b"@triage_complete".as_slice())
        } else if (lead || triage || has(b"@watch_setup")) && acted {
            Some(b"@await_result".as_slice())
        } else {
            None
        };
        if let Some(cue) = cue {
            let mut system = cue.to_vec();
            system.push(b'\n');
            system.extend_from_slice(&query.system);
            query.system = system.into_boxed_slice();
        }

        let env = Env { now: Time::from_nanos(clock.now), wall: Wall::from_nanos(clock.now), limits: self.limits };
        let mut out = Queue::with_capacity(provider::MAX_OUT);
        provider::step(
            &mut self.provider,
            &env,
            provider::Event::Call { reply_to: ReplyTo::new(owner), query },
            &mut out,
        );
        if out.is_empty() {
            provider::fire(&mut self.provider, &env, &mut out);
        }
        let provider::Request::Reply { to, result } = out.pop().expect("zero latency scripted response");
        assert_eq!(to.into_token(), owner);
        self.provider.reclaim();
        let completion = jig_inline_agent::Completion::Completed {
            owner,
            completion: smith_agent_world::translate::completion(
                result.expect("valid scripted conversation"),
                grants,
                &served,
            ),
        };
        vec![Input::Event(root::Event::Llm { client, completion })]
    }
}

#[expect(clippy::too_many_arguments, reason = "the fixture spells out the finite delegate envelope")]
pub fn delegate(
    words: &str,
    executor: &str,
    spend: u64,
    tasks: u32,
    depth: u32,
    production: bool,
    parameters: &str,
    wake: &str,
) -> String {
    let environment = if production { "production" } else { "staging" };
    format!(
        r#"{{"executor":{executor},"spec":{{"words":"{words}","parameters":[{parameters}]}},"contract":{{"kind":"report","words":128}},"authority":{{"tools":1023,"grants":[{{"connector":2,"kind":1,"segments":["env","{environment}","service"],"terminal":"open","last":""}},{{"connector":2,"kind":2,"segments":["env","{environment}","service"],"terminal":"open","last":""}},{{"connector":1,"kind":2,"segments":[],"terminal":"open","last":""}},{{"connector":1,"kind":3,"segments":[],"terminal":"open","last":""}},{{"connector":1,"kind":4,"segments":[],"terminal":"open","last":""}}],"delegation":{{"kinds":[{{"kind":"agent","number":1}},{{"kind":"procedure","number":1}},{{"kind":"procedure","number":2}}],"tasks":{tasks},"depth":{depth}}},"budget":{{"spend":{spend}}},"notes":0}}{wake}}}"#
    )
}
fn call(name: &str, arguments: String) -> Turn {
    Turn {
        lines: Box::new([Line::Call { name: name.as_bytes().into(), arguments: arguments.into_bytes().into() }]),
        finish: Finish::ToolCalls,
        tokens: 1,
    }
}
fn finish(text: &str) -> Turn {
    call("finish", format!(r#"{{"report":"{text}"}}"#))
}
fn yield_main() -> Turn {
    Turn {
        lines: Box::new([Line::Text { text: b"waiting for the delegated result".as_slice().into() }]),
        finish: Finish::Stop,
        tokens: 1,
    }
}
fn wait() -> Turn {
    call("wait", "{}".into())
}
fn logs() -> Turn {
    call(
        "read_logs",
        r#"{"environment":"production","service":"checkout","from":0,"through":7200,"max_bytes":256,"filter":""}"#
            .into(),
    )
}
fn scripts() -> Box<[Script]> {
    let watch = delegate(
        "watch checkout",
        r#"{"kind":"procedure","connector":1,"code":1}"#,
        80,
        5,
        3,
        false,
        r#"{"name":1,"kind":"bytes","value":"production"},{"name":2,"kind":"bytes","value":"checkout"},{"name":3,"kind":"number","value":1}"#,
        r#", "wake":{"words":{"kind":"immediate"},"notices":{"kind":"immediate"},"news":{"kind":"batch","count":3,"age":1000000000},"results":"last_or_failure","questions":true,"answers":true,"timers":true}"#,
    );
    let lead = delegate("@incident_lead", r#"{"kind":"agent","charter":1}"#, 40, 1, 1, false, "", "");
    let remediate = delegate(
        "restart checkout",
        r#"{"kind":"procedure","connector":2,"code":2}"#,
        10,
        0,
        0,
        true,
        r#"{"name":1,"kind":"bytes","value":"production"},{"name":2,"kind":"bytes","value":"checkout"},{"name":3,"kind":"number","value":77},{"name":4,"kind":"number","value":7200}"#,
        "",
    );
    Box::new([
        Script {
            cue: b"@lead_complete".as_slice().into(),
            turns: (0..32).map(|_| finish("checkout recovered after the accepted restart")).collect(),
        },
        Script {
            cue: b"@triage_complete".as_slice().into(),
            turns: (0..32).map(|_| finish("incident lead reported recovery")).collect(),
        },
        Script {
            cue: b"@await_result".as_slice().into(),
            turns: (0..32).map(|turn| if turn % 2 == 0 { wait() } else { yield_main() }).collect(),
        },
        Script {
            cue: b"@watch_setup".as_slice().into(),
            turns: vec![call("delegate", format!(r#"{{"batch":[{watch}]}}"#)), wait(), yield_main()].into(),
        },
        Script {
            cue: b"Alerts: 7 8 9".as_slice().into(),
            turns: vec![logs(), finish("known harmless error; noise")].into(),
        },
        Script {
            cue: b"Alerts: 12".as_slice().into(),
            turns: vec![
                logs(),
                call("delegate", format!(r#"{{"batch":[{lead}]}}"#)),
                wait(),
                yield_main(),
                finish("incident lead reported recovery"),
            ]
            .into(),
        },
        Script {
            cue: b"@incident_lead".as_slice().into(),
            turns: vec![
                call(
                    "read_series",
                    r#"{"environment":"production","service":"checkout","from":0,"through":7200,"max_bytes":256}"#
                        .into(),
                ),
                call(
                    "propose",
                    format!(r#"{{"action":"batch","reason":"restart checkout in production","batch":[{remediate}]}}"#),
                ),
                wait(),
                yield_main(),
                wait(),
                yield_main(),
                finish("checkout recovered after the accepted restart"),
            ]
            .into(),
        },
    ])
}

pub fn triage() -> Box<[u8]> {
    format!(
        r#"{{"batch":[{}]}}"#,
        delegate("triage checkout", r#"{"kind":"agent","charter":1}"#, 60, 2, 2, false, "", "")
    )
    .into_bytes()
    .into()
}
