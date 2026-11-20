//! PPoGATT (section 4): a windowed, acknowledged byte stream over GATT writes and notifications,
//! as a state machine. Packets and ticks go in; writes, delivered bytes, and state changes come
//! out. Time is passed in so tests can drive it.

use crate::wire::{err, Result};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const SEQUENCE_MOD: u8 = 32;
pub const MAX_WINDOW: u8 = 25;
pub const DEFAULT_MTU: usize = 23;
/// ATT header (3) and PPoGATT header (1).
pub const DATA_OVERHEAD: usize = 4;
const V0_WINDOW: u8 = 4;
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(12);
pub const ACK_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_RESENDS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketKind {
    Data = 0,
    Ack = 1,
    ResetRequest = 2,
    ResetComplete = 3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    Data {
        sequence: u8,
        data: Vec<u8>,
    },
    Ack {
        sequence: u8,
    },
    ResetRequest {
        version: u8,
    },
    /// Windows as the sender states them: what it can receive, then what it will send.
    ResetComplete {
        rx_window: u8,
        tx_window: u8,
    },
}

fn header(kind: PacketKind, sequence: u8) -> u8 {
    ((sequence % SEQUENCE_MOD) << 3) | kind as u8
}

impl Packet {
    pub fn encode(&self, version: u8) -> Vec<u8> {
        match self {
            Packet::Data { sequence, data } => {
                let mut out = Vec::with_capacity(1 + data.len());
                out.push(header(PacketKind::Data, *sequence));
                out.extend_from_slice(data);
                out
            }
            Packet::Ack { sequence } => vec![header(PacketKind::Ack, *sequence)],
            Packet::ResetRequest { version } => vec![header(PacketKind::ResetRequest, 0), *version],
            Packet::ResetComplete { rx_window, tx_window } => {
                if version >= 1 {
                    vec![header(PacketKind::ResetComplete, 0), *rx_window, *tx_window]
                } else {
                    vec![header(PacketKind::ResetComplete, 0)]
                }
            }
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Packet> {
        let Some(&first) = bytes.first() else {
            return err("empty PPoGATT packet");
        };
        let sequence = first >> 3;
        Ok(match first & 0b111 {
            0 => Packet::Data { sequence, data: bytes[1..].to_vec() },
            1 => Packet::Ack { sequence },
            2 => Packet::ResetRequest { version: bytes.get(1).copied().unwrap_or(0) },
            3 if bytes.len() >= 3 => Packet::ResetComplete { rx_window: bytes[1], tx_window: bytes[2] },
            3 => Packet::ResetComplete { rx_window: V0_WINDOW, tx_window: V0_WINDOW },
            other => return err(format!("unknown PPoGATT packet type {other}")),
        })
    }
}

/// Who starts the session: in forward mode the phone hosts the service and the watch starts; in
/// reversed mode the watch hosts it and the phone starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Forward,
    Reversed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseReason {
    OpenTimeout,
    AckTimeout,
    /// The watch reset an open session; the connection has to start over.
    PeerReset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// One GATT write (reversed) or notification (forward).
    Write(Vec<u8>),
    /// In-order bytes of the Pebble Protocol stream.
    Deliver(Vec<u8>),
    Opened {
        version: u8,
        tx_window: u8,
        rx_window: u8,
    },
    Closed(CloseReason),
}

#[derive(Debug)]
enum State {
    /// Forward: waiting for the watch's reset request.
    AwaitingRequest,
    /// Waiting for the other side's reset complete. `initiated` when this side sent the request
    /// and so confirms with its own reset complete.
    AwaitingComplete {
        initiated: bool,
        version: u8,
    },
    Open(Open),
    Closed,
}

#[derive(Debug)]
struct Open {
    version: u8,
    tx_window: u8,
    tx_sequence: u8,
    rx_expected: u8,
    last_ack_sent: Option<u8>,
    last_ack_received: Option<u8>,
    in_flight: VecDeque<(u8, Vec<u8>)>,
    ack_deadline: Option<Instant>,
    resends: u32,
}

#[derive(Debug)]
pub struct Session {
    state: State,
    opening_deadline: Instant,
    mtu: usize,
    pending: VecDeque<u8>,
}

impl Session {
    /// A new session; in reversed mode the returned actions hold the reset request to write.
    pub fn new(role: Role, now: Instant) -> (Session, Vec<Action>) {
        let mut session =
            Session { state: State::AwaitingRequest, opening_deadline: now + OPEN_TIMEOUT, mtu: DEFAULT_MTU, pending: VecDeque::new() };
        let mut actions = Vec::new();
        if role == Role::Reversed {
            session.state = State::AwaitingComplete { initiated: true, version: 1 };
            actions.push(Action::Write(Packet::ResetRequest { version: 1 }.encode(1)));
        }
        (session, actions)
    }

    pub fn is_open(&self) -> bool {
        matches!(self.state, State::Open(_))
    }

    pub fn is_closed(&self) -> bool {
        matches!(self.state, State::Closed)
    }

    pub fn mtu(&self) -> usize {
        self.mtu
    }

    /// Raises the MTU; a smaller value is ignored, as queued packets assume the larger.
    pub fn set_mtu(&mut self, mtu: usize) {
        if mtu > self.mtu {
            self.mtu = mtu;
        }
    }

    fn max_payload(&self) -> usize {
        self.mtu.saturating_sub(DATA_OVERHEAD).max(1)
    }

    /// The next time `on_tick` has work to do.
    pub fn next_deadline(&self) -> Option<Instant> {
        match &self.state {
            State::AwaitingRequest | State::AwaitingComplete { .. } => Some(self.opening_deadline),
            State::Open(open) => open.ack_deadline,
            State::Closed => None,
        }
    }

    /// Queues Pebble Protocol bytes; they go out once the session is open and the window allows.
    pub fn send(&mut self, bytes: &[u8], now: Instant) -> Vec<Action> {
        if self.is_closed() {
            return Vec::new();
        }
        self.pending.extend(bytes);
        let mut actions = Vec::new();
        self.pump(now, &mut actions);
        actions
    }

    pub fn on_packet(&mut self, bytes: &[u8], now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();
        let Ok(packet) = Packet::decode(bytes) else {
            return actions;
        };
        match (&mut self.state, packet) {
            (State::Closed, _) => {}
            (State::AwaitingRequest, Packet::ResetRequest { version }) => {
                self.answer_request(version, &mut actions);
            }
            (State::AwaitingComplete { initiated: true, .. }, Packet::ResetRequest { version }) => {
                // The watch gave up on this side's request and started its own.
                self.answer_request(version, &mut actions);
            }
            (State::AwaitingComplete { initiated, version }, Packet::ResetComplete { rx_window, tx_window }) => {
                let (initiated, version) = (*initiated, *version);
                if initiated {
                    actions.push(Action::Write(Packet::ResetComplete { rx_window: MAX_WINDOW, tx_window: MAX_WINDOW }.encode(version)));
                }
                self.open(version, rx_window, tx_window, &mut actions);
                self.pump(now, &mut actions);
            }
            (State::AwaitingRequest | State::AwaitingComplete { .. }, _) => {}
            (State::Open(_), Packet::ResetRequest { .. } | Packet::ResetComplete { .. }) => {
                self.close(CloseReason::PeerReset, &mut actions);
            }
            (State::Open(open), Packet::Data { sequence, data }) => {
                if sequence == open.rx_expected {
                    open.rx_expected = (open.rx_expected + 1) % SEQUENCE_MOD;
                    open.last_ack_sent = Some(sequence);
                    actions.push(Action::Write(Packet::Ack { sequence }.encode(open.version)));
                    actions.push(Action::Deliver(data));
                } else if let Some(last) = open.last_ack_sent {
                    actions.push(Action::Write(Packet::Ack { sequence: last }.encode(open.version)));
                }
            }
            (State::Open(open), Packet::Ack { sequence }) => {
                if let Some(index) = open.in_flight.iter().position(|(s, _)| *s == sequence) {
                    open.in_flight.drain(..=index);
                    open.resends = 0;
                    open.ack_deadline = if open.in_flight.is_empty() { None } else { Some(now + ACK_TIMEOUT) };
                } else if open.last_ack_received == Some(sequence) && !open.in_flight.is_empty() {
                    // A repeated ack: the watch lost what followed it.
                    for (_, packet) in &open.in_flight {
                        actions.push(Action::Write(packet.clone()));
                    }
                    open.ack_deadline = Some(now + ACK_TIMEOUT);
                }
                open.last_ack_received = Some(sequence);
                self.pump(now, &mut actions);
            }
        }
        actions
    }

    pub fn on_tick(&mut self, now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();
        match &mut self.state {
            State::AwaitingRequest | State::AwaitingComplete { .. } if now >= self.opening_deadline => {
                self.close(CloseReason::OpenTimeout, &mut actions);
            }
            State::Open(open) if open.ack_deadline.is_some_and(|d| now >= d) => {
                open.resends += 1;
                if open.resends > MAX_RESENDS {
                    self.close(CloseReason::AckTimeout, &mut actions);
                } else {
                    for (_, packet) in &open.in_flight {
                        actions.push(Action::Write(packet.clone()));
                    }
                    open.ack_deadline = Some(now + ACK_TIMEOUT);
                }
            }
            _ => {}
        }
        actions
    }

    fn answer_request(&mut self, version: u8, actions: &mut Vec<Action>) {
        let version = version.min(1);
        actions.push(Action::Write(Packet::ResetComplete { rx_window: MAX_WINDOW, tx_window: MAX_WINDOW }.encode(version)));
        self.state = State::AwaitingComplete { initiated: false, version };
    }

    fn open(&mut self, version: u8, peer_rx: u8, peer_tx: u8, actions: &mut Vec<Action>) {
        let tx_window = peer_rx.clamp(1, MAX_WINDOW);
        let rx_window = peer_tx.clamp(1, MAX_WINDOW);
        self.state = State::Open(Open {
            version,
            tx_window,
            tx_sequence: 0,
            rx_expected: 0,
            last_ack_sent: None,
            last_ack_received: None,
            in_flight: VecDeque::new(),
            ack_deadline: None,
            resends: 0,
        });
        actions.push(Action::Opened { version, tx_window, rx_window });
    }

    fn close(&mut self, reason: CloseReason, actions: &mut Vec<Action>) {
        self.state = State::Closed;
        self.pending.clear();
        actions.push(Action::Closed(reason));
    }

    fn pump(&mut self, now: Instant, actions: &mut Vec<Action>) {
        let max = self.max_payload();
        let State::Open(open) = &mut self.state else {
            return;
        };
        while !self.pending.is_empty() && open.in_flight.len() < open.tx_window as usize {
            let take = max.min(self.pending.len());
            let data: Vec<u8> = self.pending.drain(..take).collect();
            let sequence = open.tx_sequence;
            open.tx_sequence = (open.tx_sequence + 1) % SEQUENCE_MOD;
            let packet = Packet::Data { sequence, data }.encode(open.version);
            actions.push(Action::Write(packet.clone()));
            open.in_flight.push_back((sequence, packet));
            if open.ack_deadline.is_none() {
                open.ack_deadline = Some(now + ACK_TIMEOUT);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writes(actions: &[Action]) -> Vec<Packet> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Write(b) => Some(Packet::decode(b).unwrap()),
                _ => None,
            })
            .collect()
    }

    fn delivered(actions: &[Action]) -> Vec<u8> {
        actions
            .iter()
            .flat_map(|a| match a {
                Action::Deliver(b) => b.clone(),
                _ => Vec::new(),
            })
            .collect()
    }

    fn open_forward(now: Instant, watch_rx: u8) -> Session {
        let (mut s, actions) = Session::new(Role::Forward, now);
        assert!(actions.is_empty());
        let a = s.on_packet(&Packet::ResetRequest { version: 1 }.encode(1), now);
        assert_eq!(writes(&a), vec![Packet::ResetComplete { rx_window: 25, tx_window: 25 }]);
        let a = s.on_packet(&Packet::ResetComplete { rx_window: watch_rx, tx_window: 10 }.encode(1), now);
        assert!(writes(&a).is_empty(), "the responder does not confirm");
        assert_eq!(a, vec![Action::Opened { version: 1, tx_window: watch_rx.min(25), rx_window: 10 }]);
        s
    }

    #[test]
    fn header_layout() {
        assert_eq!(Packet::Data { sequence: 31, data: vec![9] }.encode(1), vec![0b1111_1000, 9]);
        assert_eq!(Packet::Ack { sequence: 5 }.encode(1), vec![(5 << 3) | 1]);
        assert_eq!(Packet::ResetRequest { version: 1 }.encode(1), vec![2, 1]);
        assert_eq!(Packet::ResetComplete { rx_window: 25, tx_window: 25 }.encode(1), vec![3, 25, 25]);
        assert_eq!(Packet::ResetComplete { rx_window: 25, tx_window: 25 }.encode(0), vec![3]);
        assert_eq!(Packet::decode(&[3]).unwrap(), Packet::ResetComplete { rx_window: 4, tx_window: 4 });
    }

    #[test]
    fn reversed_open_with_confirmation() {
        let now = Instant::now();
        let (mut s, actions) = Session::new(Role::Reversed, now);
        assert_eq!(writes(&actions), vec![Packet::ResetRequest { version: 1 }]);
        let a = s.on_packet(&Packet::ResetComplete { rx_window: 8, tx_window: 6 }.encode(1), now);
        assert_eq!(writes(&a), vec![Packet::ResetComplete { rx_window: 25, tx_window: 25 }]);
        assert!(a.contains(&Action::Opened { version: 1, tx_window: 8, rx_window: 6 }));
    }

    #[test]
    fn reversed_answers_the_watchs_own_reset() {
        let now = Instant::now();
        let (mut s, _) = Session::new(Role::Reversed, now);
        let a = s.on_packet(&Packet::ResetRequest { version: 1 }.encode(1), now);
        assert_eq!(writes(&a), vec![Packet::ResetComplete { rx_window: 25, tx_window: 25 }]);
        let a = s.on_packet(&Packet::ResetComplete { rx_window: 25, tx_window: 25 }.encode(1), now);
        assert!(writes(&a).is_empty());
        assert!(s.is_open());
    }

    #[test]
    fn opening_times_out() {
        let now = Instant::now();
        let (mut s, _) = Session::new(Role::Forward, now);
        assert!(s.on_tick(now + Duration::from_secs(11)).is_empty());
        assert_eq!(s.on_tick(now + OPEN_TIMEOUT), vec![Action::Closed(CloseReason::OpenTimeout)]);
    }

    #[test]
    fn data_is_chunked_by_mtu_and_limited_by_window() {
        let now = Instant::now();
        let mut s = open_forward(now, 2);
        let a = s.send(&[7; 50], now);
        let w = writes(&a);
        assert_eq!(w.len(), 2, "window of 2");
        assert_eq!(w[0], Packet::Data { sequence: 0, data: vec![7; 19] });
        let a = s.on_packet(&Packet::Ack { sequence: 1 }.encode(1), now);
        let w = writes(&a);
        assert_eq!(w, vec![Packet::Data { sequence: 2, data: vec![7; 12] }]);
    }

    #[test]
    fn larger_mtu_gives_larger_packets() {
        let now = Instant::now();
        let mut s = open_forward(now, 25);
        s.set_mtu(339);
        s.set_mtu(100);
        let w = writes(&s.send(&[1; 400], now));
        assert_eq!(w.len(), 2);
        assert!(matches!(&w[0], Packet::Data { data, .. } if data.len() == 335));
    }

    #[test]
    fn in_order_data_is_delivered_and_acked() {
        let now = Instant::now();
        let mut s = open_forward(now, 25);
        let a = s.on_packet(&Packet::Data { sequence: 0, data: vec![1, 2] }.encode(1), now);
        assert_eq!(delivered(&a), vec![1, 2]);
        assert_eq!(writes(&a), vec![Packet::Ack { sequence: 0 }]);
        let a = s.on_packet(&Packet::Data { sequence: 2, data: vec![9] }.encode(1), now);
        assert!(delivered(&a).is_empty());
        assert_eq!(writes(&a), vec![Packet::Ack { sequence: 0 }], "out of order repeats the last ack");
        let a = s.on_packet(&Packet::Data { sequence: 1, data: vec![3] }.encode(1), now);
        assert_eq!(delivered(&a), vec![3]);
    }

    #[test]
    fn sequence_numbers_wrap() {
        let now = Instant::now();
        let mut s = open_forward(now, 25);
        for i in 0..40u32 {
            let seq = (i % 32) as u8;
            let a = s.on_packet(&Packet::Data { sequence: seq, data: vec![i as u8] }.encode(1), now);
            assert_eq!(delivered(&a), vec![i as u8]);
            let a = s.send(&[i as u8], now);
            assert_eq!(writes(&a), vec![Packet::Data { sequence: seq, data: vec![i as u8] }]);
            s.on_packet(&Packet::Ack { sequence: seq }.encode(1), now);
        }
    }

    #[test]
    fn unacked_packets_are_resent_then_the_session_closes() {
        let now = Instant::now();
        let mut s = open_forward(now, 25);
        s.send(&[5; 30], now);
        let mut t = now;
        for _ in 0..MAX_RESENDS {
            t += ACK_TIMEOUT;
            let w = writes(&s.on_tick(t));
            assert_eq!(w.len(), 2);
        }
        t += ACK_TIMEOUT;
        assert_eq!(s.on_tick(t), vec![Action::Closed(CloseReason::AckTimeout)]);
    }

    #[test]
    fn a_repeated_ack_resends_what_is_in_flight() {
        let now = Instant::now();
        let mut s = open_forward(now, 25);
        s.send(&[5; 19 * 3], now);
        s.on_packet(&Packet::Ack { sequence: 0 }.encode(1), now);
        let w = writes(&s.on_packet(&Packet::Ack { sequence: 0 }.encode(1), now));
        assert_eq!(w.iter().filter(|p| matches!(p, Packet::Data { .. })).count(), 2);
    }

    #[test]
    fn reset_inside_a_session_closes_it() {
        let now = Instant::now();
        let mut s = open_forward(now, 25);
        let a = s.on_packet(&Packet::ResetRequest { version: 1 }.encode(1), now);
        assert_eq!(a, vec![Action::Closed(CloseReason::PeerReset)]);
        assert!(s.send(&[1], now).is_empty());
    }

    #[test]
    fn data_queued_before_opening_goes_out_on_open() {
        let now = Instant::now();
        let (mut s, _) = Session::new(Role::Reversed, now);
        assert!(s.send(&[1, 2, 3], now).is_empty());
        let a = s.on_packet(&Packet::ResetComplete { rx_window: 25, tx_window: 25 }.encode(1), now);
        assert!(writes(&a).contains(&Packet::Data { sequence: 0, data: vec![1, 2, 3] }));
    }
}
