//! The channel ⇄ agent boundary.
//!
//! Channels never call the agent and the agent never calls a channel. A channel
//! publishes an [`InboundMessage`] and consumes [`OutboundMessage`]s addressed
//! to it; the agent does the mirror image. That decoupling is what makes
//! Telegram, the web UI, the scheduler and any extension channel
//! interchangeable, and it is what stops the agent loop from acquiring a
//! dependency back on the transport layer, which is the cycle this
//! architecture exists to prevent.
//!
//! Rate limiting lives here rather than in each channel because it is the same
//! policy everywhere and an extension author must not be able to omit it. It
//! applies to inbound traffic only: outbound pacing is a per-channel concern
//! (Telegram's edit interval, a socket's backpressure) with rules the bus
//! cannot know.
//!
//! Each queue is a bounded channel with **competing consumers**: an item goes
//! to exactly one of them, which is what makes several workers draining
//! [`MessageBus::outbound`] a load-sharing pool rather than a fan-out that
//! delivers every message N times. Broadcast is deliberately not offered: the
//! two look identical at the call site and differ only in whether a user gets
//! one reply or four.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ghostai_protocol::ContentPart;
use parking_lot::Mutex;
use serde_json::{Map, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::clock::Clock;

/// What a channel hands the agent.
#[derive(Debug, Clone, PartialEq)]
pub struct InboundMessage {
    /// Unique per message; a channel's own idempotency key when it has one.
    pub id: String,
    /// Channel that produced it: `web`, `telegram`, an extension id.
    pub channel_id: String,
    /// The conversation it belongs to.
    pub session_key: String,
    /// Rate-limiting identity. Per *user*, not per session or per channel.
    pub sender_id: String,
    /// The message.
    pub content: Vec<ContentPart>,
    /// When the bus accepted it.
    pub received_at_ms: i64,
    /// Channel-specific context: message ids, topic ids, reply targets.
    pub metadata: Map<String, Value>,
}

/// Why an outbound message is being sent.
///
/// Channels render these differently (Telegram edits a `progress` message in
/// place and posts a `reply` as a new one), and a channel that does not
/// distinguish them can treat every kind as a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OutboundKind {
    /// The answer.
    #[default]
    Reply,
    /// A partial answer, replaced by the next one.
    Progress,
    /// Something to know that is not an answer.
    Notice,
    /// A failure.
    Error,
}

/// What the agent hands a channel.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboundMessage {
    /// Unique per message.
    pub id: String,
    /// The channel to deliver through.
    pub channel_id: String,
    /// The conversation it belongs to.
    pub session_key: String,
    /// Channel-specific destination: chat id, user id, room, socket id.
    pub target: String,
    /// The message.
    pub content: Vec<ContentPart>,
    /// Why it is being sent.
    pub kind: OutboundKind,
    /// When the bus accepted it.
    pub created_at_ms: i64,
    /// Channel-specific context.
    pub metadata: Map<String, Value>,
}

/// What a channel publishes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InboundMessageInput {
    /// See [`InboundMessage::channel_id`].
    pub channel_id: String,
    /// See [`InboundMessage::session_key`].
    pub session_key: String,
    /// See [`InboundMessage::sender_id`].
    pub sender_id: String,
    /// The message.
    pub content: Vec<ContentPart>,
    /// See [`InboundMessage::metadata`].
    pub metadata: Map<String, Value>,
    /// Supplied only when the channel has its own idempotency key.
    pub id: Option<String>,
}

/// What the agent publishes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OutboundMessageInput {
    /// See [`OutboundMessage::channel_id`].
    pub channel_id: String,
    /// See [`OutboundMessage::session_key`].
    pub session_key: String,
    /// See [`OutboundMessage::target`].
    pub target: String,
    /// The message.
    pub content: Vec<ContentPart>,
    /// Defaults to a reply.
    pub kind: OutboundKind,
    /// See [`OutboundMessage::metadata`].
    pub metadata: Map<String, Value>,
    /// Supplied when the caller already has an id for it.
    pub id: Option<String>,
}

/// The outcome of a publish, as a value.
///
/// A rejected publish is an ordinary, expected result (a user typing too fast
/// is not an exception), and the caller has to tell the cases apart to respond
/// correctly: back off, shed load, or stop sending entirely. An error would
/// collapse them into one string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishResult {
    /// Queued, under this id.
    Accepted {
        /// The message's id.
        id: String,
    },
    /// The sender is over its limit.
    RateLimited {
        /// How long until one token is available.
        retry_after_ms: u64,
    },
    /// The queue is at capacity.
    QueueFull {
        /// How many are waiting.
        queued: usize,
    },
    /// The bus has been closed.
    Closed,
}

/// Inbound rate limiting.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RateLimitOptions {
    /// Messages per sender per minute. `0` (or less) disables the limit.
    pub per_minute: f64,
    /// Messages allowed back-to-back. Defaults to `per_minute`, capped at 10.
    pub burst: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct TokenBucket {
    tokens: f64,
    last_refill_ms: f64,
    /// A use counter, so the least recently used bucket is the smallest.
    last_used: u64,
}

/// A per-sender token bucket.
///
/// A bucket rather than a fixed window because chat traffic is bursty by
/// nature: a user sends three messages in two seconds, then nothing for a
/// minute, and a fixed window either rejects that legitimate burst or, sized to
/// allow it, permits twice the intended rate across a window boundary.
///
/// Refill is driven by [`Clock::monotonic`], so an NTP correction cannot hand a
/// sender a free reset or freeze one out for hours.
pub struct RateLimiter {
    buckets: HashMap<String, TokenBucket>,
    per_minute: f64,
    capacity: f64,
    clock: Arc<dyn Clock>,
    uses: u64,
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("per_minute", &self.per_minute)
            .field("capacity", &self.capacity)
            .field("tracked_senders", &self.buckets.len())
            .finish_non_exhaustive()
    }
}

impl RateLimiter {
    /// A hard ceiling on tracked senders, not a target.
    ///
    /// Buckets are per sender and a public group has an unbounded supply of
    /// those, so this has to bound memory even in the case where nothing is
    /// reclaimable; otherwise the eviction pass runs on every message, finds
    /// nothing, and the map grows anyway. `evict` therefore always gets the map
    /// back under the ceiling.
    pub const MAX_TRACKED_SENDERS: usize = 10_000;

    /// A limiter reading refill time from `clock`.
    pub fn new(options: RateLimitOptions, clock: Arc<dyn Clock>) -> RateLimiter {
        let per_minute = options.per_minute.max(0.0);
        let capacity = options
            .burst
            .map_or_else(|| per_minute.min(10.0), f64::from)
            .max(1.0);
        RateLimiter {
            buckets: HashMap::new(),
            per_minute,
            capacity,
            clock,
            uses: 0,
        }
    }

    /// Whether any limit applies.
    pub fn enabled(&self) -> bool {
        self.per_minute > 0.0
    }

    /// How many senders currently have a bucket.
    pub fn tracked_senders(&self) -> usize {
        self.buckets.len()
    }

    fn now_ms(&self) -> f64 {
        self.clock.monotonic().as_secs_f64() * 1000.0
    }

    fn per_ms(&self) -> f64 {
        self.per_minute / 60_000.0
    }

    /// `None` when allowed; otherwise how many milliseconds until one token is
    /// available.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a positive millisecond count, rounded up"
    )]
    pub fn consume(&mut self, sender_id: &str) -> Option<u64> {
        if !self.enabled() {
            return None;
        }

        let now = self.now_ms();
        let per_ms = self.per_ms();
        self.uses += 1;
        let uses = self.uses;
        let capacity = self.capacity;

        let bucket = self
            .buckets
            .entry(sender_id.to_owned())
            .or_insert(TokenBucket {
                tokens: capacity,
                last_refill_ms: now,
                last_used: uses,
            });
        bucket.tokens = capacity.min(bucket.tokens + (now - bucket.last_refill_ms) * per_ms);
        bucket.last_refill_ms = now;
        bucket.last_used = uses;

        let allowed = bucket.tokens >= 1.0;
        if allowed {
            bucket.tokens -= 1.0;
        }
        let retry_after = ((1.0 - bucket.tokens) / per_ms).ceil().max(0.0) as u64;

        if self.buckets.len() > Self::MAX_TRACKED_SENDERS {
            self.evict();
        }

        (!allowed).then_some(retry_after)
    }

    /// Gets the map back under the ceiling, in two passes.
    ///
    /// Refilled buckets go first: they are indistinguishable from a sender that
    /// was never seen, so dropping them changes nothing. If that is not enough
    /// (a flood of distinct senders, which is what an abuse case looks like) the
    /// least recently used are dropped as well.
    ///
    /// That second pass is deliberately fail-open. An evicted sender's next
    /// message is treated as their first, so the worst case is that an attacker
    /// spending one sender identity per message evades a per-sender limit,
    /// which they could do anyway, by definition. Failing closed would instead
    /// let them lock out real users by flooding them out of the map, and
    /// unbounded growth would let them take the process down outright.
    fn evict(&mut self) {
        let now = self.now_ms();
        let per_ms = self.per_ms();
        let capacity = self.capacity;

        // Project the refill forward to now. A bucket's level is only
        // recomputed when *that* sender sends, so a bucket idle long enough to
        // be full still holds the level it had at its last message; testing the
        // stored value directly would reclaim almost nothing.
        self.buckets
            .retain(|_, bucket| bucket.tokens + (now - bucket.last_refill_ms) * per_ms < capacity);

        let excess = self.buckets.len().saturating_sub(Self::MAX_TRACKED_SENDERS);
        if excess == 0 {
            return;
        }
        let mut by_age: Vec<(u64, String)> = self
            .buckets
            .iter()
            .map(|(sender, bucket)| (bucket.last_used, sender.clone()))
            .collect();
        // Only the oldest `excess` need finding, not a full order.
        by_age.select_nth_unstable(excess - 1);
        for (_, sender) in by_age.into_iter().take(excess) {
            self.buckets.remove(&sender);
        }
    }
}

/// Mints message ids. The composition root supplies a UUIDv7 source; tests
/// supply a counter.
pub type IdSource = Arc<dyn Fn() -> String + Send + Sync>;

/// Inputs to [`MessageBus::new`].
pub struct MessageBusOptions {
    /// Stamps `received_at_ms` / `created_at_ms` and drives the rate limiter.
    pub clock: Arc<dyn Clock>,
    /// Ids for messages that arrive without one.
    pub new_id: IdSource,
    /// Messages buffered per direction before publishes are refused.
    ///
    /// Bounded on purpose. An unbounded queue in front of a component that can
    /// stall (a provider that stopped responding, a channel that lost its
    /// socket) converts a stall into unbounded memory growth, and the process
    /// dies with no indication of which component stopped consuming.
    pub capacity: usize,
    /// Inbound rate limiting.
    pub rate_limit: RateLimitOptions,
}

impl MessageBusOptions {
    /// Options with the default capacity of 1000 and no rate limit.
    pub fn new(clock: Arc<dyn Clock>, new_id: IdSource) -> MessageBusOptions {
        MessageBusOptions {
            clock,
            new_id,
            capacity: 1_000,
            rate_limit: RateLimitOptions::default(),
        }
    }
}

impl std::fmt::Debug for MessageBusOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageBusOptions")
            .field("capacity", &self.capacity)
            .field("rate_limit", &self.rate_limit)
            .finish_non_exhaustive()
    }
}

/// One direction's bounded queue.
struct Queue<T> {
    sender: mpsc::Sender<T>,
    receiver: Arc<tokio::sync::Mutex<mpsc::Receiver<T>>>,
    closed: Arc<AtomicBool>,
    close: CancellationToken,
}

impl<T> Queue<T> {
    fn new(capacity: usize) -> Queue<T> {
        let (sender, receiver) = mpsc::channel(capacity.max(1));
        Queue {
            sender,
            receiver: Arc::new(tokio::sync::Mutex::new(receiver)),
            closed: Arc::new(AtomicBool::new(false)),
            close: CancellationToken::new(),
        }
    }

    fn size(&self) -> usize {
        self.sender.max_capacity() - self.sender.capacity()
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// `false` when the queue is at capacity.
    fn push(&self, item: T) -> bool {
        self.sender.try_send(item).is_ok()
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.close.cancel();
    }

    fn consumer(&self) -> Consumer<T> {
        Consumer {
            receiver: Arc::clone(&self.receiver),
            closed: Arc::clone(&self.closed),
            close: self.close.clone(),
        }
    }
}

/// One consumer of a queue. Several may exist; each item reaches exactly one.
///
/// Dropping a consumer stops only that consumer: the queue and its other
/// consumers are untouched.
pub struct Consumer<T> {
    receiver: Arc<tokio::sync::Mutex<mpsc::Receiver<T>>>,
    closed: Arc<AtomicBool>,
    close: CancellationToken,
}

impl<T> std::fmt::Debug for Consumer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Consumer")
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl<T> Consumer<T> {
    /// The next item, or `None` once the bus is closed and drained.
    ///
    /// Buffered items stay readable after close: a consumer draining after
    /// shutdown still gets them, so a graceful stop can flush replies already
    /// produced. What close must do is release a consumer that is *waiting*,
    /// or an idle loop would hold the process open forever.
    pub async fn next(&self) -> Option<T> {
        let mut receiver = self.receiver.lock().await;
        if let Ok(item) = receiver.try_recv() {
            return Some(item);
        }
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        tokio::select! {
            item = receiver.recv() => item,
            // A message published in the same instant as the close is still
            // owed to somebody.
            () = self.close.cancelled() => receiver.try_recv().ok(),
        }
    }
}

/// The bus: two bounded queues and an inbound rate limiter.
pub struct MessageBus {
    inbound: Queue<InboundMessage>,
    outbound: Queue<OutboundMessage>,
    limiter: Mutex<RateLimiter>,
    clock: Arc<dyn Clock>,
    new_id: IdSource,
}

impl std::fmt::Debug for MessageBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageBus")
            .field("inbound_size", &self.inbound_size())
            .field("outbound_size", &self.outbound_size())
            .field("closed", &self.closed())
            .finish_non_exhaustive()
    }
}

impl MessageBus {
    /// A bus with empty queues.
    pub fn new(options: MessageBusOptions) -> MessageBus {
        MessageBus {
            inbound: Queue::new(options.capacity),
            outbound: Queue::new(options.capacity),
            limiter: Mutex::new(RateLimiter::new(
                options.rate_limit,
                Arc::clone(&options.clock),
            )),
            clock: options.clock,
            new_id: options.new_id,
        }
    }

    /// Messages waiting for the agent.
    pub fn inbound_size(&self) -> usize {
        self.inbound.size()
    }

    /// Messages waiting for a channel.
    pub fn outbound_size(&self) -> usize {
        self.outbound.size()
    }

    /// Whether [`MessageBus::close`] has been called.
    pub fn closed(&self) -> bool {
        self.inbound.is_closed()
    }

    /// Queues a message from a channel, rate-limited per sender.
    pub fn publish_inbound(&self, input: InboundMessageInput) -> PublishResult {
        if self.inbound.is_closed() {
            return PublishResult::Closed;
        }

        if let Some(retry_after_ms) = self.limiter.lock().consume(&input.sender_id) {
            return PublishResult::RateLimited { retry_after_ms };
        }

        let id = input.id.unwrap_or_else(|| (self.new_id)());
        let message = InboundMessage {
            id: id.clone(),
            channel_id: input.channel_id,
            session_key: input.session_key,
            sender_id: input.sender_id,
            content: input.content,
            received_at_ms: self.clock.now_ms(),
            metadata: input.metadata,
        };

        if self.inbound.push(message) {
            PublishResult::Accepted { id }
        } else {
            PublishResult::QueueFull {
                queued: self.inbound.size(),
            }
        }
    }

    /// Queues a message for a channel. Not rate limited: outbound pacing
    /// belongs to the channel that sends it.
    pub fn publish_outbound(&self, input: OutboundMessageInput) -> PublishResult {
        if self.outbound.is_closed() {
            return PublishResult::Closed;
        }

        let id = input.id.unwrap_or_else(|| (self.new_id)());
        let message = OutboundMessage {
            id: id.clone(),
            channel_id: input.channel_id,
            session_key: input.session_key,
            target: input.target,
            content: input.content,
            kind: input.kind,
            created_at_ms: self.clock.now_ms(),
            metadata: input.metadata,
        };

        if self.outbound.push(message) {
            PublishResult::Accepted { id }
        } else {
            PublishResult::QueueFull {
                queued: self.outbound.size(),
            }
        }
    }

    /// Consumed by the agent. Ends when the bus closes.
    pub fn inbound(&self) -> Consumer<InboundMessage> {
        self.inbound.consumer()
    }

    /// Consumed by the channel manager. Ends when the bus closes.
    pub fn outbound(&self) -> Consumer<OutboundMessage> {
        self.outbound.consumer()
    }

    /// Ends every waiting consumer so shutdown does not hang on a receive.
    pub fn close(&self) {
        self.inbound.close();
        self.outbound.close();
    }
}
