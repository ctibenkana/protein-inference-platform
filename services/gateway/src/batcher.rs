//! Choosing which queued sequences run together.
//!
//! Pure functions over a queue in arrival order, so the policy can be
//! tested exhaustively without a model, a clock or a network.
//!
//! A batch is a rectangle: every sequence is padded to the longest, so it
//! computes `n x (longest + 2)` token positions. Two policies:
//!
//! - **`fifo`** (baseline): arrival order, until the token budget is full.
//! - **`length-aware`**: the *oldest* item anchors the batch, so nothing
//!   waits forever; then, repeatedly, the item that adds the least padding
//!   is added, as long as the batch stays within the token budget **and**
//!   its padding fraction stays within `max_padding`. Since every batch is
//!   within the cap, so is the total: the padding SLO holds by construction.

use std::collections::VecDeque;

use crate::validate::SPECIAL_TOKENS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Policy {
    Fifo,
    LengthAware,
}

impl Policy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fifo => "fifo",
            Self::LengthAware => "length_aware",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// Padded token positions per batch: batch size x (longest + 2).
    pub max_tokens: usize,
    /// Sequences per batch (the model server's limit).
    pub max_sequences: usize,
    /// Highest padding fraction a length-aware batch may reach.
    pub max_padding: f64,
}

/// An entry in the queue: a sequence length and whatever the caller needs
/// to route the result back.
#[derive(Debug)]
pub struct Queued<T> {
    pub len: usize,
    pub payload: T,
}

/// Token positions a batch computes and how many of them are padding.
pub fn cost(lengths: &[usize]) -> (usize, usize) {
    let Some(&longest) = lengths.iter().max() else {
        return (0, 0);
    };
    let computed = lengths.len() * (longest + SPECIAL_TOKENS);
    let real: usize = lengths.iter().map(|l| l + SPECIAL_TOKENS).sum();
    (computed, computed - real)
}

impl Budget {
    fn fits(&self, n: usize, longest: usize) -> bool {
        n <= self.max_sequences && n * (longest + SPECIAL_TOKENS) <= self.max_tokens
    }
}

/// Removes the next batch from `queue` (oldest first) and returns it.
/// Returns an empty batch only for an empty queue. The oldest item is always
/// included, even if on its own it exceeds the budget.
pub fn take_batch<T>(
    queue: &mut VecDeque<Queued<T>>,
    policy: Policy,
    budget: &Budget,
) -> Vec<Queued<T>> {
    let Some(anchor) = queue.pop_front() else {
        return Vec::new();
    };
    let picked = match policy {
        Policy::Fifo => pick_fifo(queue, anchor.len, budget),
        Policy::LengthAware => pick_length_aware(queue, anchor.len, budget),
    };
    let mut batch = Vec::with_capacity(picked.len() + 1);
    batch.push(anchor);
    // Remove from the back so earlier indices stay valid.
    let mut by_index = picked;
    by_index.sort_unstable_by(|a, b| b.cmp(a));
    let mut rest: Vec<Queued<T>> = by_index
        .into_iter()
        .filter_map(|i| queue.remove(i))
        .collect();
    rest.reverse();
    batch.extend(rest);
    batch
}

fn pick_fifo<T>(queue: &VecDeque<Queued<T>>, anchor_len: usize, budget: &Budget) -> Vec<usize> {
    let mut longest = anchor_len;
    let mut picked = Vec::new();
    for (i, item) in queue.iter().enumerate() {
        let new_longest = longest.max(item.len);
        if !budget.fits(picked.len() + 2, new_longest) {
            break;
        }
        longest = new_longest;
        picked.push(i);
    }
    picked
}

fn pick_length_aware<T>(
    queue: &VecDeque<Queued<T>>,
    anchor_len: usize,
    budget: &Budget,
) -> Vec<usize> {
    let mut taken = vec![false; queue.len()];
    let mut picked = Vec::new();
    let mut longest = anchor_len;
    let mut real = anchor_len + SPECIAL_TOKENS;
    loop {
        let n = picked.len() + 1;
        // (padding added, index); scanning in arrival order with a strict
        // comparison keeps the oldest item on ties.
        let mut best: Option<(usize, usize)> = None;
        for (i, item) in queue.iter().enumerate() {
            if taken[i] {
                continue;
            }
            let new_longest = longest.max(item.len);
            if !budget.fits(n + 1, new_longest) {
                continue;
            }
            let computed = (n + 1) * (new_longest + SPECIAL_TOKENS);
            let padding = computed - (real + item.len + SPECIAL_TOKENS);
            if padding as f64 > budget.max_padding * computed as f64 {
                continue;
            }
            let added = padding - (n * (longest + SPECIAL_TOKENS) - real);
            if best.is_none_or(|(b, _)| added < b) {
                best = Some((added, i));
            }
        }
        let Some((_, i)) = best else { break };
        taken[i] = true;
        picked.push(i);
        longest = longest.max(queue[i].len);
        real += queue[i].len + SPECIAL_TOKENS;
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const BUDGET: Budget = Budget {
        max_tokens: 64,
        max_sequences: 4,
        max_padding: 0.25,
    };

    /// Payload = arrival position, so tests can check identity and order.
    fn queue(lengths: &[usize]) -> VecDeque<Queued<usize>> {
        lengths
            .iter()
            .enumerate()
            .map(|(i, &len)| Queued { len, payload: i })
            .collect()
    }

    fn ids(batch: &[Queued<usize>]) -> Vec<usize> {
        batch.iter().map(|q| q.payload).collect()
    }

    fn lens(batch: &[Queued<usize>]) -> Vec<usize> {
        batch.iter().map(|q| q.len).collect()
    }

    #[test]
    fn cost_matches_the_batch_job() {
        // esm_embed.batching.Batch: lengths 10 and 4 -> 24 computed, 6 padding.
        assert_eq!(cost(&[10, 4]), (24, 6));
        assert_eq!(cost(&[]), (0, 0));
    }

    #[test]
    fn fifo_takes_arrival_order_until_the_budget() {
        // 3 x (20 + 2) = 66 > 64, so the third doesn't fit.
        let mut q = queue(&[5, 20, 20, 3]);
        assert_eq!(ids(&take_batch(&mut q, Policy::Fifo, &BUDGET)), [0, 1]);
        assert_eq!(ids(&q.into_iter().collect::<Vec<_>>()), [2, 3]);
    }

    #[test]
    fn length_aware_groups_similar_lengths() {
        // Anchor 10 (oldest); 9 and 11 are close, 2 and 30 would add padding.
        let mut q = queue(&[10, 30, 2, 9, 11]);
        let budget = Budget {
            max_tokens: 1000,
            max_sequences: 3,
            max_padding: 0.25,
        };
        let b = take_batch(&mut q, Policy::LengthAware, &budget);
        assert_eq!(ids(&b), [0, 3, 4]);
        assert_eq!(lens(&b), [10, 9, 11]);
    }

    #[test]
    fn padding_cap_keeps_short_items_out_of_a_long_batch() {
        let budget = Budget {
            max_tokens: 10_000,
            max_sequences: 16,
            max_padding: 0.10,
        };
        let mut q = queue(&[1000, 50, 60, 990]);
        let b = take_batch(&mut q, Policy::LengthAware, &budget);
        // Adding 50 would make 47% of the batch padding.
        assert_eq!(
            lens(&b),
            [1000, 990],
            "short items wait for their own batch"
        );
        // 50 with 60: 10 padding tokens of 124, 8%, within the cap.
        assert_eq!(
            lens(&take_batch(&mut q, Policy::LengthAware, &budget)),
            [50, 60]
        );
        assert!(q.is_empty());
    }

    #[test]
    fn oversized_anchor_still_runs_alone() {
        let mut q = queue(&[100, 1]);
        assert_eq!(ids(&take_batch(&mut q, Policy::LengthAware, &BUDGET)), [0]);
        let mut q = queue(&[100, 1]);
        assert_eq!(ids(&take_batch(&mut q, Policy::Fifo, &BUDGET)), [0]);
    }

    #[test]
    fn empty_queue_gives_empty_batch() {
        let mut q = queue(&[]);
        assert!(take_batch(&mut q, Policy::LengthAware, &BUDGET).is_empty());
    }

    /// Lengths drawn like real proteins: mostly a few hundred, some long.
    fn lengths() -> impl Strategy<Value = Vec<usize>> {
        prop::collection::vec(1usize..1023, 0..80)
    }

    fn budgets() -> impl Strategy<Value = Budget> {
        (1024usize..20_000, 1usize..64, 0.0f64..0.5).prop_map(
            |(max_tokens, max_sequences, max_padding)| Budget {
                max_tokens,
                max_sequences,
                max_padding,
            },
        )
    }

    fn policies() -> impl Strategy<Value = Policy> {
        prop_oneof![Just(Policy::Fifo), Just(Policy::LengthAware)]
    }

    proptest! {
        /// Draining the queue batch by batch returns every item exactly once,
        /// and each batch contains the oldest item left.
        #[test]
        fn nothing_lost_or_duplicated_and_oldest_first(l in lengths(), p in policies(), b in budgets()) {
            let mut q = queue(&l);
            let mut seen = Vec::new();
            while !q.is_empty() {
                let oldest = q.front().unwrap().payload;
                let batch = take_batch(&mut q, p, &b);
                prop_assert!(!batch.is_empty());
                prop_assert_eq!(batch[0].payload, oldest);
                seen.extend(ids(&batch));
            }
            seen.sort_unstable();
            prop_assert_eq!(seen, (0..l.len()).collect::<Vec<_>>());
        }

        /// Batches respect the token budget and size limit, except that the
        /// oldest item may run alone when it is too big on its own.
        #[test]
        fn budget_respected(l in lengths(), p in policies(), b in budgets()) {
            let mut q = queue(&l);
            while !q.is_empty() {
                let batch = lens(&take_batch(&mut q, p, &b));
                if batch.len() > 1 {
                    prop_assert!(batch.len() <= b.max_sequences);
                    prop_assert!(cost(&batch).0 <= b.max_tokens);
                }
            }
        }

        /// Length-aware batches never exceed the padding cap.
        #[test]
        fn padding_cap_respected(l in lengths(), b in budgets()) {
            let mut q = queue(&l);
            while !q.is_empty() {
                let (computed, pad) = cost(&lens(&take_batch(&mut q, Policy::LengthAware, &b)));
                prop_assert!(pad as f64 <= b.max_padding * computed as f64 + 1e-9);
            }
        }

        /// Length-aware batches are maximal: nothing left in the queue could
        /// have been added within the limits.
        #[test]
        fn length_aware_batches_are_full(l in lengths(), b in budgets()) {
            let mut q = queue(&l);
            let batch = lens(&take_batch(&mut q, Policy::LengthAware, &b));
            for item in &q {
                let mut with = batch.clone();
                with.push(item.len);
                let (computed, pad) = cost(&with);
                let fits = with.len() <= b.max_sequences && computed <= b.max_tokens;
                prop_assert!(!fits || pad as f64 > b.max_padding * computed as f64);
            }
        }
    }

    /// The batch job's sample: 300 human proteins. Fed in arrival order,
    /// length-aware batching keeps padding under the cap where FIFO does not.
    #[test]
    fn real_lengths_padding() {
        let fasta = include_str!("../../../workloads/esm-embed/samples/uniprot_human_300.fasta");
        let lengths: Vec<usize> = fasta
            .split('>')
            .filter(|r| !r.is_empty())
            .map(|r| r.lines().skip(1).map(str::len).sum::<usize>().min(1022))
            .collect();
        assert_eq!(lengths.len(), 300);
        let budget = Budget {
            max_tokens: 8192,
            max_sequences: 256,
            max_padding: 0.10,
        };
        let padding = |policy| {
            let mut q = queue(&lengths);
            let (mut computed, mut pad) = (0, 0);
            while !q.is_empty() {
                let (c, p) = cost(&lens(&take_batch(&mut q, policy, &budget)));
                computed += c;
                pad += p;
            }
            pad as f64 / computed as f64
        };
        let (fifo, aware) = (padding(Policy::Fifo), padding(Policy::LengthAware));
        assert!(fifo > 0.30, "fifo padding {fifo:.3}");
        assert!(aware <= 0.10, "length-aware padding {aware:.3}");
    }
}
