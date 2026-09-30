//! Buffers, replays, broadcasts, and atomically closes downstream output.

use bytes::Bytes;
use tokio::sync::mpsc;

#[derive(Default)]
pub struct OutputHub {
    state: parking_lot::Mutex<OutputState>,
    delivery: parking_lot::Mutex<()>,
}

#[derive(Default)]
struct OutputState {
    history: Vec<Bytes>,
    subscribers: Vec<mpsc::UnboundedSender<Bytes>>,
    closed: bool,
}

impl OutputHub {
    pub fn emit(&self, frame: Bytes) -> bool {
        let _delivery = self.delivery.lock();
        let mut state = self.state.lock();
        if state.closed {
            return false;
        }
        state.history.push(frame.clone());
        state
            .subscribers
            .retain(|subscriber| subscriber.send(frame.clone()).is_ok());
        true
    }

    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<Bytes> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let _delivery = self.delivery.lock();
        let (history, closed) = {
            let mut state = self.state.lock();
            let history = state.history.clone();
            let closed = state.closed;
            if !closed {
                state.subscribers.push(sender.clone());
            }
            (history, closed)
        };
        for frame in history {
            let _ = sender.send(frame);
        }
        if closed {
            drop(sender);
        }
        receiver
    }

    pub fn close(&self) -> bool {
        let _delivery = self.delivery.lock();
        let mut state = self.state.lock();
        if state.closed {
            return false;
        }
        state.closed = true;
        state.subscribers.clear();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_replays_existing_frames_and_receives_new_frames_in_order() {
        let hub = OutputHub::default();
        assert!(hub.emit(Bytes::from_static(b"first")));
        let mut receiver = hub.subscribe();
        assert!(hub.emit(Bytes::from_static(b"second")));
        assert_eq!(receiver.try_recv().unwrap(), Bytes::from_static(b"first"));
        assert_eq!(receiver.try_recv().unwrap(), Bytes::from_static(b"second"));
    }

    #[test]
    fn closed_hub_does_not_accept_emits_and_replays_history_then_closes() {
        let hub = OutputHub::default();
        assert!(hub.emit(Bytes::from_static(b"first")));
        assert!(hub.close());
        assert!(!hub.close()); // Idempotent
        assert!(!hub.emit(Bytes::from_static(b"second")));

        let mut receiver = hub.subscribe();
        assert_eq!(receiver.try_recv().unwrap(), Bytes::from_static(b"first"));
        // Since hub was closed, subscriber was not registered in subscribers list,
        // and sender was dropped at end of subscribe(), so channel is disconnected.
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn closing_hub_disconnects_active_subscribers() {
        let hub = OutputHub::default();
        let mut receiver = hub.subscribe();
        assert!(hub.emit(Bytes::from_static(b"event-1")));
        assert_eq!(receiver.try_recv().unwrap(), Bytes::from_static(b"event-1"));

        assert!(hub.close());
        // After close, all sender handles in subscribers are cleared/dropped,
        // so receiver will see channel closed.
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn dead_subscribers_are_pruned_on_emit() {
        let hub = OutputHub::default();
        let receiver = hub.subscribe();
        drop(receiver); // subscriber is now dead

        let mut live_receiver = hub.subscribe();
        assert!(hub.emit(Bytes::from_static(b"hello")));

        // Hub state should have pruned the dead subscriber
        let state = hub.state.lock();
        assert_eq!(state.subscribers.len(), 1);
        drop(state);

        assert_eq!(live_receiver.try_recv().unwrap(), Bytes::from_static(b"hello"));
    }
}
