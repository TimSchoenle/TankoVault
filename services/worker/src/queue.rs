//! Prioritised, fair scan-task scheduling: restores per-provider fairness that a single
//! wildcard consumer's FIFO order would let one large catalogue scan monopolize.

use futures::StreamExt;
use std::collections::HashSet;
use std::time::Duration;
use tankovault_bus::{BrokerConsumer, BrokerMessage, Bus};
use tankovault_db::PgPool;
use tankovault_domain::ScanMode;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// How long the queue waits after a round in which every lane was empty, before polling
/// them all again.
///
/// A pull with `no_wait` costs one round trip per lane, so an idle pool polling flat out
/// would be pure chatter. The backoff doubles from here to [`IDLE_POLL_MAX`], which trades
/// a few seconds of pickup latency on a task that arrives into an idle pool — irrelevant
/// next to the seconds-to-minutes a scan task itself takes — for a quiet broker.
const IDLE_POLL_MIN: Duration = Duration::from_millis(200);

/// Ceiling for the idle backoff.
const IDLE_POLL_MAX: Duration = Duration::from_secs(5);

/// The scan modes in the order they are served — **fast first, always**.
///
/// This array *is* the priority policy; the scheduler has no other notion of precedence.
const TIERS: [ScanMode; 2] = [ScanMode::Fast, ScanMode::Full];

/// One provider's queue within one tier: its durable consumer, and the slug that names it.
struct Lane {
    slug: String,
    consumer: BrokerConsumer,
}

/// Every provider's lane for a single scan mode, plus the round-robin cursor over them.
///
/// Each tier carries its own cursor. A shared one would let a busy fast tier advance the
/// full tier's position, so a full lane could be skipped for reasons that have nothing to
/// do with whether it was served.
struct Tier {
    mode: ScanMode,
    lanes: Vec<Lane>,
    /// Index of the lane to try first on the next round.
    cursor: usize,
}

/// A prioritised, round-robin view over every provider's task lanes.
///
/// Not `Clone` and not shared: each worker task owns one, and takes one message at a time
/// from it. Holding messages in a local buffer would be faster but wrong — a buffered task
/// is a *claimed* task, and one waiting behind several minutes of other work would breach
/// its redelivery deadline and be handed to a second worker. It would also defeat the
/// prioritisation, since a fast task arriving mid-buffer would still queue behind whatever
/// full-scan tasks were already held.
pub(crate) struct FairQueue {
    bus: Bus,
    pool: PgPool,
    /// In [`TIERS`] order; index 0 is served first.
    tiers: Vec<Tier>,
    refresh_every: Duration,
    next_refresh: Instant,
}

impl FairQueue {
    /// Open the queue: retire the wildcard consumer this replaces, then build the lanes.
    ///
    /// # Errors
    /// Fails if the wildcard consumer cannot be removed. That is fatal rather than
    /// tolerated: its filter subject overlaps every per-lane filter, and a work-queue stream
    /// refuses to create a second consumer that overlaps an existing one — so a worker that
    /// carried on would sit against zero lanes and silently consume nothing.
    pub(crate) async fn open(
        bus: Bus,
        pool: PgPool,
        refresh_every: Duration,
    ) -> anyhow::Result<Self> {
        bus.retire_wildcard_task_consumer().await?;
        let mut queue = Self {
            bus,
            pool,
            tiers: TIERS
                .into_iter()
                .map(|mode| Tier {
                    mode,
                    lanes: Vec::new(),
                    cursor: 0,
                })
                .collect(),
            refresh_every,
            next_refresh: Instant::now(),
        };
        queue.refresh_lanes().await;
        Ok(queue)
    }

    /// The next task to run, or `None` once `shutdown` is triggered.
    ///
    /// Blocks — by polling with a backoff — until some lane has work. Shutdown is observed
    /// between rounds rather than mid-task, which is what lets a rolling restart drain
    /// cleanly instead of leaving claimed tasks to time out.
    pub(crate) async fn next_task(
        &mut self,
        shutdown: &CancellationToken,
    ) -> Option<BrokerMessage> {
        let mut idle = IDLE_POLL_MIN;
        loop {
            if shutdown.is_cancelled() {
                return None;
            }
            if Instant::now() >= self.next_refresh {
                self.refresh_lanes().await;
            }
            if let Some(msg) = self.poll_round().await {
                return Some(msg);
            }
            tokio::select! {
                () = shutdown.cancelled() => return None,
                () = tokio::time::sleep(idle) => {}
            }
            idle = (idle * 2).min(IDLE_POLL_MAX);
        }
    }

    /// Offer every lane a turn, highest-priority tier first, and return the first task found.
    ///
    /// A tier is exhausted before the next is touched — that is the whole of the fast-over-
    /// full guarantee.
    async fn poll_round(&mut self) -> Option<BrokerMessage> {
        for tier in &mut self.tiers {
            if let Some(msg) = tier.poll_round().await {
                return Some(msg);
            }
        }
        None
    }

    /// Open lanes for any provider that does not have one yet, in every tier.
    ///
    /// Lanes are only ever added. Dropping one would strand whatever it still holds, and a
    /// lane costs nothing but a round trip per idle poll — so a provider that is deleted
    /// keeps being drained rather than leaving its queued tasks (and the runs waiting on
    /// them) hanging.
    ///
    /// Both sources are best-effort: a failure to read either one leaves the existing lanes
    /// untouched and is retried on the next refresh, because a worker that dropped its
    /// lanes on a transient database blip would go silently idle.
    async fn refresh_lanes(&mut self) {
        self.next_refresh = Instant::now() + self.refresh_every;

        let mut wanted: HashSet<(ScanMode, String)> = HashSet::new();
        match tankovault_db::repo::providers::list(&self.pool).await {
            Ok(providers) => {
                for provider in providers {
                    for mode in TIERS {
                        wanted.insert((mode, provider.slug.clone()));
                    }
                }
            }
            Err(e) => tracing::warn!(
                error = %e,
                "could not read the provider list; keeping the current task lanes"
            ),
        }
        // The stream outlives the provider table: a renamed or deleted provider can still
        // have tasks queued under its old slug, and only a lane with that exact filter can
        // reach them.
        match self.bus.task_consumer_lanes().await {
            Ok(existing) => wanted.extend(existing),
            Err(e) => tracing::warn!(
                error = %e,
                "could not list existing task consumers; lanes for providers no longer in the \
                 database may be missed this round"
            ),
        }

        // Sorted so lanes land in the same order on every replica and across restarts, which
        // keeps a log or a metric comparable between them. Fairness does not depend on it.
        let mut wanted: Vec<(ScanMode, String)> = wanted.into_iter().collect();
        wanted.sort_unstable_by(|(a_mode, a_slug), (b_mode, b_slug)| {
            (a_mode.as_str(), a_slug).cmp(&(b_mode.as_str(), b_slug))
        });

        for (mode, slug) in wanted {
            let Some(tier) = self.tiers.iter_mut().find(|tier| tier.mode == mode) else {
                continue;
            };
            if tier.lanes.iter().any(|lane| lane.slug == slug) {
                continue;
            }
            match self.bus.provider_task_consumer(&slug, mode).await {
                Ok(consumer) => {
                    tracing::info!(provider = %slug, scan = %mode, "opened provider task lane");
                    tier.lanes.push(Lane { slug, consumer });
                }
                Err(e) => tracing::warn!(
                    provider = %slug,
                    scan = %mode,
                    error = %e,
                    next = "this provider's scan tasks will not be executed until the lane opens",
                    "could not open provider task lane"
                ),
            }
        }
    }
}

impl Tier {
    /// Try every lane in this tier once, starting where the last round left off, and return
    /// the first task found.
    ///
    /// ## Why the lanes are still polled sequentially
    ///
    /// An idle round costs one `no_wait` round trip per lane, so pickup latency and broker
    /// chatter both scale with provider count — at 25 providers and 1 ms RTT that is ~50 ms
    /// per tier per poll. The obvious fix is to issue the fetches concurrently and take the
    /// first answer.
    ///
    /// That fix is **wrong here**, and the reason is worth recording so it is not
    /// re-attempted: a concurrent round claims a message from *every* non-empty lane, but a
    /// worker may only hold one. The extras have to be handed back, and handing a `JetStream`
    /// message back — whether by `nak` or by letting the ack deadline lapse — **increments
    /// its delivery count**. `MAX_TASK_DELIVERIES` is 3. During a busy period a task could
    /// therefore exhaust its entire retry budget by being polled past, never once having
    /// actually failed, and be recorded as a permanent failure. Trading a correctness
    /// property for tens of milliseconds of pickup latency is a bad trade, and the latency is
    /// already irrelevant next to the seconds-to-minutes a scan task itself takes.
    ///
    /// The chatter itself is already bounded by the caller: [`FairQueue::next_task`] doubles
    /// its wait from [`IDLE_POLL_MIN`] to [`IDLE_POLL_MAX`] while every tier comes back
    /// empty, so an idle pool settles at one round per lane per five seconds, not a spin.
    /// Deliberately left as is.
    async fn poll_round(&mut self) -> Option<BrokerMessage> {
        let lane_count = self.lanes.len();
        if lane_count == 0 {
            return None;
        }
        for _ in 0..lane_count {
            let lane = &self.lanes[take_turn(&mut self.cursor, lane_count)];
            match take_one(lane).await {
                Ok(Some(msg)) => {
                    metrics::counter!(
                        "scan_tasks_served_total",
                        "provider" => lane.slug.clone(),
                        "scan" => self.mode.as_str(),
                    )
                    .increment(1);
                    return Some(msg);
                }
                Ok(None) => {}
                // One unreachable lane must not stall the others: skip it and let the next
                // round (or the next refresh, if its consumer was deleted) sort it out.
                Err(e) => tracing::warn!(
                    provider = %lane.slug,
                    scan = %self.mode,
                    error = %e,
                    "could not pull from provider task lane; trying the other providers"
                ),
            }
        }
        None
    }
}

/// Give the next lane its turn, and return which one that is.
///
/// The cursor advances *before* the lane is served rather than after: the following round
/// has to start at the lane after the one that answered, or a busy lane sitting early in the
/// vec would be offered two turns per cycle. Modulo on read as well as on write keeps a
/// cursor valid across lanes being appended.
///
/// `lane_count` must be non-zero.
fn take_turn(cursor: &mut usize, lane_count: usize) -> usize {
    let idx = *cursor % lane_count;
    *cursor = (idx + 1) % lane_count;
    idx
}

/// Pull at most one task from a lane, without waiting for one to arrive.
///
/// `fetch` sends a `no_wait` pull request, so an empty lane answers immediately and the
/// round moves on. Asking for exactly one message is also what makes dropping the batch
/// afterwards safe: there is no second message in flight for this request that would go
/// unacked and have to wait out its redelivery deadline.
async fn take_one(lane: &Lane) -> anyhow::Result<Option<BrokerMessage>> {
    let mut batch = lane.consumer.fetch().max_messages(1).messages().await?;
    match batch.next().await {
        // The stream's error is a boxed trait object, which `?` cannot convert on its own.
        Some(msg) => Ok(Some(msg.map_err(|e| anyhow::anyhow!(e))?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::{TIERS, take_turn};
    use tankovault_domain::ScanMode;

    #[test]
    fn fast_scans_outrank_full_scans() {
        // `poll_round` exhausts each tier in array order, so this array *is* the priority
        // policy: a fast task is never left waiting behind a catalogue walk.
        assert_eq!(TIERS[0], ScanMode::Fast);
        // Every mode needs a tier, or its tasks would be published to a lane nothing serves.
        assert_eq!(TIERS.len(), ScanMode::all().len());
        for mode in ScanMode::all() {
            assert!(TIERS.contains(mode), "{mode} has no lane");
        }
    }

    /// The lanes a full round would try, in order, from `cursor`.
    fn round_order(cursor: &mut usize, lane_count: usize) -> Vec<usize> {
        (0..lane_count)
            .map(|_| take_turn(cursor, lane_count))
            .collect()
    }

    #[test]
    fn a_round_offers_every_lane_exactly_once() {
        let mut cursor = 0;
        assert_eq!(round_order(&mut cursor, 4), vec![0, 1, 2, 3]);
    }

    #[test]
    fn the_next_round_starts_after_the_lane_that_was_served() {
        // The fairness property: a lane that answered goes to the back of the line, so a
        // provider with a huge backlog cannot be served again while another still waits.
        // A round stops at the first lane with work, so only that lane took a turn.
        let mut cursor = 0;
        assert_eq!(take_turn(&mut cursor, 3), 0);
        assert_eq!(round_order(&mut cursor, 3), vec![1, 2, 0]);
    }

    #[test]
    fn the_cursor_survives_lanes_being_added() {
        // Lanes are appended as providers appear, so a cursor left pointing past the end of
        // a shorter round must still land inside it.
        let mut cursor = 11;
        assert_eq!(round_order(&mut cursor, 3), vec![2, 0, 1]);
    }
}
