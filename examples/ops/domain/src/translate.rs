//! Application vocabulary in small total functions. Copy the core/host pairs;
//! replace resource paths, effect kinds and tool declarations (domain/root.md, 6).
use alloc::boxed::Box;
use jig_core as core;
use jig_core_authority as authority;
use jig_core_tasks as tasks;
use jig_host as host;
use jig_ops_domain_infrastructure as infrastructure;
use jig_ops_domain_observability as observability;
use skein_lib::{List, Wall};

pub(crate) fn infrastructure_key(key: core::EffectKey) -> infrastructure::Key {
    let origin = match key.origin {
        core::EffectPurpose::Call { attempt, completion, position } => {
            infrastructure::Purpose::Call { attempt, completion, position }
        }
        core::EffectPurpose::Procedure { purpose } => infrastructure::Purpose::Procedure { purpose },
        core::EffectPurpose::Projection { purpose } => infrastructure::Purpose::Projection { purpose },
    };
    infrastructure::Key { deployment: key.deployment, task: key.task, origin, purpose: key.purpose }
}
pub(crate) fn observability_key(key: core::EffectKey) -> observability::Key {
    let origin = match key.origin {
        core::EffectPurpose::Call { attempt, completion, position } => {
            observability::Purpose::Call { attempt, completion, position }
        }
        core::EffectPurpose::Procedure { purpose } => observability::Purpose::Procedure { purpose },
        core::EffectPurpose::Projection { purpose } => observability::Purpose::Projection { purpose },
    };
    observability::Key { deployment: key.deployment, task: key.task, origin, purpose: key.purpose }
}
pub(crate) fn state(value: u64) -> [u8; 32] {
    let mut state = [0; 32];
    for (slot, byte) in state.get_mut(24..).expect("eight-byte state").iter_mut().zip(value.to_be_bytes()) {
        *slot = byte;
    }
    state
}
pub(crate) fn infrastructure_description(
    number: u16,
    value: infrastructure::Description,
) -> core::connector::EffectDescription {
    let mut names = List::with_capacity(u32::try_from(value.resources.len()).expect("bounded resources"));
    for resource in value.resources {
        names.push(authority::Name { segments: resource.segments() }).expect("one resource name");
    }
    let mut additional = List::with_capacity(names.len().saturating_sub(1));
    for name in names.as_slice().get(1..).unwrap_or_default() {
        additional
            .push(authority::EffectResource { name: name.clone(), access: authority::EffectAccess::Owned })
            .expect("additional names");
    }
    let form = match value.form {
        infrastructure::Form::Creation => core::connector::EffectForm::Creation,
        infrastructure::Form::Transition => core::connector::EffectForm::Transition,
        infrastructure::Form::Set => core::connector::EffectForm::Set,
    };
    let recovery = match value.recovery {
        infrastructure::Recovery::Keyed => core::connector::Recovery::Keyed,
        infrastructure::Recovery::Conditional => core::connector::Recovery::Conditional,

        infrastructure::Recovery::Unrecoverable => core::connector::Recovery::Unrecoverable,
    };
    core::connector::EffectDescription {
        connector: number,
        purpose: value.state,
        form,
        recovery,
        effect: authority::Effect {
            connector: number,
            kind: value.kind,
            name: names.as_slice().first().expect("described resource").clone(),
            state: state(value.state),
            price: value.price,
            access: authority::EffectAccess::Owned,
            additional: additional.into_boxed(),
            guards: Box::new([]),
        },
    }
}
pub(crate) fn observability_description(
    number: u16,
    value: observability::Effect,
) -> core::connector::EffectDescription {
    core::connector::EffectDescription {
        connector: number,
        purpose: value.until,
        form: core::connector::EffectForm::Set,
        recovery: core::connector::Recovery::Keyed,
        effect: authority::Effect {
            connector: number,
            kind: 1,
            name: authority::Name { segments: observability::Resource::AlertRule(value.rule).segments() },
            state: state(value.until),
            price: None,
            access: authority::EffectAccess::Owned,
            additional: Box::new([]),
            guards: Box::new([]),
        },
    }
}
pub(crate) const fn infrastructure_outcome(value: infrastructure::Outcome) -> core::connector::OutboxOutcome {
    match value {
        infrastructure::Outcome::Made => core::connector::OutboxOutcome::Made,
        infrastructure::Outcome::Failed => core::connector::OutboxOutcome::Failed,
        infrastructure::Outcome::Uncertain => core::connector::OutboxOutcome::Uncertain,
    }
}
pub(crate) const fn observability_outcome(value: observability::Outcome) -> core::connector::OutboxOutcome {
    match value {
        observability::Outcome::Made => core::connector::OutboxOutcome::Made,
        observability::Outcome::Failed => core::connector::OutboxOutcome::Failed,
        observability::Outcome::Uncertain => core::connector::OutboxOutcome::Uncertain,
    }
}
pub(crate) const fn verdict(value: observability::Verdict, wall: Wall) -> (authority::Verdict, Wall) {
    match value {
        observability::Verdict::Met { observed } => {
            (authority::Verdict::Met, Wall::from_nanos(observed.saturating_mul(1_000_000_000)))
        }
        observability::Verdict::Wait => (authority::Verdict::Wait, wall),
        observability::Verdict::Refuse => (authority::Verdict::Refuse, wall),
    }
}
pub(crate) fn service(names: &[authority::Name]) -> Option<observability::Service> {
    let segments = &names.first()?.segments;
    if segments.len() == 4 && segments.first()?.as_ref() == b"env" && segments.get(2)?.as_ref() == b"service" {
        return Some(observability::Service::new(segments.get(1)?.clone(), segments.get(3)?.clone()));
    }
    if segments.len() == 3 && segments.first()?.as_ref() == b"services" {
        Some(observability::Service::new(segments.get(1)?.clone(), segments.get(2)?.clone()))
    } else {
        None
    }
}

/// Ops names its two requirements in its connector vocabulary.
pub(crate) const fn requirement(number: u16) -> Option<observability::Requirement> {
    match number {
        1 => Some(observability::Requirement::HealthyReplicaElsewhere),
        2 => Some(observability::Requirement::LoadBelow { percent: 20, for_seconds: 1800 }),
        _ => None,
    }
}

/// Ops's procedure parameters are typed spec carriers, never root policy.
pub(crate) fn spec_service(spec: &tasks::Spec) -> Option<infrastructure::Service> {
    let mut environment = None;
    let mut name = None;
    for parameter in &spec.parameters {
        match parameter {
            tasks::Parameter::Bytes { name: 1, value } => environment = Some(value.clone()),
            tasks::Parameter::Bytes { name: 2, value } => name = Some(value.clone()),
            tasks::Parameter::Bytes { .. } | tasks::Parameter::Number { .. } | tasks::Parameter::Resource { .. } => {}
        }
    }
    Some(infrastructure::Service::new(environment?, name?))
}
pub(crate) fn spec_number(spec: &tasks::Spec, number: u32) -> Option<u64> {
    for parameter in &spec.parameters {
        match parameter {
            tasks::Parameter::Number { name, value } => {
                if *name == number {
                    return Some(*value);
                }
            }
            tasks::Parameter::Bytes { .. } | tasks::Parameter::Resource { .. } => {}
        }
    }
    None
}
pub(crate) fn procedure(code: u32, spec: &tasks::Spec) -> Option<infrastructure::Procedure> {
    let service = spec_service(spec)?;
    match code {
        1 => Some(infrastructure::Procedure::Scale {
            service,
            replicas: u32::try_from(spec_number(spec, 3)?).ok()?,
            deadline: spec_number(spec, 4)?,
        }),
        2 => Some(infrastructure::Procedure::Remediate {
            service,
            operation: spec_number(spec, 3)?,
            deadline: spec_number(spec, 4)?,
        }),
        _ => None,
    }
}
pub(crate) fn holdings(
    number: u16,
    infrastructure: u16,
    executor: tasks::Executor,
    spec: &tasks::Spec,
) -> Option<Box<[tasks::Holding]>> {
    if number != infrastructure {
        return Some(Box::new([]));
    }
    match executor {
        tasks::Executor::Procedure { connector, .. } if connector == infrastructure => {}
        tasks::Executor::Procedure { .. } | tasks::Executor::Agent { .. } | tasks::Executor::Person(_) => {
            return Some(Box::new([]));
        }
    }
    match spec_service(spec) {
        Some(service) => Some(Box::new([tasks::Holding::Write {
            resource: tasks::Name { connector: number, path: infrastructure::Resource::Service(service).segments() },
            kind: 1,
        }])),
        None => {
            if spec.parameters.is_empty() {
                Some(Box::new([]))
            } else {
                None
            }
        }
    }
}
pub(crate) fn name(name: smith_host_domain::CallName) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (slot, byte) in bytes.iter_mut().zip(
        name.activation
            .to_le_bytes()
            .into_iter()
            .chain(name.completion.to_le_bytes())
            .chain(name.position.to_le_bytes()),
    ) {
        *slot = byte;
    }
    bytes
}
pub(crate) fn named(bytes: &[u8]) -> Option<smith_host_domain::CallName> {
    if bytes.len() != 16 {
        return None;
    }
    Some(smith_host_domain::CallName {
        activation: u64::from_le_bytes(bytes.get(..8)?.try_into().ok()?),
        completion: u32::from_le_bytes(bytes.get(8..12)?.try_into().ok()?),
        position: u32::from_le_bytes(bytes.get(12..16)?.try_into().ok()?),
    })
}
pub(crate) const fn grant(grant: host::Grant) -> smith_host_domain::Grant {
    smith_host_domain::Grant { account: grant.account, generation: grant.generation, valid: grant.valid }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Problem {
    Malformed,
    TooLarge,
    Missing,
    Type,
    Range,
    UnknownTool,
}

pub(crate) mod json {
    //! Bounded JSON-object decoding with Skein's tokenizer. The decoded tree is
    //! temporary and never crosses the engine or Smith domain boundary.

    use alloc::boxed::Box;
    use skein_json::{Token, tokenizer};
    use skein_lib::stream::{Down, Up};
    use skein_lib::{Env, Intake, List, Queue, Stack, Time, Wall};

    use super::Problem;

    #[derive(Clone, PartialEq, Eq, Debug)]
    pub(crate) enum Value {
        Object(Box<[(Box<[u8]>, Value)]>),
        Array(Box<[Value]>),
        String(Box<[u8]>),
        Number(Box<[u8]>),
        Boolean(bool),
        Null,
    }

    pub(crate) fn parse(input: &[u8]) -> Result<Value, Problem> {
        if input.len() > 65_536 {
            return Err(Problem::TooLarge);
        }
        let Ok(length) = u32::try_from(input.len()) else { return Err(Problem::TooLarge) };
        let limits = tokenizer::Limits { depth: 16, string: length, number: 24, chunk: length.max(1), length };
        let mut machine = tokenizer::Tokenizer::new(&limits);
        let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits };
        let mut intake = Intake::with_capacity(length.max(tokenizer::largest_demand(&limits)));
        if intake.append(input).is_err() {
            return Err(Problem::TooLarge);
        }
        let mut above = Queue::with_capacity(1);
        let mut below = Queue::with_capacity(1);
        let mut tokens = List::with_capacity(4096);
        let mut demanded = None;
        let steps = input.len().checked_mul(4).ok_or(Problem::TooLarge)?;
        let steps = steps.checked_add(8).ok_or(Problem::TooLarge)?;
        for _ in 0..steps {
            match demanded.take() {
                Some(read) => match intake.meet(read) {
                    Some(bytes) => tokenizer::up(&mut machine, &env, Up::Bytes(bytes), &mut above, &mut below),
                    None => tokenizer::up(&mut machine, &env, Up::End, &mut above, &mut below),
                },
                None => tokenizer::down(&mut machine, &env, tokenizer::Request::Next, &mut above, &mut below),
            }
            match below.pop() {
                Some(Down::Demand { read, .. }) => demanded = Some(read),
                Some(Down::Send(_) | Down::Finish) => return Err(Problem::Malformed),
                None => {}
            }
            match above.pop() {
                Some(tokenizer::Event::Token(token)) => {
                    let Ok(()) = tokens.push(token) else { return Err(Problem::TooLarge) };
                }
                Some(tokenizer::Event::Done) => {
                    let tokens = tokens.into_boxed();
                    let value = value(tokens)?;
                    return match value {
                        Value::Object(_) => Ok(value),
                        Value::Array(_) | Value::String(_) | Value::Number(_) | Value::Boolean(_) | Value::Null => {
                            Err(Problem::Type)
                        }
                    };
                }
                Some(tokenizer::Event::Failed(_) | tokenizer::Event::Closed) => return Err(Problem::Malformed),
                None => {}
            }
        }
        Err(Problem::Malformed)
    }

    enum Frame {
        Object { fields: List<(Box<[u8]>, Value)>, key: Option<Box<[u8]>> },
        Array(List<Value>),
    }

    fn value(tokens: Box<[Token]>) -> Result<Value, Problem> {
        let mut stack = Stack::with_capacity(16);
        let mut result = None;
        for token in tokens {
            let item = match token {
                Token::ObjectStart => {
                    if stack.push(Frame::Object { fields: List::with_capacity(128), key: None }).is_err() {
                        return Err(Problem::TooLarge);
                    }
                    continue;
                }
                Token::ArrayStart => {
                    if stack.push(Frame::Array(List::with_capacity(128))).is_err() {
                        return Err(Problem::TooLarge);
                    }
                    continue;
                }
                Token::Key(next) => {
                    let Some(Frame::Object { fields, key }) = stack.top_mut() else { return Err(Problem::Malformed) };
                    if key.is_some() {
                        return Err(Problem::Malformed);
                    }
                    for (previous, _) in fields.as_slice() {
                        if previous == &next {
                            return Err(Problem::Malformed);
                        }
                    }
                    *key = Some(next);
                    continue;
                }
                Token::ObjectEnd => match stack.pop() {
                    Some(Frame::Object { fields, key: None }) => Value::Object(fields.into_boxed()),
                    Some(Frame::Object { key: Some(_), .. } | Frame::Array(_)) | None => {
                        return Err(Problem::Malformed);
                    }
                },
                Token::ArrayEnd => match stack.pop() {
                    Some(Frame::Array(items)) => Value::Array(items.into_boxed()),
                    Some(Frame::Object { .. }) | None => return Err(Problem::Malformed),
                },
                Token::String(text) => Value::String(text),
                Token::Number(text) => Value::Number(text),
                Token::True => Value::Boolean(true),
                Token::False => Value::Boolean(false),
                Token::Null => Value::Null,
            };
            match stack.top_mut() {
                Some(Frame::Object { fields, key }) => {
                    let key = key.take().ok_or(Problem::Malformed)?;
                    if fields.push((key, item)).is_err() {
                        return Err(Problem::TooLarge);
                    }
                }
                Some(Frame::Array(items)) => {
                    if items.push(item).is_err() {
                        return Err(Problem::TooLarge);
                    }
                }
                None => {
                    if result.is_some() {
                        return Err(Problem::Malformed);
                    }
                    result = Some(item);
                }
            }
        }
        if !stack.is_empty() {
            return Err(Problem::Malformed);
        }
        result.ok_or(Problem::Malformed)
    }

    impl Value {
        pub(crate) fn field(&self, name: &[u8]) -> Result<Option<&Value>, Problem> {
            match self {
                Value::Object(fields) => {
                    for (key, value) in fields {
                        if key.as_ref() == name {
                            return Ok(Some(value));
                        }
                    }
                    Ok(None)
                }
                Value::Array(_) | Value::String(_) | Value::Number(_) | Value::Boolean(_) | Value::Null => {
                    Err(Problem::Type)
                }
            }
        }

        pub(crate) fn required(&self, name: &[u8]) -> Result<&Value, Problem> {
            self.field(name)?.ok_or(Problem::Missing)
        }

        pub(crate) fn text(&self) -> Result<Box<[u8]>, Problem> {
            match self {
                Value::String(text) => Ok(text.clone()),
                Value::Object(_) | Value::Array(_) | Value::Number(_) | Value::Boolean(_) | Value::Null => {
                    Err(Problem::Type)
                }
            }
        }

        pub(crate) fn number(&self) -> Result<u64, Problem> {
            match self {
                Value::Number(text) => super::decimal(text).ok_or(Problem::Range),
                Value::Object(_) | Value::Array(_) | Value::String(_) | Value::Boolean(_) | Value::Null => {
                    Err(Problem::Type)
                }
            }
        }

        pub(crate) fn boolean(&self) -> Result<bool, Problem> {
            match self {
                Value::Boolean(value) => Ok(*value),
                Value::Object(_) | Value::Array(_) | Value::String(_) | Value::Number(_) | Value::Null => {
                    Err(Problem::Type)
                }
            }
        }

        pub(crate) fn array(&self) -> Result<&[Value], Problem> {
            match self {
                Value::Array(items) => Ok(items),
                Value::Object(_) | Value::String(_) | Value::Number(_) | Value::Boolean(_) | Value::Null => {
                    Err(Problem::Type)
                }
            }
        }

        pub(crate) fn small(&self) -> Result<u32, Problem> {
            let Ok(value) = u32::try_from(self.number()?) else { return Err(Problem::Range) };
            Ok(value)
        }

        pub(crate) fn narrow(&self) -> Result<u16, Problem> {
            let Ok(value) = u16::try_from(self.number()?) else { return Err(Problem::Range) };
            Ok(value)
        }
    }
}

pub(crate) mod nested {
    //! Typed task values admitted from bounded JSON host inputs. Root authority
    //! and tasks still check these values against current durable facts.

    use alloc::boxed::Box;
    use jig_core as engine;
    use jig_core_tasks as tasks;
    use skein_lib::{Duration, List, Wall};

    use super::{Problem, json::Value};

    pub(crate) fn delegate(value: &Value) -> Result<engine::Delegate, Problem> {
        let executor = executor(value.required(b"executor")?)?;
        let spec = spec(value.required(b"spec")?)?;
        let contract = contract(value.required(b"contract")?)?;
        let authority = authority(value.required(b"authority")?)?;
        let symbolic_grants = match value.field(b"symbolic_grants")? {
            Some(grants) => grants_list(grants)?,
            None => Box::new([]),
        };
        let dependencies = match value.field(b"dependencies")? {
            Some(items) => {
                let items = items.array()?;
                let Ok(capacity) = u32::try_from(items.len()) else { return Err(Problem::TooLarge) };
                let mut out = List::with_capacity(capacity);
                for item in items {
                    let kind = item.required(b"kind")?.text()?;
                    let dependency = match kind.as_ref() {
                        b"batch" => engine::Dependency::Batch(item.required(b"number")?.small()?),
                        b"existing" => engine::Dependency::Existing(item.required(b"number")?.number()?),
                        _ => return Err(Problem::Range),
                    };
                    let Ok(()) = out.push(dependency) else { return Err(Problem::TooLarge) };
                }
                out.into_boxed()
            }
            None => Box::new([]),
        };
        let wake = match value.field(b"wake")? {
            Some(wake) => wake_policy(wake)?,
            None => tasks::WakePolicy::DEFAULT,
        };
        Ok(engine::Delegate { executor, spec, contract, authority, symbolic_grants, dependencies, wake })
    }

    pub(crate) fn delegates(value: &Value) -> Result<Box<[engine::Delegate]>, Problem> {
        let items = value.array()?;
        let Ok(capacity) = u32::try_from(items.len()) else { return Err(Problem::TooLarge) };
        let mut out = List::with_capacity(capacity);
        for item in items {
            let Ok(()) = out.push(delegate(item)?) else { return Err(Problem::TooLarge) };
        }
        Ok(out.into_boxed())
    }

    pub(crate) fn amendment(value: &Value) -> Result<tasks::Amendment, Problem> {
        let spec = match value.field(b"spec")? {
            Some(value) => Some(spec(value)?),
            None => None,
        };
        let wake = match value.field(b"wake")? {
            Some(value) => Some(wake_policy(value)?),
            None => None,
        };
        let dependencies = match value.field(b"dependencies")? {
            Some(value) => Some(numbers(value)?),
            None => None,
        };
        let authority = match value.field(b"authority")? {
            Some(value) => Some(authority(value)?),
            None => None,
        };
        Ok(tasks::Amendment { spec, wake, dependencies, authority, reason: value.required(b"reason")?.text()? })
    }

    fn executor(value: &Value) -> Result<tasks::Executor, Problem> {
        let kind = value.required(b"kind")?.text()?;
        match kind.as_ref() {
            b"agent" => Ok(tasks::Executor::Agent { charter: value.required(b"charter")?.small()? }),
            b"procedure" => Ok(tasks::Executor::Procedure {
                connector: value.required(b"connector")?.narrow()?,
                code: value.required(b"code")?.small()?,
            }),
            b"person" => {
                Ok(tasks::Executor::Person(tasks::PersonAddress::Person(value.required(b"person")?.number()?)))
            }
            b"role" => Ok(tasks::Executor::Person(tasks::PersonAddress::Role(value.required(b"role")?.small()?))),
            _ => Err(Problem::Range),
        }
    }

    fn spec(value: &Value) -> Result<tasks::Spec, Problem> {
        let words = value.required(b"words")?.text()?;
        let parameters = match value.field(b"parameters")? {
            Some(items) => {
                let items = items.array()?;
                let Ok(capacity) = u32::try_from(items.len()) else { return Err(Problem::TooLarge) };
                let mut out = List::with_capacity(capacity);
                for item in items {
                    let name = item.required(b"name")?.small()?;
                    let kind = item.required(b"kind")?.text()?;
                    let parameter = match kind.as_ref() {
                        b"number" => tasks::Parameter::Number { name, value: item.required(b"value")?.number()? },
                        b"bytes" => tasks::Parameter::Bytes { name, value: item.required(b"value")?.text()? },
                        b"resource" => tasks::Parameter::Resource {
                            name,
                            connector: item.required(b"connector")?.narrow()?,
                            resource: item.required(b"resource")?.number()?,
                        },
                        _ => return Err(Problem::Range),
                    };
                    let Ok(()) = out.push(parameter) else { return Err(Problem::TooLarge) };
                }
                out.into_boxed()
            }
            None => Box::new([]),
        };
        let inputs = match value.field(b"inputs")? {
            Some(items) => numbers(items)?,
            None => Box::new([]),
        };
        Ok(tasks::Spec { words, parameters, inputs })
    }

    fn contract(value: &Value) -> Result<tasks::Contract, Problem> {
        let kind = value.required(b"kind")?.text()?;
        match kind.as_ref() {
            b"report" => Ok(tasks::Contract::Report { words: value.required(b"words")?.small()? }),
            b"change" => Ok(tasks::Contract::Change {
                connector: value.required(b"connector")?.narrow()?,
                kind: value.required(b"change_kind")?.narrow()?,
                words: value.required(b"words")?.small()?,
            }),
            b"verdict" => {
                let choices = value.required(b"choices")?.array()?;
                let Ok(capacity) = u32::try_from(choices.len()) else { return Err(Problem::TooLarge) };
                let mut out = List::with_capacity(capacity);
                for choice in choices {
                    let Ok(()) = out.push(tasks::Verdict {
                        code: choice.required(b"code")?.small()?,
                        words: choice.required(b"words")?.small()?,
                        followups: 0,
                    }) else {
                        return Err(Problem::TooLarge);
                    };
                }
                Ok(tasks::Contract::Verdict { choices: out.into_boxed() })
            }
            _ => Err(Problem::Range),
        }
    }

    pub(crate) fn authority(value: &Value) -> Result<tasks::Authority, Problem> {
        let tools = tasks::Tools(value.required(b"tools")?.number()?);
        let grants = grants_list(value.required(b"grants")?)?;
        let delegation = value.required(b"delegation")?;
        let kinds = delegation.required(b"kinds")?.array()?;
        let Ok(capacity) = u32::try_from(kinds.len()) else { return Err(Problem::TooLarge) };
        let mut permitted = List::with_capacity(capacity);
        for item in kinds {
            let kind = item.required(b"kind")?.text()?;
            let executor = match kind.as_ref() {
                b"agent" => tasks::AuthorityExecutor::Charter(item.required(b"number")?.small()?),
                b"procedure" => tasks::AuthorityExecutor::Procedure(item.required(b"number")?.small()?),
                b"role" => tasks::AuthorityExecutor::Role(item.required(b"number")?.small()?),
                _ => return Err(Problem::Range),
            };
            let Ok(()) = permitted.push(executor) else { return Err(Problem::TooLarge) };
        }
        let budget = value.required(b"budget")?;
        let deadline = match budget.field(b"deadline")? {
            Some(deadline) => Some(Wall::from_nanos(deadline.number()?)),
            None => None,
        };
        let Ok(notes) = u8::try_from(value.required(b"notes")?.narrow()?) else { return Err(Problem::Range) };
        let note_resources = match value.field(b"note_resources")? {
            Some(scopes) => note_resources_list(scopes)?,
            None => Box::new([]),
        };
        Ok(tasks::Authority {
            tools,
            grants,
            delegation: tasks::Delegation {
                kinds: permitted.into_boxed(),
                tasks: delegation.required(b"tasks")?.small()?,
                depth: delegation.required(b"depth")?.small()?,
            },
            budget: tasks::Budget { spend: budget.required(b"spend")?.number()?, deadline },
            notes: tasks::Scopes(notes),
            note_resources,
        })
    }

    fn note_resources_list(value: &Value) -> Result<Box<[tasks::ResourceScope]>, Problem> {
        let scopes = value.array()?;
        let Ok(capacity) = u32::try_from(scopes.len()) else { return Err(Problem::TooLarge) };
        let mut out = List::with_capacity(capacity);
        for scope in scopes {
            let pattern = scope.required(b"pattern")?;
            let terminal = pattern.required(b"terminal")?.text()?;
            let bytes = pattern.required(b"last")?.text()?;
            let last = match terminal.as_ref() {
                b"exact" => tasks::Last::Exact(bytes),
                b"open" => tasks::Last::Open(bytes),
                _ => return Err(Problem::Range),
            };
            let Ok(()) = out.push(tasks::ResourceScope {
                connector: scope.required(b"connector")?.narrow()?,
                pattern: tasks::Pattern { segments: texts(pattern.required(b"segments")?)?, last },
            }) else {
                return Err(Problem::TooLarge);
            };
        }
        Ok(out.into_boxed())
    }

    fn grants_list(value: &Value) -> Result<Box<[tasks::Grant]>, Problem> {
        let grants = value.array()?;
        let Ok(capacity) = u32::try_from(grants.len()) else { return Err(Problem::TooLarge) };
        let mut out = List::with_capacity(capacity);
        for grant in grants {
            let segments = texts(grant.required(b"segments")?)?;
            let terminal = grant.required(b"terminal")?.text()?;
            let bytes = grant.required(b"last")?.text()?;
            let last = match terminal.as_ref() {
                b"exact" => tasks::Last::Exact(bytes),
                b"open" => tasks::Last::Open(bytes),
                _ => return Err(Problem::Range),
            };
            let Ok(()) = out.push(tasks::Grant {
                connector: grant.required(b"connector")?.narrow()?,
                kind: grant.required(b"kind")?.narrow()?,
                pattern: tasks::Pattern { segments, last },
            }) else {
                return Err(Problem::TooLarge);
            };
        }
        Ok(out.into_boxed())
    }

    fn wake_policy(value: &Value) -> Result<tasks::WakePolicy, Problem> {
        Ok(tasks::WakePolicy {
            words: wake_rule(value.required(b"words")?)?,
            notices: wake_rule(value.required(b"notices")?)?,
            news: wake_rule(value.required(b"news")?)?,
            results: match value.required(b"results")?.text()?.as_ref() {
                b"never" => tasks::ResultsWake::Never,
                b"each" => tasks::ResultsWake::Each,
                b"last_or_failure" => tasks::ResultsWake::LastOrFailure,
                _ => return Err(Problem::Range),
            },
            questions: value.required(b"questions")?.boolean()?,
            answers: value.required(b"answers")?.boolean()?,
            timers: value.required(b"timers")?.boolean()?,
        })
    }

    fn wake_rule(value: &Value) -> Result<tasks::WakeRule, Problem> {
        match value.required(b"kind")?.text()?.as_ref() {
            b"never" => Ok(tasks::WakeRule::Never),
            b"immediate" => Ok(tasks::WakeRule::Immediate),
            b"batch" => Ok(tasks::WakeRule::Batch {
                count: value.required(b"count")?.small()?,
                age: Duration::from_nanos(value.required(b"age")?.number()?),
            }),
            _ => Err(Problem::Range),
        }
    }

    fn numbers(value: &Value) -> Result<Box<[u64]>, Problem> {
        let items = value.array()?;
        let Ok(capacity) = u32::try_from(items.len()) else { return Err(Problem::TooLarge) };
        let mut out = List::with_capacity(capacity);
        for item in items {
            let Ok(()) = out.push(item.number()?) else { return Err(Problem::TooLarge) };
        }
        Ok(out.into_boxed())
    }

    fn texts(value: &Value) -> Result<Box<[Box<[u8]>]>, Problem> {
        let items = value.array()?;
        let Ok(capacity) = u32::try_from(items.len()) else { return Err(Problem::TooLarge) };
        let mut out = List::with_capacity(capacity);
        for item in items {
            let Ok(()) = out.push(item.text()?) else { return Err(Problem::TooLarge) };
        }
        Ok(out.into_boxed())
    }
}

pub(crate) enum Decoded {
    Read(observability::Read),
    Action(core::NamedAction),
    Delegate(Box<[core::Delegate]>),
    Effect { effect: infrastructure::Effect, reason: Option<Box<[u8]>> },
}

pub(crate) fn tools(timeout: skein_lib::Duration) -> Box<[jig_charter::Tool]> {
    let names: &[&[u8]] = &[
        b"delegate",
        b"message",
        b"cancel",
        b"release",
        b"introduce",
        b"decide",
        b"withdraw",
        b"subscribe",
        b"unsubscribe",
        b"amend",
        b"decide_escalation",
        b"propose",
        b"note",
        b"recall",
        b"effect_restart",
        b"propose_restart",
        b"read_logs",
        b"read_series",
        b"read_alerts",
    ];
    let mut tools = List::with_capacity(u32::try_from(names.len()).expect("finite tool registry"));
    for name in names {
        tools
            .push(jig_charter::Tool {
                name: Box::from(*name),
                description: Box::from(*name),
                schema: Box::from(&br#"{"type":"object","additionalProperties":true}"#[..]),
                effect: if name.starts_with(b"read_") {
                    jig_charter::ToolEffect::Read
                } else {
                    jig_charter::ToolEffect::Write
                },
                timeout,
            })
            .expect("finite tool registry");
    }
    tools.into_boxed()
}
#[expect(clippy::too_many_lines, reason = "one exhaustive decoder lists the application tool vocabulary")]
pub(crate) fn decode(tool: &[u8], input: &[u8]) -> Result<Decoded, Problem> {
    let value = json::parse(input)?;
    let action = match tool {
        b"read_logs" | b"read_series" | b"read_alerts" => {
            let service = observability::Service::new(
                value.required(b"environment")?.text()?,
                value.required(b"service")?.text()?,
            );
            let window = observability::Window {
                from: value.required(b"from")?.number()?,
                through: value.required(b"through")?.number()?,
            };
            let max_bytes = value.required(b"max_bytes")?.small()?;
            let read = match tool {
                b"read_logs" => {
                    observability::Read::Logs { service, window, max_bytes, filter: value.required(b"filter")?.text()? }
                }
                b"read_series" => observability::Read::Series { service, window, max_bytes },
                b"read_alerts" => observability::Read::Fired { service, window, max_bytes },
                _ => unreachable!("read tool family"),
            };
            return Ok(Decoded::Read(read));
        }
        b"delegate" => return Ok(Decoded::Delegate(nested::delegates(value.required(b"batch")?)?)),
        b"effect_restart" | b"propose_restart" => {
            let effect = infrastructure::Effect::Restart {
                service: infrastructure::Service::new(
                    value.required(b"environment")?.text()?,
                    value.required(b"service")?.text()?,
                ),
                operation: value.required(b"operation")?.number()?,
            };
            let reason = if tool == b"propose_restart" { Some(value.required(b"reason")?.text()?) } else { None };
            return Ok(Decoded::Effect { effect, reason });
        }
        b"message" => {
            let kind = match value.required(b"form")?.text()?.as_ref() {
                b"words" => tasks::MessageKind::Words,
                b"question" => tasks::MessageKind::Question,
                b"answer" => tasks::MessageKind::Answer { question: value.required(b"question")?.number()? },
                _ => return Err(Problem::Range),
            };
            core::NamedAction::Message {
                target: value.required(b"target")?.number()?,
                kind,
                words: value.required(b"words")?.text()?,
            }
        }
        b"cancel" => core::NamedAction::Control {
            target: value.required(b"target")?.number()?,
            control: tasks::Control::Cancel { reason: value.required(b"reason")?.text()? },
        },
        b"release" => core::NamedAction::Control {
            target: value.required(b"target")?.number()?,
            control: tasks::Control::Release,
        },
        b"introduce" => core::NamedAction::Introduce {
            left: value.required(b"left")?.number()?,
            right: value.required(b"right")?.number()?,
        },
        b"withdraw" => core::NamedAction::WithdrawProposal { proposal: value.required(b"proposal")?.number()? },
        b"unsubscribe" => core::NamedAction::Unsubscribe { subscription: value.required(b"subscription")?.number()? },
        b"amend" => core::NamedAction::Amend {
            target: value.required(b"target")?.number()?,
            amendment: nested::amendment(value.required(b"amendment")?)?,
        },
        b"decide" => {
            let choice = match value.required(b"decision")?.text()?.as_ref() {
                b"accept" => core::ProposalChoice::Accept,
                b"reject" => core::ProposalChoice::Reject { reason: value.required(b"reason")?.text()? },
                b"pass" => core::ProposalChoice::Pass,
                _ => return Err(Problem::Range),
            };
            core::NamedAction::DecideProposal {
                proposer: value.required(b"proposer")?.number()?,
                proposal: value.required(b"proposal")?.number()?,
                choice,
            }
        }
        b"decide_escalation" => {
            let choice = match value.required(b"decision")?.text()?.as_ref() {
                b"release" => core::EscalationChoice::Release,
                b"reject" => core::EscalationChoice::Reject { reason: value.required(b"reason")?.text()? },
                b"pass" => core::EscalationChoice::Pass,
                _ => return Err(Problem::Range),
            };
            core::NamedAction::DecideEscalation {
                task: value.required(b"task")?.number()?,
                revision: value.required(b"revision")?.number()?,
                choice,
            }
        }
        b"subscribe" => {
            let kind = match value.required(b"kind")?.text()?.as_ref() {
                b"task" => tasks::SubscriptionKind::Task {
                    target: value.required(b"target")?.number()?,
                    held: false,
                    result: true,
                },
                b"timer" => tasks::SubscriptionKind::Timer {
                    at: Wall::from_nanos(value.required(b"at")?.number()?),
                    period: match value.field(b"period")? {
                        Some(value) => Some(skein_lib::Duration::from_nanos(value.number()?)),
                        None => None,
                    },
                },
                _ => return Err(Problem::Range),
            };
            core::NamedAction::Subscribe { kind }
        }
        b"propose" => {
            let reason = value.required(b"reason")?.text()?;
            let kind = value.required(b"action")?.text()?;
            match kind.as_ref() {
                b"batch" => core::NamedAction::ProposeBatch {
                    batch: nested::delegates(value.required(b"batch")?)?,
                    reason,
                    as_holder: false,
                },
                _ => return Err(Problem::Range),
            }
        }
        b"note" => {
            let (entry, recalled) = notes::new(&value)?;
            core::NamedAction::Note { entry, recalled }
        }
        b"recall" => {
            let (by, page) = notes::recall(&value)?;
            core::NamedAction::Recall { by, page }
        }
        _ => return Err(Problem::UnknownTool),
    };
    Ok(Decoded::Action(action))
}
pub(crate) fn decimal(bytes: &[u8]) -> Option<u64> {
    let mut value = 0_u64;
    if bytes.is_empty() {
        return None;
    }
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(u64::from(byte.checked_sub(b'0')?))?;
    }
    Some(value)
}

mod notes {
    //! Smith's JSON shapes for jig's scoped note and recall tools.
    //! The root checks bounds and authority before any value becomes durable.

    use jig_core_notes as notes;
    use skein_lib::List;

    use super::{Problem, json::Value};

    pub(crate) fn new(value: &Value) -> Result<(notes::New, Option<u32>), Problem> {
        let recalled = match value.field(b"recalled")? {
            Some(revision) => Some(revision.small()?),
            None => None,
        };
        let name = match value.field(b"name")? {
            Some(name) => name.number()?,
            None => 0,
        };
        if (recalled.is_some() && name == 0) || (recalled.is_none() && name != 0) {
            return Err(Problem::Range);
        }
        let references = match value.field(b"references")? {
            Some(value) => numbers(value)?,
            None => List::with_capacity(0),
        };
        Ok((
            notes::New {
                name,
                scope: scope(value.required(b"scope")?)?,
                description: value.required(b"description")?.text()?,
                body: value.required(b"body")?.text()?,
                references,
                author: notes::Author::Task { task: 0, attempt: 0 },
            },
            recalled,
        ))
    }

    pub(crate) fn recall(value: &Value) -> Result<(notes::Recall, u32), Problem> {
        let page = match value.field(b"page")? {
            Some(page) => page.small()?,
            None => 0,
        };
        let by = match value.required(b"kind")?.text()?.as_ref() {
            b"name" => {
                let name = value.required(b"name")?.number()?;
                if name == 0 {
                    return Err(Problem::Range);
                }
                notes::Recall::Name { name }
            }
            b"search" => {
                let values = value.required(b"scopes")?.array()?;
                let Ok(capacity) = u32::try_from(values.len()) else { return Err(Problem::TooLarge) };
                let mut scopes = List::with_capacity(capacity);
                for value in values {
                    if scopes.push(scope(value)?).is_err() {
                        return Err(Problem::TooLarge);
                    }
                }
                notes::Recall::Search { scopes, query: value.required(b"query")?.text()? }
            }
            _ => return Err(Problem::Range),
        };
        Ok((by, page))
    }

    fn scope(value: &Value) -> Result<notes::Scope, Problem> {
        match value.required(b"kind")?.text()?.as_ref() {
            b"deployment" => Ok(notes::Scope::Deployment),
            b"project" => Ok(notes::Scope::Project { project: value.required(b"project")?.small()? }),
            b"goal" => Ok(notes::Scope::Goal {
                project: value.required(b"project")?.small()?,
                goal: value.required(b"goal")?.number()?,
            }),
            b"resources" => {
                let pattern = value.required(b"pattern")?;
                let values = pattern.required(b"segments")?.array()?;
                let Ok(capacity) = u32::try_from(values.len()) else { return Err(Problem::TooLarge) };
                let mut segments = List::with_capacity(capacity);
                for segment in values {
                    if segments.push(segment.text()?).is_err() {
                        return Err(Problem::TooLarge);
                    }
                }
                let last = match pattern.required(b"terminal")?.text()?.as_ref() {
                    b"exact" => notes::Last::Exact(pattern.required(b"last")?.text()?),
                    b"open" => notes::Last::Open(pattern.required(b"last")?.text()?),
                    _ => return Err(Problem::Range),
                };
                Ok(notes::Scope::Resources {
                    project: value.required(b"project")?.small()?,
                    connector: value.required(b"connector")?.narrow()?,
                    pattern: notes::Pattern { segments: segments.into_boxed(), last },
                })
            }
            _ => Err(Problem::Range),
        }
    }

    fn numbers(value: &Value) -> Result<List<u64>, Problem> {
        let values = value.array()?;
        let Ok(capacity) = u32::try_from(values.len()) else { return Err(Problem::TooLarge) };
        let mut out = List::with_capacity(capacity);
        for value in values {
            let number = value.number()?;
            if number == 0 {
                return Err(Problem::Range);
            }
            if out.push(number).is_err() {
                return Err(Problem::TooLarge);
            }
        }
        Ok(out)
    }
}

pub(crate) const fn news_class(value: observability::Class) -> tasks::NewsClass {
    match value {
        observability::Class::Wake => tasks::NewsClass::Wakes,
        observability::Class::Keep => tasks::NewsClass::Kept,
    }
}
pub(crate) fn alert_words(alert: &observability::Alert) -> Box<[u8]> {
    let bytes = b"alert "
        .len()
        .checked_add(skein_lib::Decimal::of(alert.number).as_bytes().len())
        .expect("alert number")
        .checked_add(b" severity ".len())
        .expect("severity heading")
        .checked_add(skein_lib::Decimal::of(u64::from(alert.severity)).as_bytes().len())
        .expect("severity");
    let mut writer = skein_lib::Writer::new(bytes);
    writer.put(b"alert ").expect("alert heading");
    writer.put(skein_lib::Decimal::of(alert.number).as_bytes()).expect("alert identity");
    writer.put(b" severity ").expect("severity heading");
    writer.put(skein_lib::Decimal::of(u64::from(alert.severity)).as_bytes()).expect("severity");
    writer.finish()
}
pub(crate) fn health_words(_service: &observability::Service, healthy: bool) -> Box<[u8]> {
    if healthy { Box::from(&b"healthy"[..]) } else { Box::from(&b"unhealthy"[..]) }
}
pub(crate) fn triage(delegate: &[u8], alerts: &[u64]) -> Option<Box<[core::Delegate]>> {
    let value = json::parse(delegate).ok()?;
    let mut batch = nested::delegates(value.required(b"batch").ok()?).ok()?;
    for member in &mut batch {
        let mut room = member.spec.words.len().checked_add(b"\nAlerts:".len())?;
        for alert in alerts {
            room = room.checked_add(1)?.checked_add(skein_lib::Decimal::of(*alert).as_bytes().len())?;
        }
        let mut words = skein_lib::Writer::new(room);
        words.put(&member.spec.words).ok()?;
        words.put(b"\nAlerts:").ok()?;
        for alert in alerts {
            words.put(b" ").ok()?;
            words.put(skein_lib::Decimal::of(*alert).as_bytes()).ok()?;
        }
        member.spec.words = words.finish();
    }
    Some(batch)
}

/// Read kinds and resources belong to the application's connector vocabulary.
pub(crate) fn read_description(number: u16, read: &observability::Read) -> authority::Effect {
    let (kind, resource) = match read {
        observability::Read::Logs { service, .. } => (2, observability::Resource::Logs(service.clone())),
        observability::Read::Series { service, .. } => (3, observability::Resource::Metrics(service.clone())),
        observability::Read::Fired { service, .. } => (4, observability::Resource::Service(service.clone())),
    };
    authority::Effect {
        connector: number,
        kind,
        name: authority::Name { segments: resource.segments() },
        state: [0; 32],
        price: None,
        access: authority::EffectAccess::Context,
        additional: Box::new([]),
        guards: Box::new([]),
    }
}

/// The connector owns adoption roles; the root carries them into core admission.
pub(crate) const fn resource_role(role: infrastructure::ResourceRole) -> core::connector::ResourceRole {
    match role {
        infrastructure::ResourceRole::Owned => core::connector::ResourceRole::Owned,
        infrastructure::ResourceRole::Participant => core::connector::ResourceRole::Participant,
        infrastructure::ResourceRole::Context => core::connector::ResourceRole::Context,
        infrastructure::ResourceRole::Unavailable => core::connector::ResourceRole::Unavailable,
    }
}

/// The same configured service-write kind is used by holdings translation and cold restore.
pub(crate) fn hold_kinds(number: u16) -> Box<[tasks::Kind]> {
    Box::new([tasks::Kind {
        connector: number,
        kind: 1,
        hold: tasks::HoldKind::Exclusive { taken: tasks::Taken::Waits },
    }])
}
