//! Capacity bounds for typed core sections and connector section tokens
//! (domain/engine.md, section 9).

/// Limits supplied by the core to the brief child at every step.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Limits {
    /// Briefs gathering at once.
    pub briefs: u32,
    /// Sections in one brief.
    pub sections: u32,
    /// Largest size a connector may report for one section.
    pub read_bytes: u32,
    /// Largest total size of one completed brief.
    pub brief_bytes: u32,
}
