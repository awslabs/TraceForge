use serde::{Deserialize, Serialize};

use crate::{
    event_label::SendMsg, identifier::Identifier, predicate::PredicateType, thread::ThreadId,
};

use std::fmt::{Debug, Display};

// Distinct unit type for async logic
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WakeMsg;

#[derive(Clone, Copy, Default, Debug, Hash, Serialize, Deserialize, PartialEq, Eq)]
pub enum CommunicationModel {
    // A receive can read from any matching unread send
    NoOrder,
    /// A receive can read any node's last matching unread send (FIFO)
    #[default]
    LocalOrder,
    /// A receive can read any porf-minimal matching unread send
    CausalOrder,
    /// There exists a total order s.t. receives read from the last matching send
    TotalOrder,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Loc(Box<dyn Identifier>);

impl Loc {
    pub(crate) fn new<T: crate::identifier::Identifier>(id: T) -> Self {
        Loc(Box::new(id))
    }
}

impl Display for Loc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// We avoid (de)serialization of Locations, and instead restore them during replay.
// Since they do not implement Default, we wrap them inside an Option.
// If there is a bug in the restoring process, we will panic on unrwap().

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RecvLoc {
    #[serde(skip)]
    locs: Option<Vec<Loc>>,
    // Option for the fast case where the predicate would be always true
    tag: Option<PredicateType>,
}

impl RecvLoc {
    pub(crate) fn new(locs: Vec<&Loc>, tag: Option<PredicateType>) -> Self {
        RecvLoc {
            locs: Some(locs.into_iter().cloned().collect()),
            tag,
        }
    }

    pub(crate) fn locs(&self) -> &Vec<Loc> {
        self.locs.as_ref().unwrap()
    }

    /// Returns whether the receive's tag matches the send's tag
    pub(crate) fn matches_tag(&self, send: &SendMsg) -> bool {
        let send_loc = send.send_loc();
        self.tag.is_none() || self.tag.as_ref().unwrap().0(send_loc.sender_tid, send_loc.tag)
    }

    /// Return whether the receive's tag and any of it's locations matches the send
    pub(crate) fn matches(&self, send: &SendMsg) -> bool {
        self.matches_tag(send) && self.locs().iter().any(|rc| rc == send.loc())
    }

    /// Given a matching SendLoc, it returns the index of the unique matching location
    pub(crate) fn get_matching_index(&self, send_loc: &SendLoc) -> usize {
        self.locs()
            .iter()
            .position(|c| c == send_loc.loc())
            .unwrap()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SendLoc {
    #[serde(skip)]
    loc: Option<Loc>,
    pub(crate) sender_tid: ThreadId,
    pub(crate) tag: Option<u32>,
}

impl SendLoc {
    //Dummy, only for tests
    #[allow(dead_code)]
    pub(crate) fn new_empty(sender_tid: ThreadId) -> Self {
        SendLoc {
            loc: None,
            sender_tid,
            tag: None,
        }
    }

    pub(crate) fn new(loc: &Loc, sender_tid: ThreadId, tag: Option<u32>) -> Self {
        SendLoc {
            loc: Some(loc.clone()),
            sender_tid,
            tag,
        }
    }

    pub(crate) fn loc(&self) -> &Loc {
        self.loc.as_ref().unwrap()
    }
}

// TODO: How to Display?
impl Display for RecvLoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.locs)
    }
}

impl Display for SendLoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.loc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match() {
        let id1: Loc = Loc::new("foo".to_string());
        let id2: Loc = Loc::new("bar".to_string());
        let id3: Loc = Loc::new(42);
        let id4: Loc = Loc::new(42);
        assert!(id1 != id2);
        assert!(id2 != id3);
        assert!(id3 == id4);
    }

    #[test]
    fn test_display() {
        let id: Loc = Loc::new("foo".to_string());
        assert_eq!(format!("{:}", id), "\"foo\"")
    }

    #[test]
    fn test_debug() {
        let id: Loc = Loc::new("foo".to_string());
        assert_eq!(format!("{:?}", id), "Loc(\"foo\")")
    }

    #[test]
    fn test_clone() {
        let id1: Loc = Loc::new("foo".to_string());
        let id2: Loc = Loc::new(42);
        assert!(id1 == id1.clone());
        assert!(id1.clone() != id2);
    }
}
