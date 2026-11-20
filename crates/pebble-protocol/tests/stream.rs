//! Drives a PPoGATT session against a simulated watch that follows the watch side described in
//! PROTOCOL.md section 4, over a link that can drop packets.

use pebble_protocol::frame::{encode, Reassembler};
use pebble_protocol::ppog::{Action, Packet, Role, Session, ACK_TIMEOUT};
use pebble_protocol::{endpoint, system};
use std::collections::VecDeque;
use std::time::Instant;

/// The watch's half: acknowledges in-order data, re-acks otherwise, sends with a window of 4.
struct FakeWatch {
    rx_expected: u8,
    last_ack: Option<u8>,
    tx_sequence: u8,
    received: Reassembler,
    to_phone: VecDeque<Vec<u8>>,
    open: bool,
}

impl FakeWatch {
    fn new() -> Self {
        FakeWatch { rx_expected: 0, last_ack: None, tx_sequence: 0, received: Reassembler::new(), to_phone: VecDeque::new(), open: false }
    }

    fn receive(&mut self, bytes: &[u8]) {
        match Packet::decode(bytes).unwrap() {
            Packet::ResetRequest { .. } => {
                self.to_phone.push_back(Packet::ResetComplete { rx_window: 4, tx_window: 4 }.encode(1));
            }
            Packet::ResetComplete { .. } => self.open = true,
            Packet::Data { sequence, data } => {
                if sequence == self.rx_expected {
                    self.rx_expected = (self.rx_expected + 1) % 32;
                    self.last_ack = Some(sequence);
                    self.received.push(&data);
                    self.to_phone.push_back(Packet::Ack { sequence }.encode(1));
                } else if let Some(last) = self.last_ack {
                    self.to_phone.push_back(Packet::Ack { sequence: last }.encode(1));
                }
            }
            Packet::Ack { .. } => {}
        }
    }

    fn send(&mut self, frame: &[u8]) {
        for chunk in frame.chunks(19) {
            self.to_phone.push_back(Packet::Data { sequence: self.tx_sequence, data: chunk.to_vec() }.encode(1));
            self.tx_sequence = (self.tx_sequence + 1) % 32;
        }
    }
}

/// Runs phone actions into the watch, dropping the phone's data packets listed in `drop`.
fn pump(
    phone: &mut Session,
    watch: &mut FakeWatch,
    actions: Vec<Action>,
    drop: &mut Vec<usize>,
    sent: &mut usize,
    inbound: &mut Reassembler,
    now: Instant,
) {
    let mut queue: VecDeque<Action> = actions.into();
    loop {
        while let Some(action) = queue.pop_front() {
            match action {
                Action::Write(bytes) => {
                    let is_data = matches!(Packet::decode(&bytes).unwrap(), Packet::Data { .. });
                    if is_data {
                        *sent += 1;
                        if drop.first() == Some(sent) {
                            drop.remove(0);
                            continue;
                        }
                    }
                    watch.receive(&bytes);
                }
                Action::Deliver(bytes) => inbound.push(&bytes),
                Action::Opened { .. } | Action::Closed(_) => {}
            }
        }
        let Some(bytes) = watch.to_phone.pop_front() else { break };
        queue.extend(phone.on_packet(&bytes, now));
    }
}

#[test]
fn reversed_session_carries_frames_both_ways_through_loss() {
    let now = Instant::now();
    let mut watch = FakeWatch::new();
    let mut inbound = Reassembler::new();
    let (mut phone, actions) = Session::new(Role::Reversed, now);
    let (mut drop, mut sent) = (vec![2, 5], 0);
    pump(&mut phone, &mut watch, actions, &mut drop, &mut sent, &mut inbound, now);
    assert!(phone.is_open() && watch.open);

    watch.send(&encode(endpoint::PHONE_VERSION, &[0x00]));
    pump(&mut phone, &mut watch, Vec::new(), &mut drop, &mut sent, &mut inbound, now);
    let frame = inbound.next_frame().unwrap();
    assert_eq!(frame.endpoint, endpoint::PHONE_VERSION);
    assert!(system::is_phone_version_request(&frame.payload));

    let big = encode(endpoint::APP_MESSAGE, &(0..200u8).collect::<Vec<_>>());
    let actions = phone.send(&big, now);
    pump(&mut phone, &mut watch, actions, &mut drop, &mut sent, &mut inbound, now);
    let mut t = now;
    for _ in 0..3 {
        t += ACK_TIMEOUT;
        let actions = phone.on_tick(t);
        pump(&mut phone, &mut watch, actions, &mut drop, &mut sent, &mut inbound, t);
    }
    let got = watch.received.next_frame().unwrap();
    assert_eq!(got.endpoint, endpoint::APP_MESSAGE);
    assert_eq!(got.payload.len(), 200);
    assert!(drop.is_empty(), "both drops happened");
    assert!(!phone.is_closed());
}
