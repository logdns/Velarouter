//! Local, bounded history of successful connections. No connection passwords.
use vr_net::connection_code::{self, Destination};

const MAX_PER_GROUP: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Group {
    Lan,
    Internet,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Self::Lan => "局域网",
            Self::Internet => "互联网",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(super) struct Entry {
    code: String,
    peer: String,
}

impl Entry {
    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn peer(&self) -> &str {
        &self.peer
    }

    fn group(&self) -> Option<Group> {
        match connection_code::parse(&self.code).ok()? {
            Destination::Direct(_) if self.code.starts_with("VR-") => Some(Group::Lan),
            Destination::Direct(_) | Destination::Meet(_) | Destination::Signal(_) => {
                Some(Group::Internet)
            }
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct History(Vec<Entry>);

impl History {
    pub fn from_saved(entries: Vec<Entry>) -> Self {
        let mut history = Self::default();
        for entry in entries.into_iter().rev() {
            history.record(&entry.code, &entry.peer);
        }
        history
    }

    pub fn entries(&self) -> &[Entry] {
        &self.0
    }

    pub fn for_group(&self, group: Group) -> impl Iterator<Item = &Entry> {
        self.0
            .iter()
            .filter(move |entry| entry.group() == Some(group))
    }

    pub fn record(&mut self, code: &str, peer: &str) {
        let entry = Entry {
            code: code
                .split_whitespace()
                .collect::<String>()
                .to_ascii_uppercase(),
            peer: peer
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(80)
                .collect(),
        };
        let Some(group) = entry.group() else { return };
        self.remove(&entry.code);
        // Retain independent limits so frequent Internet connections do not
        // evict the local devices (and vice versa).
        if let Some(index) = (0..self.0.len())
            .filter(|&index| self.0[index].group() == Some(group))
            .nth(MAX_PER_GROUP - 1)
        {
            self.0.remove(index);
        }
        self.0.insert(0, entry);
    }

    pub fn remove(&mut self, code: &str) {
        self.0.retain(|entry| entry.code != code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_entries_are_normalized_recency_ordered_and_bounded_per_group() {
        let mut history = History::default();
        let lan_code = vr_net::direct::encode("192.0.2.10:34789".parse().unwrap());
        history.record(&lan_code.to_ascii_lowercase(), "LAN desktop");
        for number in 123456789010..123456789017_u64 {
            history.record(&number.to_string(), "Internet desktop");
        }
        assert_eq!(history.for_group(Group::Internet).count(), MAX_PER_GROUP);
        assert_eq!(history.for_group(Group::Lan).count(), 1);
        assert_eq!(history.entries()[0].code(), "123456789016");
        history.record("1234 5678 9013", "Renamed desktop");
        assert_eq!(history.entries()[0].code(), "123456789013");
        assert_eq!(history.entries()[0].peer(), "Renamed desktop");
        assert_eq!(history.for_group(Group::Internet).count(), MAX_PER_GROUP);
        history.remove(&lan_code);
        assert_eq!(history.for_group(Group::Lan).count(), 0);
    }

    #[test]
    fn stored_history_contains_only_code_and_peer_and_rejects_raw_addresses() {
        let mut history = History::default();
        history.record("192.0.2.10:34789", "Raw address");
        assert!(history.entries().is_empty());
        history.record("123456789012", "Peer");
        let saved = serde_json::to_value(history.entries()).unwrap();
        assert_eq!(
            saved,
            serde_json::json!([{ "code": "123456789012", "peer": "Peer" }])
        );
        let loaded = History::from_saved(serde_json::from_value(saved).unwrap());
        assert_eq!(loaded.entries(), history.entries());
    }
}
