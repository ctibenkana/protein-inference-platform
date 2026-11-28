//! Dynamic batching: sequences from many requests share batches.
//!
//! Each request's sequences enter one queue as separate items. A dispatcher
//! task sends a batch whenever a backend slot is free and either enough work
//! is queued to fill a batch or the oldest item has waited `max_wait`.
//! Which items go together is decided by [`crate::batcher`]. Each item's
//! result is sent back to its request through its own channel, so a request
//! can span several batches and batches can mix requests.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::time::{Instant, sleep_until};

use crate::backend::{Backend, BackendError, BatchOutput};
use crate::batcher::{self, Budget, Policy, Queued};
use crate::metrics::{DropLabel, Metrics, OutcomeLabel, TriggerLabel};

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub policy: Policy,
    pub budget: Budget,
    /// Longest an item waits for others to share its batch.
    pub max_wait: Duration,
    /// Batches in flight to the model server at once.
    pub concurrency: usize,
}

/// One embedding inside a finished batch.
#[derive(Debug, Clone)]
pub struct Row {
    pub batch: Arc<BatchOutput>,
    pub index: usize,
}

impl Row {
    pub fn embedding(&self) -> &[f32] {
        self.batch.row(self.index)
    }
}

pub type Reply = Result<Row, BackendError>;

struct Item {
    sequence: String,
    enqueued: Instant,
    deadline: Instant,
    reply: oneshot::Sender<Reply>,
}

#[derive(Clone)]
pub struct Scheduler {
    tx: mpsc::UnboundedSender<Queued<Item>>,
}

impl Scheduler {
    /// Starts the dispatcher on the current Tokio runtime.
    pub fn start(backend: Backend, config: Config, metrics: Arc<Metrics>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(dispatch(rx, backend, config, metrics));
        Self { tx }
    }

    /// Queues one sequence. The receiver yields its embedding, or an error.
    /// Dropping the receiver cancels the item if it hasn't been batched yet.
    pub fn submit(&self, sequence: String, deadline: Instant) -> oneshot::Receiver<Reply> {
        let (reply, rx) = oneshot::channel();
        let item = Item {
            enqueued: Instant::now(),
            deadline,
            reply,
            sequence,
        };
        // If the dispatcher is gone, `reply` is dropped with the message and
        // the caller sees a closed channel.
        let _ = self.tx.send(Queued {
            len: item.sequence.len(),
            payload: item,
        });
        rx
    }
}

struct Queue {
    items: VecDeque<Queued<Item>>,
    /// Residues plus special tokens, summed over queued items.
    tokens: usize,
}

impl Queue {
    fn push(&mut self, item: Queued<Item>, m: &Metrics) {
        self.tokens += item.len + crate::validate::SPECIAL_TOKENS;
        self.items.push_back(item);
        self.observe(m);
    }

    fn observe(&self, m: &Metrics) {
        m.queue_items.set(self.items.len() as i64);
        m.queue_tokens.set(self.tokens as i64);
    }

    /// Drops items nobody is waiting for, and fails items past their deadline.
    fn sweep(&mut self, now: Instant, m: &Metrics) {
        let before = self.items.len();
        let mut kept = VecDeque::with_capacity(before);
        for q in self.items.drain(..) {
            let reason = if q.payload.reply.is_closed() {
                Some("cancelled")
            } else if q.payload.deadline <= now {
                Some("deadline")
            } else {
                None
            };
            match reason {
                None => kept.push_back(q),
                Some(reason) => {
                    self.tokens -= q.len + crate::validate::SPECIAL_TOKENS;
                    m.dropped.get_or_create(&DropLabel { reason }).inc();
                    let _ = q.payload.reply.send(Err(BackendError::Timeout));
                }
            }
        }
        self.items = kept;
        if self.items.len() != before {
            self.observe(m);
        }
    }

    /// Why a batch should go now, if it should.
    fn trigger(&self, now: Instant, config: &Config) -> Option<&'static str> {
        let oldest = self.items.front()?;
        if self.tokens >= config.budget.max_tokens
            || self.items.len() >= config.budget.max_sequences
        {
            Some("full")
        } else if now >= oldest.payload.enqueued + config.max_wait {
            Some("max_wait")
        } else {
            None
        }
    }
}

async fn dispatch(
    mut rx: mpsc::UnboundedReceiver<Queued<Item>>,
    backend: Backend,
    config: Config,
    metrics: Arc<Metrics>,
) {
    let slots = Arc::new(Semaphore::new(config.concurrency.max(1)));
    let mut queue = Queue {
        items: VecDeque::new(),
        tokens: 0,
    };
    let mut open = true;

    loop {
        while let Ok(item) = rx.try_recv() {
            queue.push(item, &metrics);
        }
        queue.sweep(Instant::now(), &metrics);

        if queue.items.is_empty() {
            if !open {
                return;
            }
            match rx.recv().await {
                Some(item) => queue.push(item, &metrics),
                None => open = false,
            }
            continue;
        }

        // Wait for a free slot; keep accepting items meanwhile, since a
        // busy backend is exactly when batches fill up.
        let permit = tokio::select! {
            permit = slots.clone().acquire_owned() => permit.expect("semaphore is never closed"),
            item = rx.recv(), if open => {
                match item {
                    Some(item) => queue.push(item, &metrics),
                    None => open = false,
                }
                continue;
            }
        };

        // Waiting for the slot may have taken a while: drop what expired or
        // was abandoned meanwhile, so it is never sent to the model.
        let now = Instant::now();
        queue.sweep(now, &metrics);
        if queue.items.is_empty() {
            continue;
        }
        // Once no more requests can arrive, waiting gains nothing.
        let trigger = queue.trigger(now, &config).or((!open).then_some("closing"));
        let Some(trigger) = trigger else {
            let wake = queue
                .items
                .front()
                .map(|q| q.payload.enqueued + config.max_wait)
                .unwrap_or(now);
            tokio::select! {
                _ = sleep_until(wake) => {}
                item = rx.recv(), if open => match item {
                    Some(item) => queue.push(item, &metrics),
                    None => open = false,
                },
            }
            continue; // the permit is released and re-acquired; it was free
        };

        let batch = batcher::take_batch(&mut queue.items, config.policy, &config.budget);
        queue.tokens -= batch
            .iter()
            .map(|q| q.len + crate::validate::SPECIAL_TOKENS)
            .sum::<usize>();
        queue.observe(&metrics);
        metrics
            .batch_trigger
            .get_or_create(&TriggerLabel { trigger })
            .inc();
        for q in &batch {
            metrics
                .queue_wait
                .observe((now - q.payload.enqueued).as_secs_f64());
        }

        let backend = backend.clone();
        let metrics = metrics.clone();
        tokio::spawn(async move {
            run_batch(&backend, batch, &metrics).await;
            drop(permit);
        });
    }
}

/// Sends one batch, retrying once on failures another attempt could fix,
/// within the latest deadline in the batch; then answers every item.
async fn run_batch(backend: &Backend, batch: Vec<Queued<Item>>, m: &Metrics) {
    let deadline = batch
        .iter()
        .map(|q| q.payload.deadline)
        .max()
        .expect("batches are never empty");
    let (sequences, replies): (Vec<String>, Vec<oneshot::Sender<Reply>>) = batch
        .into_iter()
        .map(|q| (q.payload.sequence, q.payload.reply))
        .unzip();
    let lengths: Vec<usize> = sequences.iter().map(String::len).collect();

    let mut attempt = 0;
    let result = loop {
        attempt += 1;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break Err(BackendError::Timeout);
        }
        let started = Instant::now();
        let result = backend.embed(&sequences, remaining).await;
        let outcome = match &result {
            Ok(_) => "ok",
            Err(e) => e.kind(),
        };
        m.backend
            .get_or_create(&OutcomeLabel { outcome })
            .observe(started.elapsed().as_secs_f64());
        match result {
            Err(e) if e.retryable() && attempt < 2 => {
                tracing::warn!(error = %e, sequences = sequences.len(), "model server call failed; retrying once");
            }
            other => break other,
        }
    };

    match result {
        Ok(out) => {
            let (computed, pad) = batcher::cost(&lengths);
            m.batch_sequences.observe(lengths.len() as f64);
            m.tokens_computed.inc_by(computed as u64);
            m.pad_tokens.inc_by(pad as u64);
            let out = Arc::new(out);
            for (index, reply) in replies.into_iter().enumerate() {
                let _ = reply.send(Ok(Row {
                    batch: out.clone(),
                    index,
                }));
            }
        }
        Err(e) => {
            if !e.retryable() && !matches!(e, BackendError::Timeout) {
                tracing::error!(error = %e, "model server call failed");
            }
            for reply in replies {
                let _ = reply.send(Err(e.clone()));
            }
        }
    }
}
