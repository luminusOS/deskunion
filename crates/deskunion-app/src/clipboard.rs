use deskunion_proto::{
    ClipboardTextFragment, MAX_CLIPBOARD_FRAGMENT_SIZE, MAX_CLIPBOARD_TEXT_SIZE,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const TRANSFER_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_IN_FLIGHT_TRANSFERS: usize = 4;
const MAX_RECENT_TRANSFERS: usize = 32;

pub(crate) struct PendingClipboardSend {
    pub(crate) addr: std::net::SocketAddr,
    pub(crate) transfer_id: u32,
    pub(crate) text: String,
}

/// One-worker latest-value queue. Clipboard changes coalesce during a transfer,
/// keeping both task count and retained payload bounded.
#[derive(Default)]
pub(crate) struct ClipboardSendQueue {
    pending: Option<PendingClipboardSend>,
    running: bool,
    generation: u64,
    active_ack: Option<(std::net::SocketAddr, u32, bool)>,
    ack_notify: std::sync::Arc<tokio::sync::Notify>,
}

impl ClipboardSendQueue {
    pub(crate) fn enqueue(&mut self, send: PendingClipboardSend) -> bool {
        self.pending = Some(send);
        if self.running {
            false
        } else {
            self.running = true;
            true
        }
    }

    pub(crate) fn take(&mut self) -> Option<PendingClipboardSend> {
        self.pending.take()
    }

    pub(crate) fn finish_or_continue(&mut self) -> bool {
        self.running = false;
        if self.pending.is_some() {
            self.running = true;
            true
        } else {
            false
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        self.active_ack = None;
        self.ack_notify.notify_waiters();
    }

    pub(crate) fn begin_ack(&mut self, addr: std::net::SocketAddr, transfer_id: u32) {
        self.active_ack = Some((addr, transfer_id, false));
    }

    pub(crate) fn acknowledge(&mut self, addr: std::net::SocketAddr, transfer_id: u32) {
        if let Some((active_addr, active_id, received)) = self.active_ack.as_mut()
            && *active_addr == addr
            && *active_id == transfer_id
        {
            *received = true;
            self.ack_notify.notify_one();
        }
    }

    pub(crate) fn ack_received(&self, addr: std::net::SocketAddr, transfer_id: u32) -> bool {
        self.active_ack
            .is_some_and(|(active_addr, active_id, received)| {
                active_addr == addr && active_id == transfer_id && received
            })
    }

    pub(crate) fn finish_ack(&mut self, addr: std::net::SocketAddr, transfer_id: u32) {
        if self.active_ack.is_some_and(|(active_addr, active_id, _)| {
            active_addr == addr && active_id == transfer_id
        }) {
            self.active_ack = None;
        }
    }

    pub(crate) fn ack_notifier(&self) -> std::sync::Arc<tokio::sync::Notify> {
        self.ack_notify.clone()
    }
}

/// Reassembles one bounded clipboard transfer. Per-session instance prevents
/// fragments from different peers from being combined.
#[derive(Default)]
pub(crate) struct ClipboardTextAssembler {
    pending: HashMap<u32, PendingTransfer>,
    completed: HashMap<u32, Instant>,
}

struct PendingTransfer {
    count: u16,
    fragments: Vec<Option<Vec<u8>>>,
    bytes: usize,
    started: Instant,
}

impl ClipboardTextAssembler {
    pub(crate) fn reset(&mut self) {
        self.pending.clear();
        self.completed.clear();
    }

    pub(crate) fn is_completed(&self, transfer_id: u32) -> bool {
        self.completed.contains_key(&transfer_id)
    }

    pub(crate) fn push(
        &mut self,
        fragment: &ClipboardTextFragment,
        payload: &[u8],
    ) -> Option<String> {
        let max_count = MAX_CLIPBOARD_TEXT_SIZE.div_ceil(MAX_CLIPBOARD_FRAGMENT_SIZE) as u16;
        if fragment.count == 0
            || fragment.count > max_count
            || fragment.index >= fragment.count
            || payload.len() > MAX_CLIPBOARD_FRAGMENT_SIZE
        {
            self.pending.remove(&fragment.transfer_id);
            return None;
        }

        self.pending
            .retain(|_, pending| pending.started.elapsed() <= TRANSFER_TIMEOUT);
        self.completed
            .retain(|_, completed| completed.elapsed() <= TRANSFER_TIMEOUT);
        if self.completed.contains_key(&fragment.transfer_id) {
            return None;
        }
        if !self.pending.contains_key(&fragment.transfer_id) {
            if self.pending.len() >= MAX_IN_FLIGHT_TRANSFERS
                && let Some(oldest_id) = self
                    .pending
                    .iter()
                    .min_by_key(|(_, pending)| pending.started)
                    .map(|(id, _)| *id)
            {
                self.pending.remove(&oldest_id);
            }
            self.pending.insert(
                fragment.transfer_id,
                PendingTransfer {
                    count: fragment.count,
                    fragments: vec![None; fragment.count as usize],
                    bytes: 0,
                    started: Instant::now(),
                },
            );
        }

        let pending = self.pending.get_mut(&fragment.transfer_id)?;
        if pending.count != fragment.count {
            self.pending.remove(&fragment.transfer_id);
            return None;
        }

        let slot = &mut pending.fragments[fragment.index as usize];
        if let Some(existing) = slot {
            if existing != payload {
                self.pending.remove(&fragment.transfer_id);
            }
            return None;
        }
        if pending.bytes.saturating_add(payload.len()) > MAX_CLIPBOARD_TEXT_SIZE {
            self.pending.remove(&fragment.transfer_id);
            return None;
        }
        pending.bytes += payload.len();
        *slot = Some(payload.to_vec());
        if pending.fragments.iter().any(Option::is_none) {
            return None;
        }

        let pending = self.pending.remove(&fragment.transfer_id)?;
        let mut text = Vec::with_capacity(pending.bytes);
        for part in pending.fragments.into_iter().flatten() {
            text.extend_from_slice(&part);
        }
        let text = String::from_utf8(text)
            .ok()
            .filter(|text| !text.contains('\0'))?;
        self.completed.insert(fragment.transfer_id, Instant::now());
        if self.completed.len() > MAX_RECENT_TRANSFERS
            && let Some(oldest_id) = self
                .completed
                .iter()
                .min_by_key(|(_, completed)| **completed)
                .map(|(id, _)| *id)
        {
            self.completed.remove(&oldest_id);
        }
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fragment(transfer_id: u32, index: u16, count: u16) -> ClipboardTextFragment {
        ClipboardTextFragment {
            transfer_id,
            index,
            count,
            payload_range: 0..0,
        }
    }

    #[test]
    fn assembles_out_of_order_utf8_fragments() {
        let mut assembler = ClipboardTextAssembler::default();
        assert_eq!(assembler.push(&fragment(7, 1, 2), "🦀".as_bytes()), None);
        assert_eq!(
            assembler.push(&fragment(7, 0, 2), "hello ".as_bytes()),
            Some("hello 🦀".to_owned())
        );
    }

    #[test]
    fn rejects_invalid_utf8_and_oversized_transfers() {
        let mut assembler = ClipboardTextAssembler::default();
        assert_eq!(assembler.push(&fragment(1, 0, 1), &[0xff]), None);
        assert_eq!(assembler.push(&fragment(4, 0, 1), b"before\0after"), None);
        assert!(!assembler.is_completed(1));
        assert!(!assembler.is_completed(4));
        assert_eq!(
            assembler.push(&fragment(2, 0, 1), &vec![b'x'; MAX_CLIPBOARD_TEXT_SIZE + 1]),
            None
        );
    }

    #[test]
    fn duplicate_fragment_does_not_complete_or_duplicate_text() {
        let mut assembler = ClipboardTextAssembler::default();
        let part = fragment(3, 0, 2);
        assert_eq!(assembler.push(&part, b"one"), None);
        assert_eq!(assembler.push(&part, b"one"), None);
        assert_eq!(
            assembler.push(&fragment(3, 1, 2), b"two"),
            Some("onetwo".into())
        );
    }

    #[test]
    fn interleaved_transfers_reassemble_independently_and_suppress_replays() {
        let mut assembler = ClipboardTextAssembler::default();
        assert_eq!(assembler.push(&fragment(10, 0, 2), b"old-"), None);
        assert_eq!(assembler.push(&fragment(11, 0, 2), b"new-"), None);
        assert_eq!(
            assembler.push(&fragment(10, 1, 2), b"text"),
            Some("old-text".into())
        );
        assert_eq!(
            assembler.push(&fragment(11, 1, 2), b"text"),
            Some("new-text".into())
        );
        assert!(assembler.is_completed(11));
        assert_eq!(assembler.push(&fragment(11, 0, 2), b"new-"), None);
    }

    #[test]
    fn send_queue_coalesces_changes_and_keeps_one_worker() {
        let addr = "127.0.0.1:4242".parse().expect("socket address");
        let mut queue = ClipboardSendQueue::default();
        assert!(queue.enqueue(PendingClipboardSend {
            addr,
            transfer_id: 1,
            text: "first".into(),
        }));
        assert_eq!(queue.take().expect("first item").text, "first");
        assert!(!queue.enqueue(PendingClipboardSend {
            addr,
            transfer_id: 2,
            text: "second".into(),
        }));
        assert!(!queue.enqueue(PendingClipboardSend {
            addr,
            transfer_id: 3,
            text: "latest".into(),
        }));
        assert!(queue.finish_or_continue());
        assert_eq!(queue.take().expect("coalesced item").text, "latest");
        assert!(!queue.finish_or_continue());
    }

    #[test]
    fn send_queue_accepts_ack_only_for_active_transfer_and_peer() {
        let addr = "127.0.0.1:4242".parse().expect("socket address");
        let other = "127.0.0.1:4243".parse().expect("socket address");
        let mut queue = ClipboardSendQueue::default();
        queue.begin_ack(addr, 9);
        queue.acknowledge(other, 9);
        queue.acknowledge(addr, 8);
        assert!(!queue.ack_received(addr, 9));
        queue.acknowledge(addr, 9);
        assert!(queue.ack_received(addr, 9));
        queue.finish_ack(addr, 9);
        assert!(!queue.ack_received(addr, 9));
    }
}
