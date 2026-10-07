use skein_lib::{List, Map};

use crate::{Named, Path, Record, RecordKey, ResourceRole};

/// Bounds for the connector's live records and one step's output.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Limits {
    /// Maximum live tasks.
    pub tasks: u32,
    /// Maximum adopted project-resource pairs.
    pub adoptions: u32,
    /// Maximum task-topic subscriptions.
    pub subscriptions: u32,
    /// Maximum pools.
    pub pools: u32,
    /// Maximum configured resources.
    pub resources: u32,
    /// Maximum configured topics.
    pub topics: u32,
    /// Maximum resources named by one task.
    pub resources_per_task: u32,
    /// Maximum subscribers delivered by one news event.
    pub subscribers_per_topic: u32,
    /// Maximum lost allocations reported in one pool event.
    pub lost_per_pool: u32,
    /// Maximum segments in a path.
    pub path_segments: u32,
    /// Maximum bytes in one segment.
    pub segment_bytes: u32,
}

/// The maximum heap used by the connector's tables and their owned paths.
#[must_use]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    if limits.path_segments == 0 || limits.segment_bytes == 0 {
        return None;
    }
    let path_bytes = u64::from(limits.path_segments).checked_mul(u64::from(limits.segment_bytes).checked_add(16)?)?;
    let task_paths =
        u64::from(limits.tasks).checked_mul(u64::from(limits.resources_per_task))?.checked_mul(path_bytes)?;
    Map::<u64, Record>::worst_case(limits.tasks)?
        .checked_add(Map::<(u32, Path), ResourceRole>::worst_case(limits.adoptions)?)?
        .checked_add(Map::<(u16, u64), (u16, u16)>::worst_case(limits.subscriptions)?)?
        .checked_add(Map::<Path, u32>::worst_case(limits.pools)?)?
        .checked_add(List::<Named>::worst_case(limits.resources_per_task)?)?
        .checked_add(List::<RecordKey>::worst_case(limits.subscriptions)?)?
        .checked_add(List::<crate::ResourceSpec>::worst_case(limits.resources)?)?
        .checked_add(List::<crate::PoolSpec>::worst_case(limits.pools)?)?
        .checked_add(task_paths)?
        .checked_add(u64::from(limits.adoptions).checked_mul(path_bytes)?)?
        .checked_add(u64::from(limits.pools).checked_mul(path_bytes)?)?
        .checked_add(u64::from(limits.resources).checked_mul(path_bytes)?)?
        .checked_add(List::<crate::TopicSpec>::worst_case(limits.topics)?)?
        .checked_add(path_bytes)
}
