//! Orders Bidi messages by append sequence number.

use std::collections::BTreeMap;

pub struct OrderedInbox<T> {
    next: i64,
    pending: BTreeMap<i64, T>,
}

impl<T> Default for OrderedInbox<T> {
    fn default() -> Self {
        Self::starting_at(0)
    }
}

impl<T> OrderedInbox<T> {
    pub fn starting_at(next: i64) -> Self {
        Self {
            next,
            pending: BTreeMap::new(),
        }
    }

    pub fn push(&mut self, seqno: i64, value: T) -> Vec<(i64, T)> {
        if seqno < self.next || self.pending.contains_key(&seqno) {
            return Vec::new();
        }
        self.pending.insert(seqno, value);
        let mut ready = Vec::new();
        while let Some(value) = self.pending.remove(&self.next) {
            ready.push((self.next, value));
            self.next = self.next.saturating_add(1);
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::OrderedInbox;

    #[test]
    fn default_releases_first_protocol_append() {
        let mut inbox = OrderedInbox::default();
        assert_eq!(inbox.push(0, "first"), vec![(0, "first")]);
        assert_eq!(inbox.push(1, "second"), vec![(1, "second")]);
    }

    #[test]
    fn independent_inboxes_release_their_first_appends() {
        let mut first = OrderedInbox::default();
        let mut second = OrderedInbox::default();

        assert_eq!(first.push(0, "first-request"), vec![(0, "first-request")]);
        assert_eq!(
            second.push(0, "second-request"),
            vec![(0, "second-request")]
        );
    }
}
