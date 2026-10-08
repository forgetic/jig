//! The brief's section inventory and byte plan (domain/engine.md, section 9).
//! Core sections arrive as text. A connector's section is only its
//! number, kind, token and reported size; its bytes stay with the connector.
//! The plan gives required sections first claim on the budget, then visits
//! optional sections by priority. A connector cuts its own content to the
//! size requested here and reports the resulting size before completion.

use alloc::boxed::Box;

use skein_lib::{List, Token};

/// A section the core owns and renders in its own words.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Core {
    /// The task's specification and result contract.
    Task,
    /// Its requesters' specifications, nearest first.
    Lineage,
    /// Messages since the task's last run, keeping people's words whole.
    Inbox,
    /// Dependencies' and inputs' results.
    Results,
    /// Delegates, their states and results that came since the last turn.
    Plan,
    /// Earlier attempts and why they failed.
    Attempts,
    /// Calls committed since the last turn when the run does not resume.
    Calls,
    /// Proposals and escalations waiting for this run.
    Waiting,
    /// The index of notes in scope.
    NotesIndex,
    /// The end of a transcript when it cannot be resumed whole.
    TranscriptTail,
}

/// One requested section. A lower `priority` is kept before a higher one.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Planned {
    /// Text the core owns and may cut by this kind's rules.
    Core { kind: Core, text: Box<[u8]>, limit: u32, priority: u16, required: bool },
    /// Opaque content retained by a numbered connector.
    Connector { connector: u16, kind: u16, token: Token, size: u32, limit: u32, priority: u16, required: bool },
}

impl Planned {
    /// The size reported by the owner before cutting.
    #[must_use]
    pub fn size(&self) -> u32 {
        match self {
            Planned::Core { text, .. } => u32::try_from(text.len()).unwrap_or(u32::MAX),
            Planned::Connector { size, .. } => *size,
        }
    }

    /// The largest rendered section this owner may deliver.
    #[must_use]
    pub fn limit(&self) -> u32 {
        match self {
            Planned::Core { limit, .. } | Planned::Connector { limit, .. } => *limit,
        }
    }

    /// Whether absence of this section fails the brief.
    #[must_use]
    pub fn required(&self) -> bool {
        match self {
            Planned::Core { required, .. } | Planned::Connector { required, .. } => *required,
        }
    }

    /// Which sections receive room first.
    #[must_use]
    pub fn priority(&self) -> u16 {
        match self {
            Planned::Core { priority, .. } | Planned::Connector { priority, .. } => *priority,
        }
    }
}

/// A section's position and its allotted size in a completed brief.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Placement {
    /// The position of its source in the incoming inventory.
    pub index: u32,
    /// The most bytes this section may take after cuts.
    pub size: u32,
}

/// What a connector must do with a section it holds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ConnectorAction {
    /// Cut its section by its own rules to this size, then report the size.
    CutTo { connector: u16, token: Token, size: u32 },
    /// Release an optional section that has no room.
    Drop { connector: u16, token: Token },
}

/// The budget plan, preserving source order for the final assignment.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plan {
    /// Every retained section and its allotment, in source order.
    pub order: Box<[Placement]>,
    /// Requests to connector owners, in the order sections were considered.
    pub connectors: Box<[ConnectorAction]>,
    /// The sum of the allotted sizes.
    pub size: u32,
}

/// Plan sections by requirement and priority within `budget`. A required
/// inbox must fit whole; other sections can be cut by their owner.
/// Returns `None` when the inventory or a required inbox cannot fit.
#[must_use]
pub fn plan(sections: &[Planned], budget: u32) -> Option<Plan> {
    let count = u32::try_from(sections.len()).ok()?;
    let mut seen = List::with_capacity(count);
    let mut sizes = List::with_capacity(count);
    for _ in sections {
        seen.push(false).ok()?;
        sizes.push(0_u32).ok()?;
    }
    let mut connectors = List::with_capacity(count);
    let mut left = budget;
    for _ in sections {
        let mut selected = None;
        for (index, section) in sections.iter().enumerate() {
            let index = u32::try_from(index).ok()?;
            if *seen.get(index)? {
                continue;
            }
            match selected {
                Some(previous) => {
                    let old = sections.get(usize::try_from(previous).ok()?)?;
                    if (section.required() && !old.required())
                        || (section.required() == old.required() && section.priority() < old.priority())
                    {
                        selected = Some(index);
                    }
                }
                None => selected = Some(index),
            }
        }
        let index = selected?;
        *seen.get_mut(index)? = true;
        let section = sections.get(usize::try_from(index).ok()?)?;
        let wanted = section.size();
        let allotted = wanted.min(section.limit()).min(left);
        let whole = match section {
            Planned::Core { kind: Core::Inbox, required: true, .. } => true,
            Planned::Core {
                kind:
                    Core::Task
                    | Core::Lineage
                    | Core::Inbox
                    | Core::Results
                    | Core::Plan
                    | Core::Attempts
                    | Core::Calls
                    | Core::Waiting
                    | Core::NotesIndex
                    | Core::TranscriptTail,
                ..
            }
            | Planned::Connector { .. } => false,
        };
        if whole && allotted < wanted {
            return None;
        }
        *sizes.get_mut(index)? = allotted;
        left = left.checked_sub(allotted)?;
        match section {
            Planned::Core { .. } => {}
            Planned::Connector { connector, token, .. } => {
                let action = if allotted == 0 && !section.required() {
                    ConnectorAction::Drop { connector: *connector, token: *token }
                } else if allotted < wanted {
                    ConnectorAction::CutTo { connector: *connector, token: *token, size: allotted }
                } else {
                    continue;
                };
                connectors.push(action).ok()?;
            }
        }
    }
    let mut order = List::with_capacity(count);
    for (index, section) in sections.iter().enumerate() {
        let index = u32::try_from(index).ok()?;
        let size = *sizes.get(index)?;
        if size > 0 || section.required() {
            order.push(Placement { index, size }).ok()?;
        }
    }
    Some(Plan { order: order.into_boxed(), connectors: connectors.into_boxed(), size: budget.checked_sub(left)? })
}

#[cfg(test)]
mod tests {
    use super::{ConnectorAction, Core, Placement, Planned, plan};
    use alloc::boxed::Box;
    use skein_lib::Token;

    #[test]
    fn required_sections_claim_budget_before_optional_sections() {
        let sections = [
            Planned::Connector {
                connector: 3,
                kind: 7,
                token: Token::new(1),
                size: 80,
                limit: 80,
                priority: 0,
                required: false,
            },
            Planned::Core { kind: Core::Task, text: Box::from(&b"task"[..]), limit: 30, priority: 9, required: true },
            Planned::Connector {
                connector: 4,
                kind: 2,
                token: Token::new(2),
                size: 20,
                limit: 20,
                priority: 1,
                required: true,
            },
        ];
        let planned = plan(&sections, 30).unwrap();
        assert_eq!(planned.size, 30);
        assert_eq!(
            planned.order.as_ref(),
            [Placement { index: 0, size: 6 }, Placement { index: 1, size: 4 }, Placement { index: 2, size: 20 }]
        );
        assert_eq!(
            planned.connectors.as_ref(),
            [ConnectorAction::CutTo { connector: 3, token: Token::new(1), size: 6 }]
        );
    }

    #[test]
    fn an_optional_connector_without_room_is_dropped_by_token() {
        let sections = [
            Planned::Core { kind: Core::Task, text: Box::from(&b"task"[..]), limit: 4, priority: 0, required: true },
            Planned::Connector {
                connector: 2,
                kind: 9,
                token: Token::new(5),
                size: 100,
                limit: 100,
                priority: 0,
                required: false,
            },
        ];
        let planned = plan(&sections, 4).unwrap();
        assert_eq!(planned.order.len(), 1);
        assert_eq!(planned.connectors.as_ref(), [ConnectorAction::Drop { connector: 2, token: Token::new(5) }]);
    }

    #[test]
    fn a_required_inbox_is_kept_whole() {
        let sections = [Planned::Core {
            kind: Core::Inbox,
            text: Box::from(&b"human words"[..]),
            limit: 11,
            priority: 0,
            required: true,
        }];
        assert_eq!(plan(&sections, 10), None);
        assert_eq!(plan(&sections, 11).unwrap().order[0].size, 11);
    }

    #[test]
    fn a_kind_limit_cuts_a_connector_with_room_left_in_the_brief() {
        let token = Token::new(6);
        let sections =
            [Planned::Connector { connector: 2, kind: 4, token, size: 80, limit: 40, priority: 0, required: true }];
        let planned = plan(&sections, 200).expect("one bounded section");
        assert_eq!(planned.size, 40);
        assert_eq!(planned.order.as_ref(), [Placement { index: 0, size: 40 }]);
        assert_eq!(planned.connectors.as_ref(), [ConnectorAction::CutTo { connector: 2, token, size: 40 }]);
    }
}
