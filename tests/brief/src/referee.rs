//! Observations of the brief's requests, never its private state.

use jig_core_brief::{GatherPlaced, GatherRequest};

/// Every completed brief stays inside the admitted byte budget.
#[must_use]
pub fn within_budget(requests: &[GatherRequest], budget: u32) -> bool {
    for request in requests {
        if let GatherRequest::Complete { order, .. } = request {
            let mut total = 0_u32;
            for section in order {
                let size = match section {
                    GatherPlaced::Core { text, .. } => u32::try_from(text.len()).expect("bounded core text"),
                    GatherPlaced::Connector { size, .. } => *size,
                    GatherPlaced::CoreMissing { .. } | GatherPlaced::Missing { .. } => 0,
                };
                let Some(next) = total.checked_add(size) else { return false };
                total = next;
            }
            if total > budget {
                return false;
            }
        }
    }
    true
}
