//! Pure translations between Smith's composed domain and its host-facing
//! boundary (domain/hosts.md, sections 5.2 and 8). Only charter and saved turns
//! cross as bytes; callback names and settled results remain typed.

use alloc::boxed::Box;
use skein_lib::{List, ReplyTo, Token};
use smith_domain as smith;
use smith_host_domain as host;
use smith_protocol_channel as protocol;

use crate::limits::Limits;

fn count(length: usize) -> Option<u32> {
    u32::try_from(length).ok()
}

/// Decode the agent start the process capability would have received.
pub(crate) fn start(
    client: Token,
    source: host::Start,
    limits: &Limits,
    endpoints: &protocol::Endpoints,
) -> Option<smith::Event> {
    if source.workspace.is_some() || !source.directories.is_empty() {
        return None;
    }
    let charter = protocol::decode_charter(&source.charter, &limits.charter, endpoints).ok()?;
    let transcript = match source.transcript {
        Some(turns) => protocol::decode_transcript(&turns, &limits.transcript, endpoints).ok()?,
        None => None,
    };
    let mut answered = List::with_capacity(count(source.answered.len())?);
    for call in source.answered {
        let value = match call.reply {
            host::SavedReply::Host { error, body } => smith::Answered::Host(smith::run::HostAnswer::new(body, error)?),
            host::SavedReply::Delivery(delivery) => smith::Answered::Delivery(Box::new(delivery_value(*delivery)?)),
            host::SavedReply::TooLarge => smith::Answered::TooLarge,
        };
        answered
            .push(smith::AnsweredCall {
                name: smith::run::CallName {
                    activation: call.name.activation,
                    completion: call.name.completion,
                    position: call.name.position,
                },
                tool: call.tool,
                answer: value,
            })
            .ok()?;
    }
    let mut grants = List::with_capacity(count(source.grants.len())?);
    for grant in source.grants {
        grants
            .push(smith::Grant {
                name: smith::GrantName { account: grant.account, generation: grant.generation },
                valid: grant.valid,
            })
            .ok()?;
    }
    Some(smith::Event::Start {
        reply_to: ReplyTo::new(client),
        host_run: source.logical_run,
        activation: source.activation,
        window: limits.window,
        charter,
        workspace: None,
        transcript,
        answered: answered.into_boxed(),
        grants: grants.into_boxed(),
    })
}

/// Translate a settled host delivery while preserving every receipt and detail.
pub(crate) fn delivery_value(source: host::Delivery) -> Option<smith::run::Delivery> {
    match source {
        host::Delivery::Delivered(delivered) => {
            let mut receipts = List::with_capacity(count(delivered.receipts().len())?);
            for receipt in delivered.receipts() {
                receipts.push(smith::run::Receipt::new(receipt.directory(), Box::from(receipt.text()))?).ok()?;
            }
            Some(smith::run::Delivery::Delivered(smith::run::Delivered::new(receipts.into_boxed())?))
        }
        host::Delivery::Nothing => Some(smith::run::Delivery::Nothing),
        host::Delivery::Refused(refused) => {
            let marker = match refused.marker() {
                Some(marker) => Some(smith::run::Marker::new(marker.directory(), Box::from(marker.path()))?),
                None => None,
            };
            Some(smith::run::Delivery::Refused(smith::run::DeliveryRefusal::new(
                marker,
                Box::from(refused.explanation()),
            )?))
        }
        host::Delivery::Failed(failed) => {
            let reason = match failed.reason {
                host::DeliveryReason::Unreachable => smith::run::DeliveryReason::Unreachable,
                host::DeliveryReason::RefusedByTarget => smith::run::DeliveryReason::RefusedByTarget,
                host::DeliveryReason::TimedOut => smith::run::DeliveryReason::TimedOut,
                host::DeliveryReason::Broken => smith::run::DeliveryReason::Broken,
                host::DeliveryReason::TooLarge => smith::run::DeliveryReason::TooLarge,
                host::DeliveryReason::Missing => smith::run::DeliveryReason::Missing,
                host::DeliveryReason::Busy => smith::run::DeliveryReason::Busy,
                host::DeliveryReason::Unavailable => smith::run::DeliveryReason::Unavailable,
                host::DeliveryReason::Cancelled => smith::run::DeliveryReason::Cancelled,
                host::DeliveryReason::Unknown => smith::run::DeliveryReason::Unknown,
            };
            Some(smith::run::Delivery::Failed(smith::run::DeliveryFailure {
                directory: failed.directory,
                reason,
                diagnostic: smith::run::Diagnostic::new(failed.diagnostic.output(), failed.diagnostic.cut()),
            }))
        }
        host::Delivery::Stale => Some(smith::run::Delivery::Stale),
    }
}

/// Return a host tool's terminal to the composed run.
pub(crate) fn host_reply(source: host::Reply) -> Option<smith::run::HostReply> {
    match source {
        host::Reply::Host { error, body } => {
            Some(smith::run::HostReply::Answered(smith::run::HostAnswer::new(body, error)?))
        }
        host::Reply::Busy => Some(smith::run::HostReply::Busy),
        host::Reply::Unavailable => Some(smith::run::HostReply::Unanswered(smith::run::Unanswered::Lost)),
        host::Reply::Withdrawn => Some(smith::run::HostReply::Withdrawn),
        host::Reply::TooLarge => Some(smith::run::HostReply::TooLarge),
        host::Reply::Delivery(_) => None,
    }
}

fn invalid(source: smith::run::Invalid) -> host::RunInvalid {
    match source {
        smith::run::Invalid::CharterVersion => host::RunInvalid::CharterVersion,
        smith::run::Invalid::MalformedCharter => host::RunInvalid::MalformedCharter,
        smith::run::Invalid::Endpoint => host::RunInvalid::Endpoint,
        smith::run::Invalid::Activation => host::RunInvalid::Activation,
        smith::run::Invalid::Window => host::RunInvalid::Window,
        smith::run::Invalid::Conventions => host::RunInvalid::Conventions,
        smith::run::Invalid::TooLarge => host::RunInvalid::TooLarge,
        smith::run::Invalid::Workspace => host::RunInvalid::Workspace,
        smith::run::Invalid::Grants => host::RunInvalid::Grants,
        smith::run::Invalid::Outcome => host::RunInvalid::Outcome,
        smith::run::Invalid::Budget => host::RunInvalid::Budget,
        smith::run::Invalid::Llm => host::RunInvalid::Llm,
        smith::run::Invalid::Conversation => host::RunInvalid::Conversation,
    }
}

fn model_failure(source: smith::run::Fault) -> host::ModelFault {
    match source {
        smith::run::Fault::Completion { failure, evidence } => {
            let failure = match failure {
                smith::run::CompletionFailure::Limit => host::CompletionFailure::Limit,
                smith::run::CompletionFailure::Protocol => host::CompletionFailure::Protocol,
                smith::run::CompletionFailure::Cancelled => host::CompletionFailure::Cancelled,
                smith::run::CompletionFailure::Overloaded => host::CompletionFailure::Overloaded,
                smith::run::CompletionFailure::Unavailable => host::CompletionFailure::Unavailable,
                smith::run::CompletionFailure::TimedOut => host::CompletionFailure::TimedOut,
                smith::run::CompletionFailure::ContextTooLong => host::CompletionFailure::ContextTooLong,
                smith::run::CompletionFailure::Invalid => host::CompletionFailure::Invalid,
                smith::run::CompletionFailure::Unauthorized => host::CompletionFailure::Unauthorized,
                smith::run::CompletionFailure::RateLimited { retry_after } => {
                    host::CompletionFailure::RateLimited { retry_after }
                }
                smith::run::CompletionFailure::Exhausted { retry_after } => {
                    host::CompletionFailure::Exhausted { retry_after }
                }
            };
            let evidence = match evidence {
                smith::run::CompletionEvidence::Unsent => host::CompletionEvidence::Unsent,
                smith::run::CompletionEvidence::Unknown => host::CompletionEvidence::Unknown,
                smith::run::CompletionEvidence::Response => host::CompletionEvidence::Response,
            };
            host::ModelFault::Completion { failure, evidence }
        }
        smith::run::Fault::Exhausted => host::ModelFault::Exhausted,
        smith::run::Fault::Provider => host::ModelFault::Provider,
        smith::run::Fault::ContextFull => host::ModelFault::ContextFull,
        smith::run::Fault::Refused => host::ModelFault::Refused,
        smith::run::Fault::Truncated => host::ModelFault::Truncated,
        smith::run::Fault::Malformed => host::ModelFault::Malformed,
    }
}

fn budget_failure(source: smith::run::Exhausted) -> host::Exhausted {
    match source {
        smith::run::Exhausted::Turns => host::Exhausted::Turns,
        smith::run::Exhausted::Spend => host::Exhausted::Spend,
        smith::run::Exhausted::Time => host::Exhausted::Time,
        smith::run::Exhausted::Tokens(limit) => host::Exhausted::Tokens(match limit {
            smith::run::ReceivingLimit::Input => host::ReceivingLimit::Input,
            smith::run::ReceivingLimit::Output => host::ReceivingLimit::Output,
            smith::run::ReceivingLimit::CacheRead => host::ReceivingLimit::CacheRead,
            smith::run::ReceivingLimit::CacheWrite => host::ReceivingLimit::CacheWrite,
        }),
        smith::run::Exhausted::Overflow(overflow) => host::Exhausted::Overflow(match overflow {
            smith::run::Overflow::Spend => host::Overflow::Spend,
            smith::run::Overflow::Usage => host::Overflow::Usage,
        }),
    }
}

fn failure(source: smith::run::Failure) -> host::RunFailure {
    match source {
        smith::run::Failure::Transcript(refusal) => host::RunFailure::Transcript(match refusal {
            smith::run::TranscriptRefusal::Version => host::TranscriptRefusal::Version,
            smith::run::TranscriptRefusal::Endpoint => host::TranscriptRefusal::Endpoint,
            smith::run::TranscriptRefusal::Dialect => host::TranscriptRefusal::Dialect,
            smith::run::TranscriptRefusal::Malformed => host::TranscriptRefusal::Malformed,
            smith::run::TranscriptRefusal::Unresolved => host::TranscriptRefusal::Unresolved,
            smith::run::TranscriptRefusal::TooLarge => host::TranscriptRefusal::TooLarge,
        }),
        smith::run::Failure::Model(fault) => host::RunFailure::Model(model_failure(fault)),
        smith::run::Failure::Budget(exhausted) => host::RunFailure::Budget(budget_failure(exhausted)),
        smith::run::Failure::Policy(smith::run::Policy::Unfinished { nudges, rejected }) => {
            host::RunFailure::Policy(host::Policy::Unfinished { nudges, rejected })
        }
        smith::run::Failure::Cancelled => host::RunFailure::Cancelled,
        smith::run::Failure::Stale => host::RunFailure::Stale,
    }
}

/// Encode the last word once, keeping accepted result bytes opaque to the hub.
pub(crate) fn answer(source: smith::run::Answer, charter: &smith_charter::v1::Limits) -> Option<host::Answer> {
    let (turns, spent, result) = match source {
        smith::run::Answer::Refused(reason) => {
            let refusal = match reason {
                smith::run::Refusal::Busy => host::Refusal::Busy,
                smith::run::Refusal::Invalid(reason) => host::Refusal::Invalid(invalid(reason)),
            };
            (0, 0, host::RunResult::Refused { refusal })
        }
        smith::run::Answer::Parked { turns, spent } => (turns, spent.units, host::RunResult::Parked),
        smith::run::Answer::Accepted { outcome, turns, spent } => {
            let outcome = protocol::encode_result(&outcome, charter).ok()?;
            (turns, spent.units, host::RunResult::Accepted { outcome })
        }
        smith::run::Answer::Failed { failure: why, turns, spent } => {
            (turns, spent.units, host::RunResult::Failed { failure: failure(why) })
        }
    };
    Some(host::Answer { turns, spent, result })
}
