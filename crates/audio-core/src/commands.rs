//! Lock-free SPSC channel for control commands (SPEC-003).
//!
//! The UI thread owns the [`CommandQueue`] producer, the audio thread owns the
//! consumer. Commands travel through an [`rtrb`] ring buffer pre-allocated at
//! start-up, so nothing on the audio path allocates or blocks:
//!
//! ```text
//! UI / agent thread              audio thread
//! ─────────────────              ────────────
//! CommandQueue::try_push  ─rtrb─►  CommandQueue::pop
//! ```
//!
//! `try_push` never blocks: when the ring is full it returns [`PushError`]
//! with the command back, and the caller may drop or retry it. `pop` returns
//! `None` when empty and never allocates.
//!
//! Ownership is split at construction: [`CommandQueue::new`] returns a
//! `(producer, consumer)` pair that can be sent to two different threads. The
//! [`CommandQueue`] value itself is the producer; the consumer handle carries
//! the receiving side (see [`CommandConsumer`]).

use rtrb::{Consumer, Producer, PushError, RingBuffer};

use crate::TransportCommand;

/// Producer side of the transport-command channel.
///
/// Holding a `CommandQueue` lets the control side enqueue
/// [`TransportCommand`]s for the audio thread. Dropping it closes the channel;
/// the consumer then sees an empty queue.
pub struct CommandQueue {
    producer: Producer<TransportCommand>,
}

/// Consumer side of the transport-command channel, owned by the audio thread.
///
/// [`CommandConsumer::pop_next`] is allocation- and lock-free, so it is safe to
/// call from [`crate::Renderer::render`].
pub struct CommandConsumer {
    consumer: Consumer<TransportCommand>,
}

impl CommandQueue {
    /// Splits a fresh channel into a producer and a consumer.
    ///
    /// `capacity` is the number of commands that can be queued before
    /// [`CommandQueue::try_push`] reports [`PushError`]. The buffer is
    /// allocated here, at setup time, never on the audio path.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero: an always-full queue is never useful and
    /// hiding the mistake behind a permanent `Full` would be a silent bug.
    #[must_use]
    pub fn new(capacity: usize) -> (Self, CommandConsumer) {
        assert!(capacity > 0, "command queue capacity must be non-zero");
        let (producer, consumer) = RingBuffer::new(capacity);
        (Self { producer }, CommandConsumer { consumer })
    }

    /// Enqueues a command without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`PushError`] (carrying the command back) if the queue is full.
    /// The caller decides whether to retry or drop it; the audio side is never
    /// blocked or panicked.
    pub fn try_push(
        &mut self,
        command: TransportCommand,
    ) -> Result<(), PushError<TransportCommand>> {
        self.producer.push(command)
    }

    /// Number of commands currently waiting to be consumed.
    ///
    /// Uses the producer's free-slot count, so it is the control-side view of
    /// the queue depth.
    #[must_use]
    pub fn len(&self) -> usize {
        self.producer.buffer().capacity() - self.producer.slots()
    }

    /// Whether the queue currently holds no commands.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.producer.slots() == self.producer.buffer().capacity()
    }

    /// Capacity this channel was created with.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.producer.buffer().capacity()
    }
}

impl CommandConsumer {
    /// Pops the next command, or `None` if the queue is empty.
    ///
    /// Allocation-free and lock-free: safe to call on the audio thread.
    pub fn pop_next(&mut self) -> Option<TransportCommand> {
        self.consumer.pop().ok()
    }

    /// Number of commands waiting on this side.
    #[must_use]
    pub fn len(&self) -> usize {
        self.consumer.slots()
    }

    /// Whether no commands are waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.consumer.slots() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_queue_pops_none() {
        let (_tx, mut rx) = CommandQueue::new(4);
        assert!(rx.is_empty());
        assert!(rx.pop_next().is_none());
    }

    #[test]
    #[should_panic(expected = "capacity must be non-zero")]
    fn zero_capacity_panics() {
        let _ = CommandQueue::new(0);
    }

    #[test]
    fn fifo_order_is_preserved() {
        let (mut tx, mut rx) = CommandQueue::new(4);
        tx.try_push(TransportCommand::Play).unwrap();
        tx.try_push(TransportCommand::SeekSamples(96_000)).unwrap();
        tx.try_push(TransportCommand::Stop).unwrap();

        assert!(matches!(rx.pop_next(), Some(TransportCommand::Play)));
        assert!(matches!(
            rx.pop_next(),
            Some(TransportCommand::SeekSamples(96_000))
        ));
        assert!(matches!(rx.pop_next(), Some(TransportCommand::Stop)));
        assert!(rx.pop_next().is_none());
    }

    #[test]
    fn full_queue_returns_command_back() {
        let (mut tx, mut rx) = CommandQueue::new(2);
        assert_eq!(tx.len(), 0);
        tx.try_push(TransportCommand::Play).unwrap();
        tx.try_push(TransportCommand::Stop).unwrap();
        assert_eq!(tx.len(), 2);

        let err = tx.try_push(TransportCommand::Rewind).unwrap_err();
        assert!(matches!(err, PushError::Full(TransportCommand::Rewind)));

        // Nothing is dropped silently; the two queued commands still arrive.
        assert!(matches!(rx.pop_next(), Some(TransportCommand::Play)));
        assert!(matches!(rx.pop_next(), Some(TransportCommand::Stop)));
        assert!(rx.pop_next().is_none());

        // After draining there is room again.
        tx.try_push(TransportCommand::Rewind).unwrap();
        assert!(matches!(rx.pop_next(), Some(TransportCommand::Rewind)));
    }

    #[test]
    fn many_commands_fit_when_capacity_allows() {
        let (mut tx, mut rx) = CommandQueue::new(1000);
        for i in 0..1000u64 {
            tx.try_push(TransportCommand::SeekSamples(i)).unwrap();
        }
        for i in 0..1000u64 {
            match rx.pop_next() {
                Some(TransportCommand::SeekSamples(p)) => assert_eq!(p, i),
                other => panic!("unexpected command at {i}: {other:?}"),
            }
        }
        assert!(rx.pop_next().is_none());
    }
}
