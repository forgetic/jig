//! One authenticated party's bounded historical result or inbox read.

use jig_core_people as people;
use skein_lib::Token;

use crate::Core;

impl Core {
    /// Whether this party already owns a historical result or inbox read.
    #[must_use]
    pub fn reading_result(&self, person: u64) -> bool {
        self.reading_results.contains_key(&person)
    }

    /// Reserve one root store read for an authenticated party.
    #[must_use]
    pub fn reserve_result_read(&mut self, person: u64, waiter: Token) -> bool {
        self.reading_results.insert(person, waiter) == Ok(None)
    }

    /// Release a completed or failed historical read.
    #[must_use]
    pub fn release_result_read(&mut self, person: u64, waiter: Token) -> bool {
        self.reading_results.remove(&person) == Some(waiter)
    }

    /// Advance the committed inbox cursor after the root assembled its final page.
    #[must_use]
    pub fn advance_inbox_read(&mut self, person: u64, high: u64) -> Option<people::Stored> {
        self.people.advance_read_position(person, high)
    }
}
