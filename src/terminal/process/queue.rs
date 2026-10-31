//! Bounded input buffering with reserved space for terminal replies. Neither the GUI
//! nor the output reader may wait for a program to consume its input.
use std::{
    collections::VecDeque,
    sync::{Condvar, Mutex},
};

const PIECES: usize = 256;
const INPUT_BYTES: usize = 4 * 1024 * 1024;
const REPLY_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct Budget {
    pieces: usize,
    bytes: usize,
}

#[derive(Default)]
struct State {
    pending: VecDeque<(Vec<u8>, bool)>,
    input: Budget,
    replies: Budget,
    closed: bool,
}

#[derive(Default)]
pub(super) struct InputQueue {
    state: Mutex<State>,
    ready: Condvar,
}

impl InputQueue {
    pub fn send(&self, bytes: &[u8], reply: bool) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Err("the program no longer takes input".into());
        }
        let (budget, limit) = if reply {
            (&mut state.replies, REPLY_BYTES)
        } else {
            (&mut state.input, INPUT_BYTES)
        };
        if budget.pieces == PIECES || bytes.len() > limit.saturating_sub(budget.bytes) {
            return Err(if reply {
                "terminal replies are full; a reply was not sent"
            } else {
                "input is full or this paste is too large; it was not sent"
            }
            .into());
        }
        budget.pieces += 1;
        budget.bytes += bytes.len();
        state.pending.push_back((bytes.to_vec(), reply));
        self.ready.notify_one();
        Ok(())
    }

    pub fn recv(&self) -> Option<Vec<u8>> {
        let mut state = self.state.lock().unwrap();
        while !state.closed && state.pending.is_empty() {
            state = self.ready.wait(state).unwrap();
        }
        if state.closed {
            return None;
        }
        let (bytes, reply) = state.pending.pop_front().unwrap();
        let budget = if reply {
            &mut state.replies
        } else {
            &mut state.input
        };
        budget.pieces -= 1;
        budget.bytes -= bytes.len();
        Some(bytes)
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }

    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.pending.clear();
        state.input = Budget::default();
        state.replies = Budget::default();
        self.ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_have_reserved_space_and_input_is_bounded_by_bytes() {
        let queue = InputQueue::default();
        for _ in 0..PIECES {
            queue.send(b"key", false).unwrap();
        }
        assert!(queue.send(b"another", false).is_err());
        queue.send(b"reply", true).unwrap();
        queue.close();
        assert!(queue.recv().is_none());
        let queue = InputQueue::default();
        queue.send(&vec![0; INPUT_BYTES], false).unwrap();
        assert!(queue.send(b"x", false).is_err());
        queue.send(b"reply", true).unwrap();
        assert_eq!(queue.recv().unwrap().len(), INPUT_BYTES);
        queue.send(b"x", false).unwrap();
    }
}
