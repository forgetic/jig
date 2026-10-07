//! An independent finite statement of coverage, then the order's laws
//! (domain/authority.md, section 11; testing-strategy.md, step tests).

use alloc::boxed::Box;

use skein_lib::{List, Wall, bytes::copy_of};

use crate::{
    Authority, Budget, Delegation, Executor, Grant, Implication, Implies, Last, Name, Pattern, ResourceScope, Scopes,
    Tools, at_most, grant_at_most, grant_covers, pattern_at_most, pattern_covers,
};

const LETTERS: [&[u8]; 3] = [b"a", b"b", b"c"];

const SEGMENTS: [&[u8]; 7] = [b"", b"a", b"b", b"c", b"aa", b"ba", b"ca"];

const TERMINALS: [&[u8]; 4] = [b"", b"a", b"b", b"c"];

const WORDS: usize = 44; // ceil(2,801 names / 64).

fn segments(parts: &[&[u8]]) -> Box<[Box<[u8]>]> {
    let mut result = List::with_capacity(u32::try_from(parts.len()).unwrap());
    for part in parts {
        result.push(copy_of(part)).unwrap();
    }
    result.into_boxed()
}

fn pattern(parts: &[&[u8]], last: Last) -> Pattern {
    Pattern { segments: segments(parts), last }
}

fn path(mut code: u32, depth: u32, alphabet: &[&[u8]]) -> Box<[Box<[u8]>]> {
    let radix = u32::try_from(alphabet.len()).unwrap();
    let mut parts = List::with_capacity(depth);
    for _ in 0..depth {
        let digit = usize::try_from(code.checked_rem(radix).unwrap()).unwrap();
        parts.push(copy_of(alphabet[digit])).unwrap();
        code = code.checked_div(radix).unwrap();
    }
    parts.into_boxed()
}

fn names() -> Box<[Name]> {
    let mut names = List::with_capacity(2_801);
    for depth in 0..=4 {
        for code in 0..7_u32.pow(depth) {
            names.push(Name { segments: path(code, depth, &SEGMENTS) }).unwrap();
        }
    }
    names.into_boxed()
}

fn patterns() -> Box<[Pattern]> {
    let mut patterns = List::with_capacity(104);
    for depth in 0..=2 {
        for code in 0..3_u32.pow(depth) {
            let base = path(code, depth, &LETTERS);
            for terminal in TERMINALS {
                patterns.push(Pattern { segments: base.clone(), last: Last::Exact(copy_of(terminal)) }).unwrap();
                patterns.push(Pattern { segments: base.clone(), last: Last::Open(copy_of(terminal)) }).unwrap();
            }
        }
    }
    patterns.into_boxed()
}

/// A statement over complete names, deliberately independent of production's
/// slice-prefix and terminal inclusion algorithm.
fn naive_covers(pattern: &Pattern, name: &Name) -> bool {
    if name.segments.len() < pattern.segments.len() {
        return false;
    }
    for (position, segment) in pattern.segments.iter().enumerate() {
        if name.segments[position] != *segment {
            return false;
        }
    }
    match &pattern.last {
        Last::Exact(wanted) => {
            name.segments.len().checked_sub(1) == Some(pattern.segments.len())
                && name.segments.get(pattern.segments.len()) == Some(wanted)
        }
        Last::Open(wanted) => {
            if name.segments.len() <= pattern.segments.len() {
                return false;
            }
            let segment = &name.segments[pattern.segments.len()];
            if segment.len() < wanted.len() {
                return false;
            }
            for (position, byte) in wanted.iter().enumerate() {
                if segment[position] != *byte {
                    return false;
                }
            }
            true
        }
    }
}

fn covered_names(patterns: &[Pattern], names: &[Name]) -> Box<[[u64; WORDS]]> {
    let mut sets = List::with_capacity(u32::try_from(patterns.len()).unwrap());
    for pattern in patterns {
        let mut bits = [0_u64; WORDS];
        for (position, name) in names.iter().enumerate() {
            let covered = naive_covers(pattern, name);
            assert_eq!(pattern_covers(pattern, name), covered, "pattern={pattern:?}, name={name:?}");
            if covered {
                let word = position.checked_div(64).unwrap();
                let bit = position.checked_rem(64).unwrap();
                bits[word] |= 1_u64 << bit;
            }
        }
        sets.push(bits).unwrap();
    }
    sets.into_boxed()
}

fn subset(a: &[u64; WORDS], b: &[u64; WORDS]) -> bool {
    for (left, right) in a.iter().zip(b) {
        if left & !right != 0 {
            return false;
        }
    }
    true
}

#[test]
fn patterns_agree_with_finite_name_sets_and_obey_preorder_laws() {
    let patterns = patterns();
    let names = names();
    assert_eq!(patterns.len(), 104, "the fixed sample has both terminal forms");
    assert_eq!(names.len(), 2_801, "the universe has paths through four segments");
    let sets = covered_names(&patterns, &names);
    let mut relation = [[false; 104]; 104];
    for (i, a) in patterns.iter().enumerate() {
        for (j, b) in patterns.iter().enumerate() {
            let actual = pattern_at_most(a, b);
            assert_eq!(actual, subset(&sets[i], &sets[j]), "a={a:?}, b={b:?}");
            relation[i][j] = actual;
        }
        assert!(relation[i][i], "pattern order is reflexive");
    }
    for row in &relation {
        for (j, ab) in row.iter().enumerate() {
            if *ab {
                for (k, bc) in relation[j].iter().enumerate() {
                    assert!(!bc || row[k], "pattern order is transitive");
                }
            }
        }
    }
}

fn implications() -> Implies {
    Implies::new(
        Box::new([
            Implication { connector: 1, kind: 3, implies: 2 },
            Implication { connector: 1, kind: 2, implies: 1 },
            Implication { connector: 1, kind: 3, implies: 1 },
        ]),
        3,
    )
    .unwrap()
}

fn empty() -> Authority {
    Authority {
        tools: Tools(0),
        grants: Box::new([]),
        delegation: Delegation { kinds: Box::new([]), tasks: 0, depth: 0 },
        budget: Budget { spend: 0, deadline: Some(Wall::EPOCH) },
        notes: Scopes(0),
        note_resources: Box::new([]),
    }
}

#[test]
fn implication_tables_are_bounded_closed_and_connector_scoped() {
    let chain = [Implication { connector: 1, kind: 3, implies: 2 }, Implication { connector: 1, kind: 2, implies: 1 }];
    assert!(Implies::new(Box::new(chain), 2).is_none(), "an unclosed chain is refused");
    assert!(Implies::new(Box::new(chain), 1).is_none(), "too many pairs are refused");
    let table = implications();
    assert!(table.allows(1, 1, 3), "transitive consequence is included");
    assert!(!table.allows(1, 3, 1), "an implication is directed");
    assert!(!table.allows(2, 1, 3), "another connector has no implied grant");
    assert!(table.allows(99, 17, 17), "identity needs no explicit table entry");
    let cycle = Implies::new(
        Box::new([
            Implication { connector: 1, kind: 1, implies: 2 },
            Implication { connector: 1, kind: 2, implies: 1 },
            Implication { connector: 1, kind: 2, implies: 1 },
        ]),
        3,
    )
    .unwrap();
    assert!(cycle.allows(1, 2, 1) && cycle.allows(1, 1, 2), "cycles and duplicates form a preorder");
    let separate = Implies::new(
        Box::new([
            Implication { connector: 1, kind: 3, implies: 2 },
            Implication { connector: 2, kind: 2, implies: 1 },
        ]),
        2,
    );
    assert!(separate.is_some(), "cross-connector pairs do not form a chain");
    assert!(Implies::new(Box::new([]), 0).is_some(), "the zero limit accepts identity only");
}

#[test]
fn terminals_are_literal_and_grants_cover_only_their_connector_and_kinds() {
    let table = implications();
    let grant = Grant { connector: 1, kind: 3, pattern: pattern(&[b"object"], Last::Exact(copy_of(b"c42"))) };
    let name = Name { segments: segments(&[b"object", b"c42"]) };
    assert!(grant_covers(&grant, 1, 1, &name, &table), "exact terminal and implied kind");
    let descendant = Name { segments: segments(&[b"object", b"c42", b"child"]) };
    assert!(!grant_covers(&grant, 1, 1, &descendant, &table), "exact terminal excludes descendants");
    assert!(!grant_covers(&grant, 2, 1, &name, &table), "connector must agree");
    assert!(!grant_covers(&grant, 1, 4, &name, &table), "unrelated kinds do not follow");
    let sibling = Name { segments: segments(&[b"object", b"c420"]) };
    assert!(!grant_covers(&grant, 1, 3, &sibling, &table), "exact bytes exclude continuations");
    assert!(pattern_at_most(&grant.pattern, &grant.pattern), "one exact name fits itself");
    let open = pattern(&[b"object"], Last::Open(copy_of(b"r42-")));
    let run = Name { segments: segments(&[b"object", b"r42-1", b"saved"]) };
    assert!(pattern_covers(&open, &run), "open terminals include byte continuations and descendants");
    let literal = pattern(&[b"\xff/\0"], Last::Exact(copy_of(b"")));
    assert!(
        pattern_covers(&literal, &Name { segments: segments(&[b"\xff/\0", b""]) }),
        "non-UTF8, slash, nul and empty bytes are literal"
    );
    let prefixes = [
        pattern(&[b"object"], Last::Open(copy_of(b"a"))),
        pattern(&[b"object"], Last::Open(copy_of(b"aa"))),
        pattern(&[b"object"], Last::Exact(copy_of(b"aa"))),
        pattern(&[b"object", b"aa"], Last::Exact(copy_of(b"child"))),
    ];
    let witnesses = [
        Name { segments: segments(&[b"object", b"a"]) },
        Name { segments: segments(&[b"object", b"aa"]) },
        Name { segments: segments(&[b"object", b"aaa"]) },
        Name { segments: segments(&[b"object", b"ab"]) },
        Name { segments: segments(&[b"object", b"aa", b"child"]) },
    ];
    for left in &prefixes {
        for right in &prefixes {
            let mut included = true;
            for witness in &witnesses {
                if naive_covers(left, witness) && !naive_covers(right, witness) {
                    included = false;
                }
            }
            assert_eq!(pattern_at_most(left, right), included, "proper byte prefixes agree with name inclusion");
        }
    }
}

/// Independent grant order for the generated table: connector 1 has the
/// numeric chain 1 <= 2 <= 3, all other connectors have identity only.
fn naive_grant(a: &Grant, b: &Grant, patterns: &[Pattern], sets: &[[u64; WORDS]]) -> bool {
    if a.connector != b.connector || (a.kind != b.kind && (a.connector != 1 || a.kind > b.kind)) {
        return false;
    }
    let mut left = None;
    let mut right = None;
    for (position, pattern) in patterns.iter().enumerate() {
        if *pattern == a.pattern {
            left = Some(position);
        }
        if *pattern == b.pattern {
            right = Some(position);
        }
    }
    subset(&sets[left.unwrap()], &sets[right.unwrap()])
}

fn naive_authority(a: &Authority, b: &Authority, patterns: &[Pattern], sets: &[[u64; WORDS]]) -> bool {
    for bit in 0_u32..64 {
        let mask = 1_u64 << bit;
        if a.tools.0 & mask != 0 && b.tools.0 & mask == 0 {
            return false;
        }
    }
    for bit in 0_u32..8 {
        let mask = 1_u8 << bit;
        if a.notes.0 & mask != 0 && b.notes.0 & mask == 0 {
            return false;
        }
    }
    for executor in &a.delegation.kinds {
        let mut included = false;
        for ceiling in &b.delegation.kinds {
            if executor == ceiling {
                included = true;
            }
        }
        if !included {
            return false;
        }
    }
    let left_deadline = match a.budget.deadline {
        Some(time) => u128::from(time.as_nanos()),
        None => u128::MAX,
    };
    let right_deadline = match b.budget.deadline {
        Some(time) => u128::from(time.as_nanos()),
        None => u128::MAX,
    };
    if a.delegation.tasks > b.delegation.tasks
        || a.delegation.depth > b.delegation.depth
        || a.budget.spend > b.budget.spend
        || left_deadline > right_deadline
    {
        return false;
    }
    for grant in &a.grants {
        let mut included = false;
        for ceiling in &b.grants {
            if naive_grant(grant, ceiling, patterns, sets) {
                included = true;
            }
        }
        if !included {
            return false;
        }
    }
    for scope in &a.note_resources {
        let mut included = false;
        for ceiling in &b.note_resources {
            if scope.connector == ceiling.connector {
                let mut left = None;
                let mut right = None;
                for (position, pattern) in patterns.iter().enumerate() {
                    if *pattern == scope.pattern {
                        left = Some(position);
                    }
                    if *pattern == ceiling.pattern {
                        right = Some(position);
                    }
                }
                if subset(&sets[left.unwrap()], &sets[right.unwrap()]) {
                    included = true;
                }
            }
        }
        if !included {
            return false;
        }
    }
    true
}

fn authorities(patterns: &[Pattern]) -> Box<[Authority]> {
    let mut sample = List::with_capacity(48);
    for index in 0_u32..48 {
        let level = index.checked_rem(4).unwrap();
        let mut authority = empty();
        authority.tools = Tools((1_u64 << level).checked_sub(1).unwrap());
        authority.notes = Scopes(u8::try_from(authority.tools.0).unwrap());
        authority.delegation.tasks = level;
        authority.delegation.depth = level;
        authority.budget.spend = u64::from(level);
        authority.budget.deadline = if level == 3 { None } else { Some(Wall::from_nanos(u64::from(level))) };
        let mut kinds = List::with_capacity(level);
        for number in 0..level {
            let executor = match index.checked_rem(3).unwrap() {
                0 => Executor::Charter(number),
                1 => Executor::Procedure(number),
                _ => Executor::Role(number),
            };
            kinds.push(executor).unwrap();
        }
        authority.delegation.kinds = kinds.into_boxed();
        let mut grants = List::with_capacity(level);
        for number in 0..level {
            let slot = usize::try_from(index.checked_div(4).unwrap().checked_add(number).unwrap()).unwrap();
            grants
                .push(Grant {
                    connector: u16::try_from(index.checked_rem(2).unwrap().checked_add(1).unwrap()).unwrap(),
                    kind: u16::try_from(level).unwrap(),
                    pattern: patterns[slot].clone(),
                })
                .unwrap();
        }
        authority.grants = grants.into_boxed();
        let scope = usize::try_from(index.checked_div(4).unwrap()).unwrap();
        authority.note_resources = Box::new([ResourceScope {
            connector: u16::try_from(index.checked_rem(2).unwrap().checked_add(1).unwrap()).unwrap(),
            pattern: patterns[scope].clone(),
        }]);
        sample.push(authority).unwrap();
    }
    sample.into_boxed()
}

#[test]
fn authority_order_agrees_with_independent_components_and_is_a_preorder() {
    let patterns = patterns();
    let names = names();
    let sets = covered_names(&patterns, &names);
    let table = implications();
    let sample = authorities(&patterns);
    let mut relation = [[false; 48]; 48];
    let mut strict_chains = 0_u32;
    for (i, a) in sample.iter().enumerate() {
        for (j, b) in sample.iter().enumerate() {
            let actual = at_most(a, b, &table);
            assert_eq!(actual, naive_authority(a, b, &patterns, &sets), "a={a:?}, b={b:?}");
            relation[i][j] = actual;
            for left in &a.grants {
                for right in &b.grants {
                    assert_eq!(
                        grant_at_most(left, right, &table),
                        naive_grant(left, right, &patterns, &sets),
                        "individual grants agree with the same independent statement"
                    );
                }
            }
        }
        assert!(relation[i][i], "authority order is reflexive");
    }
    for (i, row) in relation.iter().enumerate() {
        for (j, ab) in row.iter().enumerate() {
            if *ab {
                for (k, bc) in relation[j].iter().enumerate() {
                    assert!(!bc || row[k], "authority order is transitive");
                    if *bc && !relation[j][i] && !relation[k][j] {
                        strict_chains = strict_chains.checked_add(1).unwrap();
                    }
                }
            }
        }
    }
    assert!(strict_chains > 0, "the sample exercises strict transitive chains");
}

#[test]
fn each_component_can_independently_prevent_the_order() {
    let table = implications();
    let ceiling = empty();
    let mut child = ceiling.clone();
    child.tools = Tools(1_u64 << 63);
    assert!(!at_most(&child, &ceiling, &table), "the highest tool bit needs a grant");
    child = ceiling.clone();
    child.notes = Scopes::DEPLOYMENT;
    assert!(!at_most(&child, &ceiling, &table), "notes need their scope");
    child = ceiling.clone();
    child.delegation.kinds = Box::new([Executor::Role(7)]);
    assert!(!at_most(&child, &ceiling, &table), "executors need membership");
    child = ceiling.clone();
    child.delegation.tasks = 1;
    assert!(!at_most(&child, &ceiling, &table), "task count cannot grow");
    child = ceiling.clone();
    child.delegation.depth = 1;
    assert!(!at_most(&child, &ceiling, &table), "depth cannot grow");
    child = ceiling.clone();
    child.budget.spend = u64::MAX;
    assert!(!at_most(&child, &ceiling, &table), "spend cannot grow");
    child = ceiling.clone();
    child.budget.deadline = Some(Wall::from_nanos(1));
    assert!(!at_most(&child, &ceiling, &table), "a deadline cannot move later");
    child.budget.deadline = None;
    assert!(!at_most(&child, &ceiling, &table), "no deadline is later than every finite deadline");
    let mut unlimited = ceiling.clone();
    unlimited.budget.deadline = None;
    child.budget.deadline = Some(Wall::from_nanos(u64::MAX));
    assert!(at_most(&child, &unlimited, &table), "the latest finite deadline precedes no deadline");
    child = ceiling.clone();
    child.grants = Box::new([Grant { connector: 1, kind: 1, pattern: pattern(&[], Last::Exact(copy_of(b"a"))) }]);
    assert!(!at_most(&child, &ceiling, &table), "every grant needs a containing grant");
}

#[test]
fn reordered_duplicate_values_are_equivalent_but_grant_unions_do_not_cover() {
    let table = implications();
    let mut a = empty();
    a.delegation.kinds = Box::new([Executor::Charter(1), Executor::Role(1)]);
    let first = Grant { connector: 1, kind: 1, pattern: pattern(&[], Last::Exact(copy_of(b"a"))) };
    let second = Grant { connector: 1, kind: 2, pattern: pattern(&[], Last::Exact(copy_of(b"b"))) };
    a.grants = Box::new([first.clone(), second.clone()]);
    let mut b = a.clone();
    b.delegation.kinds = Box::new([Executor::Role(1), Executor::Charter(1), Executor::Charter(1)]);
    b.grants = Box::new([second, first.clone(), first]);
    assert!(a != b && at_most(&a, &b, &table) && at_most(&b, &a, &table), "the order is a preorder");
    a.grants = Box::new([Grant { connector: 1, kind: 1, pattern: pattern(&[], Last::Open(copy_of(b"a"))) }]);
    let mut partition = List::with_capacity(257);
    partition.push(Grant { connector: 1, kind: 1, pattern: pattern(&[], Last::Exact(copy_of(b"a"))) }).unwrap();
    for byte in 0..=u8::MAX {
        partition
            .push(Grant { connector: 1, kind: 1, pattern: pattern(&[], Last::Open(copy_of(&[b'a', byte]))) })
            .unwrap();
    }
    b.grants = partition.into_boxed();
    assert!(
        !at_most(&a, &b, &table),
        "even a complete byte partition is not the one containing grant the order requires"
    );
}
