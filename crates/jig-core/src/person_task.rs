//! Authenticated person-task standing and task handoff
//! (domain/engine.md, section 4.4; domain/people.md, section 5).

use jig_core_people as people;
use jig_core_tasks as tasks;
use skein_lib::{ReplyTo, Token};

use crate::{Core, PersonTaskRoute};

fn person_result(result: people::PersonResult) -> tasks::TaskResult {
    match result {
        people::PersonResult::Report { words } => tasks::TaskResult::Report { words },
        people::PersonResult::Verdict { code, words } => tasks::TaskResult::Verdict { code, words },
        people::PersonResult::Failure { reason } => tasks::TaskResult::Failure { reason },
    }
}

impl Core {
    #[expect(clippy::too_many_lines, reason = "one exhaustive route handles all person task requests")]
    pub fn person_task(
        &mut self,
        request: Token,
        person: u64,
        project: u32,
        role: Option<people::Role>,
        ask: people::Ask,
    ) -> Result<tasks::Event, people::Refusal> {
        let task = match &ask {
            people::Ask::TakePerson { task, .. }
            | people::Ask::HandBackPerson { task, .. }
            | people::Ask::AnswerPerson { task, .. } => *task,
            people::Ask::Move { .. }
            | people::Ask::DecideProposal { .. }
            | people::Ask::Say { .. }
            | people::Ask::AnswerQuestion { .. }
            | people::Ask::Prioritise { .. }
            | people::Ask::Amend { .. }
            | people::Ask::Watch { .. }
            | people::Ask::EditNote { .. }
            | people::Ask::MakeService { .. }
            | people::Ask::SetRoles { .. }
            | people::Ask::Adopt { .. }
            | people::Ask::ChangePolicy { .. }
            | people::Ask::SetPool { .. }
            | people::Ask::DecideEscalation { .. }
            | people::Ask::StartChat { .. }
            | people::Ask::Stop { .. }
            | people::Ask::Cancel { .. }
            | people::Ask::Release { .. }
            | people::Ask::SetGoal { .. } => unreachable!("person-task route owns its ask"),
        };
        let Some(context) = self.tasks.delegation(task) else {
            return Err(people::Refusal::Ended);
        };
        let allowed = context.project == project
            && match self.tasks.executor(task).expect("live task has executor") {
                tasks::Executor::Person(tasks::PersonAddress::Person(address)) => {
                    address == person
                        && match &ask {
                            people::Ask::AnswerPerson { .. } => true,
                            people::Ask::TakePerson { .. } | people::Ask::HandBackPerson { .. } => false,
                            people::Ask::Move { .. }
                            | people::Ask::DecideProposal { .. }
                            | people::Ask::Say { .. }
                            | people::Ask::AnswerQuestion { .. }
                            | people::Ask::Prioritise { .. }
                            | people::Ask::Amend { .. }
                            | people::Ask::Watch { .. }
                            | people::Ask::EditNote { .. }
                            | people::Ask::MakeService { .. }
                            | people::Ask::SetRoles { .. }
                            | people::Ask::Adopt { .. }
                            | people::Ask::ChangePolicy { .. }
                            | people::Ask::SetPool { .. }
                            | people::Ask::DecideEscalation { .. }
                            | people::Ask::StartChat { .. }
                            | people::Ask::Stop { .. }
                            | people::Ask::Cancel { .. }
                            | people::Ask::Release { .. }
                            | people::Ask::SetGoal { .. } => unreachable!("person-task route owns its ask"),
                        }
                }
                tasks::Executor::Person(tasks::PersonAddress::Role(address)) => {
                    let handing_back = match &ask {
                        people::Ask::HandBackPerson { .. } => true,
                        people::Ask::TakePerson { .. } | people::Ask::AnswerPerson { .. } => false,
                        people::Ask::Move { .. }
                        | people::Ask::DecideProposal { .. }
                        | people::Ask::Say { .. }
                        | people::Ask::AnswerQuestion { .. }
                        | people::Ask::Prioritise { .. }
                        | people::Ask::Amend { .. }
                        | people::Ask::Watch { .. }
                        | people::Ask::EditNote { .. }
                        | people::Ask::MakeService { .. }
                        | people::Ask::SetRoles { .. }
                        | people::Ask::Adopt { .. }
                        | people::Ask::ChangePolicy { .. }
                        | people::Ask::SetPool { .. }
                        | people::Ask::DecideEscalation { .. }
                        | people::Ask::StartChat { .. }
                        | people::Ask::Stop { .. }
                        | people::Ask::Cancel { .. }
                        | people::Ask::Release { .. }
                        | people::Ask::SetGoal { .. } => unreachable!("person-task route owns its ask"),
                    };
                    handing_back
                        || match role {
                            Some(holding) => holding.number() == address,
                            None => false,
                        }
                }
                tasks::Executor::Agent { .. } | tasks::Executor::Procedure { .. } => false,
            };
        if !allowed {
            return Err(people::Refusal::Standing);
        }
        let (route, event) = match ask {
            people::Ask::TakePerson { .. } => (
                PersonTaskRoute::Take(task),
                tasks::Event::TakePerson { reply_to: ReplyTo::new(request), task, person },
            ),
            people::Ask::HandBackPerson { .. } => (
                PersonTaskRoute::HandBack(task),
                tasks::Event::HandBackPerson { reply_to: ReplyTo::new(request), task, person },
            ),
            people::Ask::AnswerPerson { result, .. } => (
                PersonTaskRoute::Answer(task),
                tasks::Event::AnswerPerson {
                    reply_to: ReplyTo::new(request),
                    task,
                    person,
                    result: person_result(result),
                },
            ),
            people::Ask::Move { .. }
            | people::Ask::DecideProposal { .. }
            | people::Ask::Say { .. }
            | people::Ask::AnswerQuestion { .. }
            | people::Ask::Prioritise { .. }
            | people::Ask::Amend { .. }
            | people::Ask::Watch { .. }
            | people::Ask::EditNote { .. }
            | people::Ask::MakeService { .. }
            | people::Ask::SetRoles { .. }
            | people::Ask::Adopt { .. }
            | people::Ask::ChangePolicy { .. }
            | people::Ask::SetPool { .. }
            | people::Ask::DecideEscalation { .. }
            | people::Ask::StartChat { .. }
            | people::Ask::Stop { .. }
            | people::Ask::Cancel { .. }
            | people::Ask::Release { .. }
            | people::Ask::SetGoal { .. } => unreachable!("person-task route owns its ask"),
        };
        assert!(self.person_tasks.insert(request, route) == Ok(None), "one routed person task per keyed flight");
        Ok(event)
    }
}
