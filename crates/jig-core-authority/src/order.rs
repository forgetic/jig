//! Segment coverage and componentwise authority order (domain/authority.md, 4–5).

use alloc::boxed::Box;

use skein_lib::{List, Queue};

use crate::{Authority, Grant, Last, Limits, Name, Numbers, Pattern, left};

/// Resolve open-prefix symbolic grants after the root assigns a task number.
/// Each terminal gains that number in decimal, so its resulting pattern is a
/// subset of the prefix checked with the creator's batch. Returns `None` when
/// the result exceeds admission bounds; callers then refuse the whole batch.
#[must_use]
pub fn resolve_task_grants(symbolic: &[Grant], task: u64, limits: &Limits) -> Option<Box<[Grant]>> {
    if u32::try_from(symbolic.len()).ok()? > limits.grants {
        return None;
    }
    let mut decimal = [0_u8; 20];
    let mut at = decimal.len();
    let mut remaining = task;
    for _ in 0..decimal.len() {
        at = at.checked_sub(1)?;
        *decimal.get_mut(at)? = b'0'.checked_add(u8::try_from(remaining % 10).ok()?)?;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    if remaining != 0 {
        return None;
    }
    let mut resolved = List::with_capacity(limits.grants);
    for grant in symbolic {
        let Last::Open(prefix) = &grant.pattern.last else { return None };
        let mut terminal = List::with_capacity(limits.segment_bytes);
        for byte in prefix {
            terminal.push(*byte).ok()?;
        }
        for byte in decimal.get(at..)? {
            terminal.push(*byte).ok()?;
        }
        let pattern = Pattern { segments: grant.pattern.segments.clone(), last: Last::Exact(terminal.into_boxed()) };
        if !crate::limits::pattern_within(&pattern, limits) {
            return None;
        }
        resolved.push(Grant { connector: grant.connector, kind: grant.kind, pattern }).ok()?;
    }
    Some(resolved.into_boxed())
}

/// One component deficit emitted by the pure `fits` query, without modifying either authority
/// value. (domain/authority.md, sections 4–5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Lack {
    /// Requested tool families do not fit.
    Tools,
    /// Requested resource grants do not fit.
    Grants,
    /// Requested executor kinds do not fit.
    Executors,
    /// Lifetime task capacity does not fit.
    Tasks,
    /// Descendant depth does not fit, including the creation level.
    Depth,
    /// Spend does not fit authority and current available funding.
    Spend,
    /// Ending deadline is too late.
    Deadline,
    /// Requested note scopes do not fit.
    Notes,
}

/// Every independent component that failed; a check can name all of them
/// in one bounded finding without losing the stricter answer.
/// Independent component deficits summarized in one fixed-size value; ordering is permission
/// inclusion, not structural equality. (domain/authority.md, sections 4–5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[expect(clippy::struct_excessive_bools, reason = "these are independent component deficits, not lifecycle state")]
pub struct Lacks {
    pub tools: bool,
    /// Some requested grant is not covered.
    pub grants: bool,
    pub executors: bool,
    /// Requested lifetime task capacity does not fit.
    pub tasks: bool,
    /// Requested descendant depth does not fit.
    pub depth: bool,
    /// Requested spend does not fit current available funding or authority.
    pub spend: bool,
    /// Requested deadline is later than permitted.
    pub deadline: bool,
    pub notes: bool,
}

impl Lacks {
    /// Pure query returning whether this bounded value has no entries or deficits; emits no output
    /// and allocates nothing.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !(self.tools
            || self.grants
            || self.executors
            || self.tasks
            || self.depth
            || self.spend
            || self.deadline
            || self.notes)
    }
}

/// Maximum deficits emitted by one fitting query: one for each of eight components.
pub const FITS_MAX_OUT: u32 = 8;

/// On one connector, granting `kind` also grants `implies`.
/// Root-configured connector-kind inclusion pair, validated as part of `Implies`.
/// (domain/authority.md, sections 4–5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Implication {
    pub connector: u16,
    pub kind: u16,
    /// Kind included by granting `kind` on this connector.
    pub implies: u16,
}

/// A connector-scoped preorder. Equality is implicit; all other transitive
/// consequences must be present. Cycles and duplicate entries are permitted.
/// The private table can only be made by validating bounded configuration.
/// Validated bounded connector-kind preorder; queries retain no connector state and emit no child
/// events. (domain/authority.md, sections 4–5).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Implies {
    pairs: Box<[Implication]>,
}

impl Implies {
    /// Refuses past the configured pair limit or if the table is not transitively closed.
    /// Validation scans at most `max_pairs` cubed; queries scan at most `max_pairs`, and neither
    /// allocates. Admit owned `pairs` under `max_pairs` and require explicit transitive closure;
    /// return `None` for excess or invalid configuration. Takes ownership without allocating
    /// additional storage.
    #[must_use]
    pub fn new(pairs: Box<[Implication]>, max_pairs: u32) -> Option<Implies> {
        if pairs.len() > usize::try_from(max_pairs).ok()? {
            return None;
        }
        let table = Implies { pairs };
        for first in &table.pairs {
            for second in &table.pairs {
                if first.connector == second.connector
                    && first.implies == second.kind
                    && !table.allows(first.connector, second.implies, first.kind)
                {
                    return None;
                }
            }
        }
        Some(table)
    }

    /// Whether a grant of `given` includes `needed` on this connector. Pure query: does granting
    /// `given` include `needed` on `connector`? Equality is implicit; scans at most the admitted
    /// pair count with no allocation or output.
    #[must_use]
    pub fn allows(&self, connector: u16, needed: u16, given: u16) -> bool {
        if needed == given {
            return true;
        }
        for pair in &self.pairs {
            if pair.connector == connector && pair.kind == given && pair.implies == needed {
                return true;
            }
        }
        false
    }

    /// Pure query of the constructor-bounded implication-pair count; duplicates count as retained
    /// entries.
    #[must_use]
    pub fn len(&self) -> u32 {
        u32::try_from(self.pairs.len()).expect("constructor bounds pairs by a u32")
    }

    /// Pure query returning whether this bounded value has no entries or deficits; emits no output
    /// and allocates nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }
}

/// Whether the pattern includes this name, compared segment by segment. Pure literal-segment
/// coverage query over caller-admitted `pattern` and `name`; exact/open terminals require an
/// additional segment; only open terminals include descendants. No allocation or child output.
#[must_use]
pub fn pattern_covers(pattern: &Pattern, name: &Name) -> bool {
    if name.segments.get(..pattern.segments.len()) != Some(pattern.segments.as_ref()) {
        return false;
    }
    match &pattern.last {
        Last::Exact(last) => {
            name.segments.len().checked_sub(1) == Some(pattern.segments.len())
                && name.segments.get(pattern.segments.len()) == Some(last)
        }
        Last::Open(last) => match name.segments.get(pattern.segments.len()) {
            Some(segment) => segment.starts_with(last),
            None => false,
        },
    }
}

/// Whether every name covered by `a` is covered by `b`, without enumerating names. Pure inclusion
/// query: every name covered by `a` must be covered by `b`; scans the caller- admitted owned
/// segments without allocation or child output.
#[must_use]
pub fn pattern_at_most(a: &Pattern, b: &Pattern) -> bool {
    match &b.last {
        Last::Exact(last) => match &a.last {
            Last::Exact(candidate) => a.segments == b.segments && candidate == last,
            Last::Open(_) => false,
        },
        Last::Open(last) => terminal_at_most(a, b, last),
    }
}

fn terminal_at_most(a: &Pattern, b: &Pattern, last: &[u8]) -> bool {
    if a.segments.get(..b.segments.len()) != Some(b.segments.as_ref()) {
        return false;
    }
    if let Some(segment) = a.segments.get(b.segments.len()) {
        return segment.starts_with(last);
    }
    match &a.last {
        Last::Exact(segment) | Last::Open(segment) => segment.starts_with(last),
    }
}

/// Every resource and kind of `a` must be included by this one grant `b`. Pure inclusion query of
/// `a` within one grant `b`, using the validated kind preorder; callers bound owned inputs. No
/// allocation or child output.
#[must_use]
pub fn grant_at_most(a: &Grant, b: &Grant, implies: &Implies) -> bool {
    a.connector == b.connector && implies.allows(a.connector, a.kind, b.kind) && pattern_at_most(&a.pattern, &b.pattern)
}

/// Whether a grant includes one named resource of the requested connector and kind. Pure query of
/// one `grant` covering the supplied connector, kind and literal `name`; callers bound the name and
/// grant. No allocation or child output.
#[must_use]
pub fn grant_covers(grant: &Grant, connector: u16, kind: u16, name: &Name, implies: &Implies) -> bool {
    grant.connector == connector && implies.allows(connector, kind, grant.kind) && pattern_covers(&grant.pattern, name)
}

/// Whether every part of `a` is at most `b`. Duplicate or reordered grants and executors need not
/// make equal values to make equivalent authority. Pure componentwise inclusion of `a` in `b`,
/// using admitted owned values and the validated kind preorder; compares permissions, not current
/// counters. No allocation or child output.
#[must_use]
pub fn at_most(a: &Authority, b: &Authority, implies: &Implies) -> bool {
    differences(a, b, b.delegation.tasks, b.delegation.depth, b.budget.spend, implies).is_empty()
}

/// Whether the child's authority fits, with spend and lifetime task capacity taken from separate
/// current inputs. The caller reserves `FITS_MAX_OUT` queue slots. A batch additionally consumes
/// one task slot for each immediate child and sums reservations. Pure fitting query over
/// caller-admitted child and creator values, current `numbers` and separate `tasks_left`; returns a
/// boolean and all deficits in `lacks`. Reserve `FITS_MAX_OUT` free slots; no mutation of inputs or
/// allocation.
#[must_use]
pub fn fits(
    child: &Authority,
    creator: &Authority,
    numbers: &Numbers,
    tasks_left: u32,
    implies: &Implies,
    lacks: &mut Queue<Lack>,
) -> bool {
    assert!(lacks.room() >= FITS_MAX_OUT, "caller reserves every fitting finding");
    let parts = fit_lacks(child, creator, numbers, tasks_left, implies);
    if parts.tools {
        lacks.push(Lack::Tools);
    }
    if parts.grants {
        lacks.push(Lack::Grants);
    }
    if parts.executors {
        lacks.push(Lack::Executors);
    }
    if parts.tasks {
        lacks.push(Lack::Tasks);
    }
    if parts.depth {
        lacks.push(Lack::Depth);
    }
    if parts.spend {
        lacks.push(Lack::Spend);
    }
    if parts.deadline {
        lacks.push(Lack::Deadline);
    }
    if parts.notes {
        lacks.push(Lack::Notes);
    }
    parts.is_empty()
}

pub(crate) fn fit_lacks(
    child: &Authority,
    creator: &Authority,
    numbers: &Numbers,
    tasks_left: u32,
    implies: &Implies,
) -> Lacks {
    let mut parts = differences(
        child,
        creator,
        tasks_left.min(creator.delegation.tasks),
        creator.delegation.depth.saturating_sub(1),
        left(*numbers).min(creator.budget.spend),
        implies,
    );
    if creator.delegation.depth == 0 {
        parts.depth = true;
    }
    if numbers.spent.checked_add(numbers.spent_below).is_none() {
        parts.spend = true;
    }
    parts
}

pub(crate) fn differences(
    a: &Authority,
    b: &Authority,
    tasks: u32,
    depth: u32,
    spend: u64,
    implies: &Implies,
) -> Lacks {
    let deadline_fits = match a.budget.deadline {
        Some(deadline) => match b.budget.deadline {
            Some(ceiling) => deadline <= ceiling,
            None => true,
        },
        None => b.budget.deadline.is_none(),
    };
    let mut lacks = Lacks {
        tools: a.tools.0 & !b.tools.0 != 0,
        grants: false,
        executors: false,
        tasks: a.delegation.tasks > tasks,
        depth: a.delegation.depth > depth,
        spend: a.budget.spend > spend,
        deadline: !deadline_fits,
        notes: a.notes.0 & !b.notes.0 != 0,
    };
    for kind in &a.delegation.kinds {
        if !b.delegation.kinds.contains(kind) {
            lacks.executors = true;
        }
    }
    for grant in &a.grants {
        let mut covered = false;
        for ceiling in &b.grants {
            if grant_at_most(grant, ceiling, implies) {
                covered = true;
                break;
            }
        }
        if !covered {
            lacks.grants = true;
        }
    }
    for scope in &a.note_resources {
        let mut covered = false;
        for ceiling in &b.note_resources {
            if scope.connector == ceiling.connector && pattern_at_most(&scope.pattern, &ceiling.pattern) {
                covered = true;
                break;
            }
        }
        if !covered {
            lacks.notes = true;
        }
    }
    lacks
}
