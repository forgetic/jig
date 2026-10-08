//! The core's bounded task, dependency, attempt and transcript sections
//! (domain/engine.md, section 9).

use alloc::boxed::Box;
use jig_core_notes as notes;
use jig_core_tasks as tasks;
use skein_lib::{Decimal, List, Writer};

use crate::{Core, HistoricalResult, Transcript};

/// Limits for rendering one core-owned brief section.
#[derive(Clone, Copy, Debug)]
pub struct BriefTextLimits {
    pub parts: u32,
    pub read_bytes: u32,
    pub brief_bytes: u32,
}

/// A root-owned part of the task context rendered for a brief.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BriefPart {
    Spec,
    Dependencies,
    Delegates,
    Attempts,
    TranscriptTail,
}

/// Render the bounded note index by durable name so a run can recall entries.
#[must_use]
pub fn note_index_text(lines: &List<notes::Line>, more: u32, budget: u32) -> Box<[u8]> {
    let limit = usize::try_from(budget).expect("brief byte bound fits usize");
    let mut kept = 0_u32;
    let mut length = 0_usize;
    for line in lines {
        let name = Decimal::of(line.name);
        let revision = Decimal::of(u64::from(line.revision));
        let bytes = name
            .as_bytes()
            .len()
            .saturating_add(revision.as_bytes().len())
            .saturating_add(line.description.len())
            .saturating_add(b" r: \n".len());
        if length.saturating_add(bytes).saturating_add(32) > limit {
            break;
        }
        length = length.saturating_add(bytes);
        kept = kept.checked_add(1).expect("bounded note lines");
    }
    let omitted = u64::from(more).saturating_add(u64::from(lines.len().saturating_sub(kept)));
    let marker = Decimal::of(omitted);
    let marker_length = b"[more: ".len().saturating_add(marker.as_bytes().len()).saturating_add(b"]\n".len());
    let show_more = omitted > 0 && length.saturating_add(marker_length) <= limit;
    if show_more {
        length = length.saturating_add(marker_length);
    }
    let show_empty = kept == 0 && omitted == 0 && length.saturating_add(b"No notes\n".len()) <= limit;
    if show_empty {
        length = length.saturating_add(b"No notes\n".len());
    }
    let mut writer = Writer::new(length);
    let mut at = 0_u32;
    for line in lines {
        if at >= kept {
            break;
        }
        let name = Decimal::of(line.name);
        let revision = Decimal::of(u64::from(line.revision));
        writer.put(name.as_bytes()).expect("measured note name");
        writer.put(b" r").expect("measured revision label");
        writer.put(revision.as_bytes()).expect("measured revision");
        writer.put(b": ").expect("measured separator");
        writer.put(&line.description).expect("measured description");
        writer.put(b"\n").expect("measured newline");
        at = at.checked_add(1).expect("bounded note lines");
    }
    if show_more {
        writer.put(b"[more: ").expect("measured marker");
        writer.put(marker.as_bytes()).expect("measured omitted count");
        writer.put(b"]\n").expect("measured marker end");
    } else if show_empty {
        writer.put(b"No notes\n").expect("measured empty index");
    }
    writer.finish()
}

/// A bounded fragment and the amount its root renderer omitted.
#[derive(Debug)]
struct BriefFragment {
    bytes: Box<[u8]>,
    left: u64,
}

/// The result of reading a root-owned part of a task.
enum BriefRead {
    Got(Box<[BriefFragment]>),
    Failed,
}

#[must_use]
pub fn read_brief_part(core: &Core, limits: BriefTextLimits, task: u64, part: BriefPart) -> Option<Box<[u8]>> {
    let tail = match part {
        BriefPart::TranscriptTail => true,
        BriefPart::Spec | BriefPart::Dependencies | BriefPart::Delegates | BriefPart::Attempts => false,
    };
    let read = task_section(core, task, part, limits.parts, limits.read_bytes);
    let parts = match read {
        BriefRead::Got(parts) => parts,
        BriefRead::Failed => return None,
    };
    let mut length = 0_usize;
    let mut ended_line = true;
    for part in &parts {
        length = length.checked_add(part.bytes.len())?;
        if let Some(last) = part.bytes.last() {
            ended_line = *last == b'\n';
        }
        if part.left > 0 {
            length = length
                .checked_add(usize::from(!tail && !ended_line))?
                .checked_add(1)?
                .checked_add(Decimal::of(part.left).as_bytes().len())?
                .checked_add(b" bytes cut]\n".len())?;
            ended_line = true;
        }
    }
    if length > usize::try_from(limits.brief_bytes).ok()? {
        return None;
    }
    let mut writer = Writer::new(length);
    let mut ended_line = true;
    for part in parts {
        if tail && part.left > 0 {
            writer.put(b"[").expect("measured cut marker");
            writer.put(Decimal::of(part.left).as_bytes()).expect("measured lost count");
            writer.put(b" bytes cut]\n").expect("measured cut marker");
        }
        writer.put(&part.bytes).expect("measured part");
        if let Some(last) = part.bytes.last() {
            ended_line = *last == b'\n';
        }
        if !tail && part.left > 0 {
            if !ended_line {
                writer.put(b"\n").expect("measured break");
            }
            writer.put(b"[").expect("measured cut marker");
            writer.put(Decimal::of(part.left).as_bytes()).expect("measured lost count");
            writer.put(b" bytes cut]\n").expect("measured cut marker");
            ended_line = true;
        }
    }
    Some(writer.finish())
}

fn text_part(first: &[u8], second: &[u8], third: &[u8], fourth: &[u8], available: u32) -> BriefFragment {
    let total = first
        .len()
        .checked_add(second.len())
        .expect("bounded part bytes")
        .checked_add(third.len())
        .expect("bounded part bytes")
        .checked_add(fourth.len())
        .expect("bounded part bytes");
    let wanted = total.min(usize::try_from(available).expect("u32 fits usize"));
    let mut keep = 0_usize;
    for source in [first, second, third, fourth] {
        let demand = wanted.checked_sub(keep).expect("prefix within demand");
        let length = prefix(source, demand).len();
        keep = keep.checked_add(length).expect("bounded part prefix");
        if length < source.len() {
            break;
        }
    }
    let mut text = Writer::new(keep);
    for source in [first, second, third, fourth] {
        let bytes = prefix(source, text.room());
        text.put(bytes).expect("measured exact text part");
        if bytes.len() < source.len() {
            break;
        }
    }
    BriefFragment {
        bytes: text.finish(),
        left: u64::try_from(total.checked_sub(keep).expect("prefix in part")).expect("usize fits u64"),
    }
}

fn prefix(bytes: &[u8], most: usize) -> &[u8] {
    let mut end = most.min(bytes.len());
    for _ in 0_u32..3 {
        if let Some(byte) = bytes.get(end)
            && byte & 0b1100_0000 == 0b1000_0000
        {
            if end == 0 {
                break;
            }
            end = end.checked_sub(1).expect("positive prefix end");
        }
    }
    bytes.get(..end).expect("UTF-8 prefix within source")
}

fn task_section(core: &Core, task: u64, part: BriefPart, parts: u32, bytes: u32) -> BriefRead {
    if parts == 0 {
        return BriefRead::Failed;
    }
    match part {
        BriefPart::Spec => match core.contexts.get(&task) {
            Some(context) => task_read(context, parts, bytes),
            None => BriefRead::Failed,
        },
        BriefPart::Delegates => match core.contexts.get(&task) {
            Some(context) => delegates_read(&context.delegates, bytes),
            None => BriefRead::Failed,
        },
        BriefPart::Dependencies => match core.dependency_results.get(&task) {
            Some(results) => dependency_read(results, bytes),
            None => BriefRead::Failed,
        },
        BriefPart::Attempts => match core.contexts.get(&task) {
            Some(context) => attempt_read(context.tries, context.invalid_result, bytes),
            None => BriefRead::Failed,
        },
        BriefPart::TranscriptTail => match core.transcripts.get(&task) {
            Some(transcript) => tail_read(transcript, bytes),
            None => BriefRead::Failed,
        },
    }
}

fn attempt_read(tries: tasks::Tries, invalid: Option<tasks::InvalidResult>, bytes: u32) -> BriefRead {
    let classes: [(&[u8], u32); 6] = [
        (b"transient: ", tries.transient),
        (b"permanent: ", tries.permanent),
        (b"run: ", tries.run),
        (b"agent: ", tries.agent),
        (b"lost: ", tries.lost),
        (b"invalid: ", tries.invalid),
    ];
    let mut total = 0_usize;
    for (name, count) in classes {
        if count > 0 {
            total = total
                .checked_add(name.len())
                .expect("bounded attempt label")
                .checked_add(Decimal::of(u64::from(count)).as_bytes().len())
                .expect("bounded count")
                .checked_add(1)
                .expect("newline");
        }
    }
    let reason = match invalid {
        Some(tasks::InvalidResult::Form) => b"last invalid result: contract form\n".as_slice(),
        Some(tasks::InvalidResult::Verdict) => b"last invalid result: verdict code\n",
        Some(tasks::InvalidResult::Words) => b"last invalid result: word limit\n",
        Some(tasks::InvalidResult::Change) => b"last invalid result: change identity\n",
        Some(tasks::InvalidResult::Followups) => b"last invalid result: follow-up limit\n",
        None => b"",
    };
    total = total.checked_add(reason.len()).expect("bounded invalid reason");
    let mut writer = Writer::new(total.min(usize::try_from(bytes).expect("u32 fits usize")));
    for (name, count) in classes {
        if count > 0 {
            for fragment in [name, Decimal::of(u64::from(count)).as_bytes(), b"\n"] {
                let kept = prefix(fragment, writer.room());
                writer.put(kept).expect("attempt prefix fits");
            }
        }
    }
    let kept = prefix(reason, writer.room());
    writer.put(kept).expect("invalid reason prefix fits");
    let text = writer.finish();
    BriefRead::Got(Box::new([BriefFragment {
        left: u64::try_from(total.checked_sub(text.len()).expect("written prefix")).expect("usize fits u64"),
        bytes: text,
    }]))
}

fn delegate_phase(phase: &tasks::Phase) -> &'static [u8] {
    match phase {
        tasks::Phase::Waiting => b"waiting",
        tasks::Phase::Active(active) => match active {
            tasks::Active::Idle => b"idle",
            tasks::Active::Due | tasks::Active::Preparing => b"due",
            tasks::Active::Claimed { .. } | tasks::Active::Running { .. } => b"running",
            tasks::Active::BackingOff { .. } => b"backing off",
        },
        tasks::Phase::Closing(_) => b"closing",
        tasks::Phase::Held { .. } => b"held",
        tasks::Phase::Ended(_) => b"ended",
    }
}

fn delegates_read(delegates: &[tasks::DelegateState], bytes: u32) -> BriefRead {
    let mut total = 0_usize;
    for delegate in delegates {
        total = total
            .checked_add(b"delegate ".len())
            .expect("bounded delegate text")
            .checked_add(Decimal::of(delegate.task).as_bytes().len())
            .expect("bounded delegate ID")
            .checked_add(b": ".len())
            .expect("bounded delegate text")
            .checked_add(delegate_phase(&delegate.phase).len())
            .expect("bounded delegate phase")
            .checked_add(1)
            .expect("delegate newline");
    }
    let mut writer = Writer::new(total);
    for delegate in delegates {
        writer.put(b"delegate ").expect("measured delegate text");
        writer.put(Decimal::of(delegate.task).as_bytes()).expect("measured delegate ID");
        writer.put(b": ").expect("measured delegate text");
        writer.put(delegate_phase(&delegate.phase)).expect("measured delegate phase");
        writer.put(b"\n").expect("measured delegate newline");
    }
    let text = writer.finish();
    BriefRead::Got(Box::new([text_part(&text, b"", b"", b"", bytes)]))
}

fn result_label(kind: tasks::ResultKind) -> &'static [u8] {
    match kind {
        tasks::ResultKind::Report => b"report",
        tasks::ResultKind::Verdict { .. } => b"verdict",
        tasks::ResultKind::Change { .. } => b"change",
        tasks::ResultKind::Failed => b"failed",
        tasks::ResultKind::Cancelled => b"cancelled",
    }
}

fn dependency_read(results: &[HistoricalResult], bytes: u32) -> BriefRead {
    let mut total = 0_usize;
    for result in results {
        total = total
            .checked_add(b"task ".len())
            .expect("bounded result text")
            .checked_add(Decimal::of(result.task).as_bytes().len())
            .expect("bounded result ID")
            .checked_add(b": ".len())
            .expect("bounded result text")
            .checked_add(result_label(result.kind).len())
            .expect("bounded result kind")
            .checked_add(result.words.len())
            .expect("bounded result words")
            .checked_add(3)
            .expect("result separators");
        match result.kind {
            tasks::ResultKind::Verdict { code } => {
                total = total
                    .checked_add(Decimal::of(u64::from(code)).as_bytes().len())
                    .expect("bounded verdict code")
                    .checked_add(1)
                    .expect("verdict space");
            }
            tasks::ResultKind::Report
            | tasks::ResultKind::Change { .. }
            | tasks::ResultKind::Failed
            | tasks::ResultKind::Cancelled => {}
        }
    }
    let mut writer = Writer::new(total);
    for result in results {
        writer.put(b"task ").expect("measured result text");
        writer.put(Decimal::of(result.task).as_bytes()).expect("measured result ID");
        writer.put(b": ").expect("measured result text");
        writer.put(result_label(result.kind)).expect("measured result kind");
        match result.kind {
            tasks::ResultKind::Verdict { code } => {
                writer.put(b" ").expect("measured verdict space");
                writer.put(Decimal::of(u64::from(code)).as_bytes()).expect("measured verdict code");
            }
            tasks::ResultKind::Report
            | tasks::ResultKind::Change { .. }
            | tasks::ResultKind::Failed
            | tasks::ResultKind::Cancelled => {}
        }
        writer.put(b"\n").expect("measured result newline");
        writer.put(&result.words).expect("measured result words");
        writer.put(b"\n\n").expect("measured result separator");
    }
    let text = writer.finish();
    BriefRead::Got(Box::new([text_part(&text, b"", b"", b"", bytes)]))
}

fn tail_read(transcript: &Transcript, bytes: u32) -> BriefRead {
    let kept = u64::from(bytes).min(transcript.kept);
    let skip = transcript.kept.checked_sub(kept).expect("tail within kept bytes");
    let mut writer = Writer::new(usize::try_from(kept).expect("u32 bound fits usize"));
    let mut passed = 0_u64;
    for turn in &transcript.turns {
        let end = passed.checked_add(u64::try_from(turn.len()).expect("bounded turn")).expect("bounded retained tail");
        if end > skip {
            let from = usize::try_from(skip.saturating_sub(passed)).expect("bounded offset");
            writer.put(turn.get(from..).expect("tail starts inside retained turn")).expect("tail fits chosen budget");
        }
        passed = end;
    }
    let text = writer.finish();
    BriefRead::Got(Box::new([BriefFragment {
        left: transcript.bytes.saturating_sub(u64::try_from(text.len()).expect("bounded tail")),
        bytes: text,
    }]))
}

fn add_text_len(total: &mut usize, fragment: &[u8]) {
    *total = total.checked_add(fragment.len()).expect("bounded brief text");
}

fn contract_text(contract: &tasks::Contract) -> Box<[u8]> {
    let mut room = 0_usize;
    match contract {
        tasks::Contract::Report { words } => {
            for fragment in [b"[Report: at most ".as_slice(), Decimal::of(u64::from(*words)).as_bytes(), b" bytes]\n"] {
                add_text_len(&mut room, fragment);
            }
        }
        tasks::Contract::Verdict { choices } => {
            add_text_len(&mut room, b"[Verdict choices:\n");
            for choice in choices {
                for fragment in [
                    b"  ".as_slice(),
                    Decimal::of(u64::from(choice.code)).as_bytes(),
                    b": at most ",
                    Decimal::of(u64::from(choice.words)).as_bytes(),
                    b" bytes\n",
                ] {
                    add_text_len(&mut room, fragment);
                }
            }
            add_text_len(&mut room, b"]\n");
        }
        tasks::Contract::Change { connector, kind, words } => {
            for fragment in [
                b"[Change connector ".as_slice(),
                Decimal::of(u64::from(*connector)).as_bytes(),
                b", kind ",
                Decimal::of(u64::from(*kind)).as_bytes(),
                b", at most ",
                Decimal::of(u64::from(*words)).as_bytes(),
                b" bytes]\n",
            ] {
                add_text_len(&mut room, fragment);
            }
        }
    }
    let mut writer = Writer::new(room);
    match contract {
        tasks::Contract::Report { words } => {
            writer.put(b"[Report: at most ").expect("contract text room");
            writer.put(Decimal::of(u64::from(*words)).as_bytes()).expect("contract text room");
            writer.put(b" bytes]\n").expect("contract text room");
        }
        tasks::Contract::Verdict { choices } => {
            writer.put(b"[Verdict choices:\n").expect("contract text room");
            for choice in choices {
                writer.put(b"  ").expect("contract text room");
                writer.put(Decimal::of(u64::from(choice.code)).as_bytes()).expect("contract text room");
                writer.put(b": at most ").expect("contract text room");
                writer.put(Decimal::of(u64::from(choice.words)).as_bytes()).expect("contract text room");
                writer.put(b" bytes\n").expect("contract text room");
            }
            writer.put(b"]\n").expect("contract text room");
        }
        tasks::Contract::Change { connector, kind, words } => {
            writer.put(b"[Change connector ").expect("contract text room");
            writer.put(Decimal::of(u64::from(*connector)).as_bytes()).expect("contract text room");
            writer.put(b", kind ").expect("contract text room");
            writer.put(Decimal::of(u64::from(*kind)).as_bytes()).expect("contract text room");
            writer.put(b", at most ").expect("contract text room");
            writer.put(Decimal::of(u64::from(*words)).as_bytes()).expect("contract text room");
            writer.put(b" bytes]\n").expect("contract text room");
        }
    }
    writer.finish()
}

/// Render an agent task's spec and typed contract from its activation snapshot.
fn task_read(record: &tasks::RunContext, parts: u32, bytes: u32) -> BriefRead {
    if parts == 0 {
        return BriefRead::Failed;
    }
    let contract = contract_text(&record.contract);
    let first = text_part(&record.spec.words, b"\n", &contract, b"", bytes);
    let remaining =
        bytes.checked_sub(u32::try_from(first.bytes.len()).expect("bounded task part")).expect("part within read");
    let part = match record.requester {
        tasks::Party::Person(person) => {
            let person = Decimal::of(person);
            text_part(b"[Requested by person ", person.as_bytes(), b"]\n", b"", remaining)
        }
        tasks::Party::Task(task) => {
            let task = Decimal::of(task);
            text_part(b"[Requested by task ", task.as_bytes(), b"]\n", b"", remaining)
        }
        tasks::Party::Deployment { project } => {
            let project = Decimal::of(u64::from(project));
            text_part(b"[Requested by deployment for project ", project.as_bytes(), b"]\n", b"", remaining)
        }
    };
    let mut gathered = List::with_capacity(parts);
    gathered.push(first).expect("positive part room");
    if parts > 1 {
        gathered.push(part).expect("requester part room");
    } else {
        let last = gathered.get_mut(0).expect("first part");
        last.left = last
            .left
            .checked_add(u64::try_from(part.bytes.len()).expect("usize fits u64"))
            .expect("bounded omitted bytes")
            .checked_add(part.left)
            .expect("bounded omitted bytes");
    }
    BriefRead::Got(gathered.into_boxed())
}
