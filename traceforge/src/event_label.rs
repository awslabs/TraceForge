//! Label of an execution graph event

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::ops::RangeInclusive;

use crate::event::Event;
use crate::loc::{CommunicationModel, Loc, RecvLoc, SendLoc, WakeMsg};
use crate::msg::Val;
use crate::thread::main_thread_id;
use crate::vector_clock::VectorClock;
use crate::ThreadId;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum LabelEnum {
    Begin(Begin),
    End(End),
    TCreate(TCreate),
    TJoin(TJoin),
    RecvMsg(RecvMsg),
    SendMsg(SendMsg),
    Unique(Unique),
    CToss(CToss),
    Choice(Choice),
    Sample(Sample),
    Block(Block),
}

macro_rules! match_and_run {
    ( $lab:expr, $name:ident $( , $arg:ident )* ) => {
        match $lab {
            LabelEnum::Begin(l) => l.as_event_label().$name($($arg),*),
            LabelEnum::End(l) => l.as_event_label().$name($($arg),*),
            LabelEnum::TCreate(l) => l.as_event_label().$name($($arg),*),
            LabelEnum::TJoin(l) => l.as_event_label().$name($($arg),*),
            LabelEnum::RecvMsg(l) => l.as_event_label().$name($($arg),*),
            LabelEnum::SendMsg(l) => l.as_event_label().$name($($arg),*),
            LabelEnum::Unique(l) => l.as_event_label().$name($($arg),*),
            LabelEnum::CToss(l) => l.as_event_label().$name($($arg),*),
            LabelEnum::Choice(l) => l.as_event_label().$name($($arg),*),
            LabelEnum::Sample(l) => l.as_event_label().$name($($arg),*),
            LabelEnum::Block(l) => l.as_event_label().$name($($arg),*),
        }
    };
}

macro_rules! match_and_run_mut {
    ( $lab:expr, $name:ident $( , $arg:ident )* ) => {
        match $lab {
            LabelEnum::Begin(l) => l.as_event_label_mut().$name($($arg),*),
            LabelEnum::End(l) => l.as_event_label_mut().$name($($arg),*),
            LabelEnum::TCreate(l) => l.as_event_label_mut().$name($($arg),*),
            LabelEnum::TJoin(l) => l.as_event_label_mut().$name($($arg),*),
            LabelEnum::RecvMsg(l) => l.as_event_label_mut().$name($($arg),*),
            LabelEnum::SendMsg(l) => l.as_event_label_mut().$name($($arg),*),
            LabelEnum::Unique(l) => l.as_event_label_mut().$name($($arg),*),
            LabelEnum::CToss(l) => l.as_event_label_mut().$name($($arg),*),
            LabelEnum::Choice(l) => l.as_event_label_mut().$name($($arg),*),
            LabelEnum::Sample(l) => l.as_event_label_mut().$name($($arg),*),
            LabelEnum::Block(l) => l.as_event_label_mut().$name($($arg),*),
        }
    };
}

impl LabelEnum {
    pub(crate) fn pos(&self) -> Event {
        match_and_run!(self, pos)
    }

    pub(crate) fn index(&self) -> u32 {
        match_and_run!(self, index)
    }

    pub(crate) fn thread(&self) -> ThreadId {
        match_and_run!(self, thread)
    }

    pub(crate) fn stamped(&self) -> bool {
        match_and_run!(self, stamped)
    }

    pub(crate) fn stamp(&self) -> usize {
        match_and_run!(self, stamp)
    }

    pub(crate) fn set_stamp(&mut self, s: usize) {
        match_and_run_mut!(self, set_stamp, s)
    }

    /// This includes the event, but not its rf/TCreate/TEnd dependencies.
    /// Therefore, direct access should be avoided, unless,
    /// e.g. the event is a SendMsg (there are no such dependencies).
    pub(crate) fn cached_porf(&self) -> &VectorClock {
        match_and_run!(self, cached_porf)
    }

    pub(crate) fn set_porf_cache(&mut self, v: VectorClock) {
        match_and_run_mut!(self, set_porf_cache, v)
    }

    /// Similar to cached_porf.
    pub(crate) fn cached_posw(&self) -> &VectorClock {
        match_and_run!(self, cached_posw)
    }

    pub(crate) fn set_posw_cache(&mut self, v: VectorClock) {
        match_and_run_mut!(self, set_posw_cache, v)
    }

    pub(crate) fn compare_for_replay(&self, other: &Self) -> Result<(), String> {
        match self {
            LabelEnum::Begin(_) => {
                if let LabelEnum::Begin(_) = other {
                    return Ok(()); // Comparison not needed because Begin is generated from TCreate which is checked.
                }
            }
            LabelEnum::End(s) => {
                if let LabelEnum::End(o) = other {
                    if !s.result().is_pending() && s.result() != o.result() {
                        return Err(format!(
                            "Expected the thread to return {:?} but it returned {:?}",
                            s.result(),
                            o.result()
                        ));
                    }
                    return Ok(());
                }
            }
            LabelEnum::TCreate(s) => {
                if let LabelEnum::TCreate(o) = other {
                    if s.name() != o.name() {
                        return Err(format!(
                            "Expected the thread to be named {:?} but it was named {:?}",
                            s.name(),
                            o.name()
                        ));
                    } else if s.is_daemon != o.is_daemon {
                        return Err(format!(
                            "Expected the is_daemon={} but got is_daemon={}",
                            s.is_daemon, o.is_daemon
                        ));
                    } else if s.sym_cid() != o.sym_cid() {
                        return Err(format!(
                            "Expected the symmetric thread id={:?} but got {:?}",
                            s.sym_cid(),
                            o.sym_cid()
                        ));
                    }
                    return Ok(());
                }
            }
            LabelEnum::TJoin(s) => {
                if let LabelEnum::TJoin(o) = other {
                    if s.cid() != o.cid() {
                        return Err(format!(
                            "Expected to join thread id={:?} but got thread id={:?}",
                            s.cid(),
                            o.cid()
                        ));
                    }
                    return Ok(());
                }
            }
            LabelEnum::RecvMsg(_s) => {
                if let LabelEnum::RecvMsg(_o) = other {
                    return Ok(());
                }
            }
            LabelEnum::SendMsg(s) => {
                if let LabelEnum::SendMsg(o) = other {
                    if !s.val().is_pending() && s.val() != o.val() {
                        return Err(format!(
                            "Expected to send message {:?} but actually sent {:?}",
                            s.val(),
                            o.val()
                        ));
                    }
                    if !s.val().is_pending() && s.monitor_sends() != o.monitor_sends() {
                        return Err(format!(
                            "Expected to send messages {:?} to monitors but actually sent {:?}",
                            s.monitor_sends(),
                            o.monitor_sends()
                        ));
                    }
                    if !Self::slocs_are_compatible(&s.loc, &o.loc) {
                        return Err(format!(
                            "Expected to send message with tag={:?} from {} but actually sent with={:?} from {}",
                            s.loc.tag,
                            s.loc.sender_tid,
                            o.loc.tag,
                            o.loc.sender_tid,
                        ));
                    }
                    return Ok(());
                }
            }
            LabelEnum::Unique(s) => {
                if let LabelEnum::Unique(o) = other {
                    if s.comm != o.comm {
                        return Err(format!(
                            "Expected to create a {:?} channel but actually created a {:?} channel",
                            s.comm, o.comm
                        ));
                    }
                    return Ok(());
                }
            }
            LabelEnum::CToss(_) => {
                if let LabelEnum::CToss(_) = other {
                    return Ok(()); // CToss has no parameters to vary.
                }
            }
            LabelEnum::Choice(s) => {
                if let LabelEnum::Choice(o) = other {
                    if s.range() != o.range() {
                        return Err(format!(
                            "Expected nondet over range {:?} but got {:?}",
                            s.range(),
                            o.range()
                        ));
                    } else {
                        return Ok(());
                    }
                }
            }
            LabelEnum::Sample(_) => {
                if let LabelEnum::Sample(_) = other {
                    return Ok(()); // Experimental feature so we're not sure how to compare this.
                }
            }
            LabelEnum::Block(s) => {
                if let LabelEnum::Block(o) = other {
                    if !Self::blocks_are_compatible(&s.btype, &o.btype) {
                        return Err(format!(
                            "Expected to block on {:?} but got {:?}",
                            s.btype(),
                            o.btype()
                        ));
                    }
                    return Ok(());
                }
            }
        }

        if let (LabelEnum::Block(_), LabelEnum::End(_)) = (self, other) {
            return Ok(()); // This happens during estimation mode.
        }

        let expected = self.get_action_descr();
        let actual = other.get_action_descr();

        Err(format!(
            "At this point in the thread, it should have {} but it {} instead.",
            expected, actual
        ))
    }

    fn blocks_are_compatible(block1: &BlockType, block2: &BlockType) -> bool {
        match (block1, block2) {
            (BlockType::Assume, BlockType::Assume) => true,
            (BlockType::Assert, BlockType::Assert) => true,
            // Blocking on value is a temporary internal label,
            // and thus we will never reach this path.
            // If ever needed, return true since we can compare neither locations
            // (they are lost during deserialization) nor tags (they are predicates)
            (BlockType::Value(_), BlockType::Value(_)) => unreachable!(),
            _ => false,
        }
    }

    // Only compare sender and tag, since the location is lost during (de)serialization
    // and will be constructed from the new one
    fn slocs_are_compatible(loc1: &SendLoc, loc2: &SendLoc) -> bool {
        loc1.sender_tid == loc2.sender_tid && loc1.tag == loc2.tag
    }

    pub(crate) fn get_action_descr(&self) -> String {
        match self {
            LabelEnum::Begin(_) => "started".to_string(),
            LabelEnum::End(_) => "exited".to_string(),
            LabelEnum::TCreate(_) => "spawned another thread/future".to_string(),
            LabelEnum::TJoin(_) => "joined a thread".to_string(),
            LabelEnum::RecvMsg(_) => "requested a message for receipt".to_string(),
            LabelEnum::SendMsg(_) => "sent a message".to_string(),
            LabelEnum::Unique(_) => "created a new channel".to_string(),
            LabelEnum::CToss(_) => "called nondet() -> bool".to_string(),
            LabelEnum::Choice(s) => format!("called Range({:?})::nondet", s.range()),
            LabelEnum::Sample(_) => "called sample()".to_string(),
            LabelEnum::Block(_) => "became blocked".to_string(),
        }
    }
}

impl fmt::Display for LabelEnum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LabelEnum::Begin(lab) => write!(f, "{}", lab),
            LabelEnum::End(lab) => write!(f, "{}", lab),
            LabelEnum::TCreate(lab) => write!(f, "{}", lab),
            LabelEnum::TJoin(lab) => write!(f, "{}", lab),
            LabelEnum::RecvMsg(lab) => write!(f, "{}", lab),
            LabelEnum::SendMsg(lab) => write!(f, "{}", lab),
            LabelEnum::Unique(lab) => write!(f, "{}", lab),
            LabelEnum::CToss(lab) => write!(f, "{}", lab),
            LabelEnum::Choice(lab) => write!(f, "{}", lab),
            LabelEnum::Sample(lab) => write!(f, "{}", lab),
            LabelEnum::Block(lab) => write!(f, "{}", lab),
        }
    }
}

impl fmt::Debug for LabelEnum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LabelEnum::Begin(lab) => write!(f, "{}", lab),
            LabelEnum::End(lab) => write!(f, "{}", lab),
            LabelEnum::TCreate(lab) => write!(f, "{}", lab),
            LabelEnum::TJoin(lab) => write!(f, "{}", lab),
            LabelEnum::RecvMsg(lab) => write!(f, "{}", lab),
            LabelEnum::SendMsg(lab) => write!(f, "{}", lab),
            LabelEnum::Unique(lab) => write!(f, "{}", lab),
            LabelEnum::CToss(lab) => write!(f, "{}", lab),
            LabelEnum::Choice(lab) => write!(f, "{}", lab),
            LabelEnum::Sample(lab) => write!(f, "{}", lab),
            LabelEnum::Block(lab) => write!(f, "{}", lab),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct EventLabel {
    pos: Event,
    stamp: Option<usize>,

    /// Cached porf view up to and including the current event,
    /// without the direct Send/Create/End dependencies
    /// for Receive/Begin/Join, respectively.
    ///
    /// This corresponds to a core concept of the algorithm
    /// and is relevant outside consistency checking as well.
    cached_porf: VectorClock,

    // Similar to cached porf, but posw = (po U sw)^+,
    // where sw is rf restricted to events up to CausalOrder (i.e. not TotalOrder).
    // This is useful for checking consistency under CausalOrder.
    cached_posw: VectorClock,
}

impl EventLabel {
    fn new(p: Event) -> Self {
        Self {
            pos: p,
            stamp: None,
            cached_porf: VectorClock::new(),
            cached_posw: VectorClock::new(),
        }
    }

    fn main() -> Self {
        let mut vec = VectorClock::new();
        let pos = Event {
            thread: main_thread_id(),
            index: 0,
        };
        vec.set_tid(pos.thread);
        Self {
            pos,
            stamp: Some(0),
            cached_porf: vec.clone(),
            cached_posw: vec.clone(),
        }
    }

    pub(crate) fn pos(&self) -> Event {
        self.pos
    }

    pub(crate) fn index(&self) -> u32 {
        self.pos.index
    }

    pub(crate) fn thread(&self) -> ThreadId {
        self.pos.thread
    }

    pub(crate) fn stamped(&self) -> bool {
        self.stamp.is_some()
    }

    pub(crate) fn stamp(&self) -> usize {
        self.stamp.unwrap()
    }

    pub(crate) fn set_stamp(&mut self, s: usize) {
        self.stamp = Some(s)
    }

    pub(self) fn cached_porf(&self) -> &VectorClock {
        &self.cached_porf
    }

    pub(crate) fn set_porf_cache(&mut self, v: VectorClock) {
        self.cached_porf = v
    }

    pub(crate) fn cached_posw(&self) -> &VectorClock {
        &self.cached_posw
    }

    pub(crate) fn set_posw_cache(&mut self, v: VectorClock) {
        self.cached_posw = v
    }
}

impl fmt::Display for EventLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if cfg!(feature = "print_stamps") {
            write!(f, "{} @ {}", self.stamp(), self.pos(),)
        } else {
            write!(f, "{}", self.pos(),)
        }
    }
}

pub(crate) trait AsEventLabel {
    fn as_event_label(&self) -> &EventLabel;
    fn as_event_label_mut(&mut self) -> &mut EventLabel;
    fn pos(&self) -> Event;
    fn stamp(&self) -> usize;
}

macro_rules! as_label {
    ($t:ty) => {
        impl AsEventLabel for $t {
            fn as_event_label(&self) -> &EventLabel {
                &self.label
            }
            fn as_event_label_mut(&mut self) -> &mut EventLabel {
                &mut self.label
            }
            fn pos(&self) -> Event {
                self.as_event_label().pos()
            }
            fn stamp(&self) -> usize {
                self.as_event_label().stamp()
            }
        }
    };
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Begin {
    label: EventLabel,
    parent: Option<Event>,
    sym_id: Option<ThreadId>,
}

impl Begin {
    pub(crate) fn new(pos: Event, parent: Option<Event>, symm_id: Option<ThreadId>) -> Self {
        Self {
            label: EventLabel::new(pos),
            parent,
            sym_id: symm_id,
        }
    }

    pub(crate) fn main() -> Self {
        Self {
            label: EventLabel::main(),
            parent: None,
            sym_id: None,
        }
    }

    pub(crate) fn parent(&self) -> Option<Event> {
        self.parent
    }

    pub(crate) fn sym_id(&self) -> Option<ThreadId> {
        self.sym_id
    }
}

as_label!(Begin);

impl fmt::Display for Begin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: BEGIN", self.as_event_label(),)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct End {
    label: EventLabel,
    #[serde(skip)]
    pub(crate) result: Val,
}

impl End {
    pub(crate) fn new(pos: Event, result: Val) -> Self {
        Self {
            label: EventLabel::new(pos),
            result,
        }
    }

    pub(crate) fn result(&self) -> &Val {
        &self.result
    }

    pub(crate) fn recover_result(&mut self, other: Self) {
        self.result = other.result;
    }

    pub(crate) fn recover_lost(&mut self, other: Self) {
        self.result = other.result;
    }
}

as_label!(End);

impl fmt::Display for End {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[cfg(not(feature = "print_vals_custom"))]
        {
            write!(f, "{}: END", self.as_event_label())
        }
        #[cfg(feature = "print_vals_custom")]
        {
            write!(f, "{}: END ({})", self.as_event_label(), self.result(),)
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct TCreate {
    label: EventLabel,
    cid: ThreadId,
    name: Option<String>,
    is_daemon: bool,
    sym_cid: Option<ThreadId>,
    origination_vec: Vec<u32>,
}

impl TCreate {
    pub(crate) fn new(
        pos: Event,
        cid: ThreadId,
        name: Option<String>,
        is_daemon: bool,
        sym_cid: Option<ThreadId>,
        origination_vec: Vec<u32>,
    ) -> Self {
        Self {
            label: EventLabel::new(pos),
            cid,
            name,
            is_daemon,
            sym_cid,
            origination_vec,
        }
    }

    pub(crate) fn cid(&self) -> ThreadId {
        self.cid
    }

    #[allow(dead_code)]
    pub(crate) fn sym_cid(&self) -> Option<ThreadId> {
        self.sym_cid
    }

    pub(crate) fn name(&self) -> &Option<String> {
        &self.name
    }

    pub(crate) fn daemon(&self) -> bool {
        self.is_daemon
    }

    pub(crate) fn origination_vec(&self) -> Vec<u32> {
        self.origination_vec.clone()
    }
}

as_label!(TCreate);

impl fmt::Display for TCreate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tid = self.cid();
        let tname = if self.name().is_none() {
            "".to_owned()
        } else {
            format!(":\"{}\"", self.name().as_ref().unwrap())
        };
        let daemon = if self.is_daemon { "[daemon]" } else { "" }.to_owned();
        write!(
            f,
            "{}: TCREATE({}{}{})",
            self.as_event_label(),
            tid,
            tname,
            daemon
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct TJoin {
    label: EventLabel,
    cid: ThreadId,
}

impl TJoin {
    pub(crate) fn new(pos: Event, cid: ThreadId) -> Self {
        Self {
            label: EventLabel::new(pos),
            cid,
        }
    }

    pub(crate) fn cid(&self) -> ThreadId {
        self.cid
    }
}

as_label!(TJoin);

impl fmt::Display for TJoin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: TJOIN({})", self.as_event_label(), self.cid)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RecvMsg {
    label: EventLabel,
    loc: RecvLoc,
    comm: CommunicationModel,
    rf: Option<Event>,
    non_blocking: bool,
    revisitable: bool,
}

impl RecvMsg {
    pub(crate) fn new(
        pos: Event,
        loc: RecvLoc,
        comm: CommunicationModel,
        rf: Option<Event>,
        non_blocking: bool,
    ) -> Self {
        Self {
            label: EventLabel::new(pos),
            loc,
            comm,
            rf,
            non_blocking,
            revisitable: true,
        }
    }

    pub(crate) fn rf(&self) -> Option<Event> {
        self.rf
    }

    pub(crate) fn set_rf(&mut self, rf: Option<Event>) {
        self.rf = rf
    }

    pub(crate) fn is_non_blocking(&self) -> bool {
        self.non_blocking
    }

    pub(crate) fn is_revisitable(&self) -> bool {
        self.revisitable
    }

    pub(crate) fn set_revisitable(&mut self, status: bool) {
        self.revisitable = status
    }

    /// Return whether the receive's thread should observe the send,
    /// i.e.g whether this is a monitor receive that accepts the send
    pub(crate) fn monitors(&self, send: &SendMsg) -> bool {
        // If the receive accepts the send, we only consider
        // "monitor-reading" from it: directly sending a message
        // to a monitor that observes you is treated the same
        send.is_monitored_from(&self.as_event_label().pos.thread)
    }

    pub(crate) fn recover_lost(&mut self, other: Self) {
        self.loc = other.loc;
    }

    /// Return if either the send is monitored by the receive or the locations match
    pub(crate) fn matches(&self, send: &SendMsg) -> bool {
        self.monitors(send) || self.recv_loc().matches(send)
    }

    pub(crate) fn recv_loc(&self) -> &RecvLoc {
        &self.loc
    }

    pub(crate) fn comm(&self) -> CommunicationModel {
        self.comm
    }

    pub(crate) fn receiver(&self) -> ThreadId {
        self.label.pos.thread
    }

    pub(crate) fn sender(&self) -> ThreadId {
        self.rf().unwrap().thread
    }

    // N.B. This doesn't include the rf dependency
    pub(crate) fn cached_porf(&self) -> &VectorClock {
        &self.as_event_label().cached_porf
    }
}

as_label!(RecvMsg);

impl fmt::Display for RecvMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: RECV() [{}]",
            self.label,
            if self.rf().is_none() {
                "TIMEOUT".to_string()
            } else {
                format!("{}", self.rf().unwrap())
            },
        )
    }
}

pub(crate) type MonitorSends = BTreeMap<ThreadId, Val>;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SendMsg {
    label: EventLabel,
    #[serde(skip)]
    pub(crate) val: Val,
    loc: SendLoc,
    comm: CommunicationModel,
    lossy: bool,
    dropped: bool,
    // sends_before: The sends that are ordered before, depending on the communication model.
    sb: VectorClock,
    // Reader fields cache the receives that read from this send.
    // This is an optimization and needs to be maintained when updating the graph.
    /// Receive that reads from this send.
    reader: Option<Event>,
    /// Monitor receives that currently read from this send.
    monitor_readers: Vec<Event>,
    /// Monitor threads that accept this message,
    /// and the respective value they would observe.
    #[serde(skip)]
    monitor_sends: MonitorSends,
}

impl SendMsg {
    pub(crate) fn new(
        pos: Event,
        loc: SendLoc,
        comm: CommunicationModel,
        val: Val,
        monitor_sends: MonitorSends,
        lossy: bool,
    ) -> Self {
        Self {
            label: EventLabel::new(pos),
            val,
            loc,
            comm,
            lossy,
            dropped: false,
            sb: VectorClock::new(),
            reader: None,
            monitor_readers: Vec::new(),
            monitor_sends,
        }
    }

    pub(crate) fn recover_val(&mut self, other: Self) {
        self.val = other.val;
    }

    pub(crate) fn recover_lost(&mut self, other: Self) {
        self.val = other.val;
        self.monitor_sends = other.monitor_sends;
        self.loc = other.loc;
    }

    pub(crate) fn porf(&self) -> &VectorClock {
        // A send event has no direct non-po dependencies, so cached porf suffices
        &self.as_event_label().cached_porf
    }

    pub(crate) fn loc(&self) -> &Loc {
        self.send_loc().loc()
    }

    /// Returns the implicits sends to the monitor threads, along with the
    /// values the monitors would observe.
    pub(crate) fn monitor_sends(&self) -> &MonitorSends {
        &self.monitor_sends
    }

    /// Only used when print_val
    pub fn send_loc(&self) -> &SendLoc {
        &self.loc
    }

    pub(crate) fn is_lossy(&self) -> bool {
        self.lossy
    }

    pub(crate) fn is_dropped(&self) -> bool {
        self.dropped
    }

    pub(crate) fn set_dropped(&mut self) {
        self.dropped = true;
    }

    pub(crate) fn sb(&self) -> &VectorClock {
        &self.sb
    }

    pub(crate) fn set_sb(&mut self, v: VectorClock) {
        self.sb = v;
    }

    pub(crate) fn comm(&self) -> CommunicationModel {
        self.comm
    }

    pub(crate) fn val(&self) -> &Val {
        &self.val
    }

    /// The value a receive would obtain by reading from this send.
    /// Might be different from the actual value if the send
    /// is monitored by the receive.
    pub(crate) fn recv_val(&self, recv: &RecvMsg) -> &Val {
        let mon_val = self.monitor_sends.get(&recv.as_event_label().pos.thread);
        if let Some(mv) = mon_val {
            mv
        } else {
            &self.val
        }
    }

    pub(crate) fn reader(&self) -> Option<Event> {
        self.reader
    }

    pub(crate) fn is_monitored_from(&self, tid: &ThreadId) -> bool {
        self.monitor_sends.contains_key(tid)
    }

    pub(crate) fn can_be_monitor_read(&self, reader: &Event) -> bool {
        self.is_monitored_from(&reader.thread)
            && !self
                .monitor_readers
                .iter()
                .any(|e| e.thread == reader.thread)
    }

    pub(crate) fn is_monitor_read(&self) -> bool {
        self.monitor_readers.is_empty()
    }

    pub(crate) fn add_monitor_reader(&mut self, new_reader: Event) {
        // debug_assert!(!self.monitor_readers.contains(&new_reader));
        self.monitor_readers.push(new_reader);
    }

    pub(crate) fn remove_monitor_reader(&mut self, old_reader: Event) {
        // debug_assert!(self.monitor_readers.contains(&old_reader));
        self.monitor_readers.retain(|&x| x != old_reader);
    }

    pub(crate) fn set_reader(&mut self, reader: Option<Event>) {
        self.reader = reader
    }

    pub(crate) fn is_unread(&self) -> bool {
        self.reader().is_none()
    }

    pub(crate) fn is_cancelled_wrt(&self, other: &EventLabel) -> bool {
        // cached_porf to disregard own rf (porf;po prefix)
        self.val.as_any().downcast::<WakeMsg>().is_ok() && other.cached_porf().contains(self.pos())
    }
    pub(crate) fn can_be_read_from(&self, reader_loc: &RecvLoc) -> bool {
        self.is_unread() && reader_loc.matches(self)
    }

    pub(crate) fn monitor_readers(&self) -> &Vec<Event> {
        &self.monitor_readers
    }

    #[allow(unused)]
    pub(crate) fn sender(&self) -> ThreadId {
        self.label.pos.thread
    }
}

as_label!(SendMsg);

impl fmt::Display for SendMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[cfg(not(any(feature = "print_vals", feature = "print_vals_custom")))]
        {
            write!(
                f,
                "{}: SEND(T{}){}",
                self.label,
                self.send_loc(),
                if self.is_dropped() { " [dropped]" } else { "" }
            )
        }
        #[cfg(feature = "print_vals")]
        {
            write!(
                f,
                "{}: SEND(T{}, {:?}){}",
                self.label,
                self.send_loc(),
                // TODO: How to Display monitor values?
                self.val(),
                if self.is_dropped() { " [dropped]" } else { "" },
            )
        }
        #[cfg(feature = "print_vals_custom")]
        {
            write!(
                f,
                "{}: SEND(T{}, {}){}",
                self.label,
                self.send_loc(),
                self.val(),
                if self.is_dropped() { " [dropped]" } else { "" },
            )
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Unique {
    label: EventLabel,
    // This is only stored to be validated during replay
    comm: CommunicationModel,
}

impl Unique {
    pub(crate) fn new(pos: Event, comm: CommunicationModel) -> Self {
        Self {
            label: EventLabel::new(pos),
            comm,
        }
    }

    pub(crate) fn get_loc(&self) -> Loc {
        Loc::new(self.label.pos())
    }
}

as_label!(Unique);

impl fmt::Display for Unique {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: UNIQUE [{:?}]", self.as_event_label(), self.comm,)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct CToss {
    label: EventLabel,
    result: bool,
}

impl CToss {
    pub(crate) fn new(pos: Event) -> Self {
        Self {
            label: EventLabel::new(pos),
            result: Self::maximal(),
        }
    }

    pub(crate) fn result(&self) -> bool {
        self.result
    }

    pub(crate) fn set_result(&mut self, result: bool) {
        self.result = result
    }

    pub(crate) fn maximal() -> bool {
        false
    }
}

as_label!(CToss);

impl fmt::Display for CToss {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: NONDET {}", self.as_event_label(), self.result())
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Choice {
    label: EventLabel,
    range: RangeInclusive<usize>,
    result: usize,
}

impl Choice {
    pub(crate) fn new(pos: Event, range: &mut RangeInclusive<usize>) -> Self {
        let start = range.start();
        Self {
            label: EventLabel::new(pos),
            range: range.clone(),
            result: *start,
        }
    }

    pub(crate) fn range(&self) -> &RangeInclusive<usize> {
        &self.range
    }

    pub(crate) fn result(&self) -> usize {
        self.result
    }

    pub(crate) fn set_result(&mut self, result: usize) {
        assert!(self.range.contains(&result));
        self.result = result
    }
}

as_label!(Choice);

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: CHOOSE {}[{}-{})",
            self.as_event_label(),
            self.result(),
            self.range().start(),
            self.range().end()
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Sample {
    label: EventLabel,
    current: serde_json::Value,
    samples: Vec<serde_json::Value>,
}

impl Sample {
    pub(crate) fn new(
        pos: Event,
        current: serde_json::Value,
        samples: Vec<serde_json::Value>,
    ) -> Self {
        Self {
            label: EventLabel::new(pos),
            current,
            samples,
        }
    }

    pub(crate) fn current(&self) -> &serde_json::Value {
        &self.current
    }

    pub(crate) fn next(&mut self) -> bool {
        let v = self.samples.pop();
        match v {
            None => false,
            Some(val) => {
                self.current = val;
                true
            }
        }
    }
}

as_label!(Sample);

impl fmt::Display for Sample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: SAMPLE {:?}", self.as_event_label(), self.current())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum BlockType {
    // User-level blocking
    Assume,
    Assert,
    // Internal blocking
    Value(RecvLoc),
    Join(ThreadId),
}

// Block events are used in two different ways:
// - In case of Assume/Assert, they represent a user-level blocking,
// . where an instruction was executed that effectively stops the thread execution.
// . The instruction pointer points to the Assume/Assert instruction.
// - In case of Value/Join, they represent an internal blocking,
// . where the thread should stop executing because the corresponding Send/End
// . dependency of the Receive/Join event is not present in the execution.
// . The instruction pointer points to the previous instruction
// . . (the Block event is removed when the dependency is present in the graph, see must::next_task)
// . N.B. This is different from TaskState::Stuck, where the dependency is present but
// . . has not executed yet, in which case *no* Block event is added.

// TODO: Consider refactoring so that the above distrinction is explicit.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Block {
    label: EventLabel,
    btype: BlockType,
}

impl Block {
    pub(crate) fn new(pos: Event, t: BlockType) -> Self {
        Self {
            label: EventLabel::new(pos),
            btype: t,
        }
    }

    pub(crate) fn btype(&self) -> &BlockType {
        &self.btype
    }
}

as_label!(Block);

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: BLK {:?}", self.as_event_label(), self.btype())
    }
}
