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

/// The scan modes, fast first — the order [`tier_order`] serves them in by default.
const TIERS: [ScanMode; 2] = [ScanMode::Fast, ScanMode::Full];

/// Fast tasks served in a row before the full tier is offered a turn ahead of them.
///
/// ## Why this exists at all
///
/// Priority used to be strict, and that was safe **because the fast tier was bounded by
/// construction**: a fast run enqueued exactly one `latest_feed` task per provider and walked the
/// feed inside it, so the fast lanes could never hold more than one task per provider and could
/// not outlast the full tier's patience.
///
/// A fast run now fans out a `series` task per feed entry — which is what makes its progress
/// legible at all — and that argument lapses with it. Under strict priority a single provider
/// with a large latest feed would hold a non-empty fast lane for the whole run, and since every
/// fast lane is offered a turn before any full lane is looked at, **every other provider's full
/// scan would wait behind it**. That is not a slow full scan; it is a full scan that does not run.
///
/// One turn in nine is a deliberately small concession. A fast task is the one that surfaces a new
/// chapter, so the fast tier should still win nearly every time; the point is only that "nearly
/// always" is a bound and "always" is not.
const FULL_TIER_EVERY: u32 = 8;

/// The order to offer the tiers in, given how many fast tasks have been served consecutively.
///
/// Split out from the polling so the policy can be tested without a broker: the starvation this
/// prevents takes hours to show up in a deployment and nothing about it fails loudly.
fn tier_order(fast_streak: u32) -> [ScanMode; 2] {
    if fast_streak >= FULL_TIER_EVERY {
        [ScanMode::Full, ScanMode::Fast]
    } else {
        TIERS
    }
}

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
/// Not `Clone` and not shared: the consumer loop owns one, and takes one message at a time
/// from it. Holding messages in a local buffer would be faster but wrong — a buffered task
/// is a *claimed* task, and one waiting behind several minutes of other work would breach
/// its redelivery deadline and be handed to a second worker. It would also defeat the
/// prioritisation, since a fast task arriving mid-buffer would still queue behind whatever
/// full-scan tasks were already held.
///
/// Concurrency lives entirely above this type: the loop executes several claimed tasks at
/// once, but still claims them one at a time and never holds one it is not already running.
pub(crate) struct FairQueue {
    bus: Bus,
    pool: PgPool,
    /// In [`TIERS`] order; [`tier_order`] decides which is offered first on a given round.
    tiers: Vec<Tier>,
    /// Fast tasks served since the last full one, for [`FULL_TIER_EVERY`].
    fast_streak: u32,
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
            fast_streak: 0,
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
    ///
    /// `busy` names the providers that already have a task in flight; their lanes are passed
    /// over. The caller owns that set because it owns the tasks — see [`Tier::poll_round`]
    /// for why the filter has to happen before the pull rather than after it.
    pub(crate) async fn next_task(
        &mut self,
        shutdown: &CancellationToken,
        busy: &HashSet<String>,
    ) -> Option<BrokerMessage> {
        let mut idle = IDLE_POLL_MIN;
        loop {
            if shutdown.is_cancelled() {
                return None;
            }
            if Instant::now() >= self.next_refresh {
                self.refresh_lanes().await;
            }
            if let Some(msg) = self.poll_round(busy).await {
                return Some(msg);
            }
            tokio::select! {
                () = shutdown.cancelled() => return None,
                () = tokio::time::sleep(idle) => {}
            }
            idle = (idle * 2).min(IDLE_POLL_MAX);
        }
    }

    /// Offer every lane a turn, in [`tier_order`], and return the first task found.
    ///
    /// A tier is exhausted before the next is touched — that is the whole of the fast-over-full
    /// guarantee, and it survives `busy`: a fast lane skipped for being in flight does not promote
    /// a full lane past a fast one that is merely idle. The order itself is the one concession,
    /// and only after [`FULL_TIER_EVERY`] consecutive fast tasks.
    async fn poll_round(&mut self, busy: &HashSet<String>) -> Option<BrokerMessage> {
        for mode in tier_order(self.fast_streak) {
            let Some(tier) = self.tiers.iter_mut().find(|tier| tier.mode == mode) else {
                continue;
            };
            if let Some(msg) = tier.poll_round(busy).await {
                // Counted on what was *served*, not on what was offered: a round that found the
                // full tier empty has not fed the full tier, and resetting the streak there would
                // let a busy fast tier reset its own debt forever.
                self.fast_streak = match mode {
                    ScanMode::Fast => self.fast_streak.saturating_add(1),
                    ScanMode::Full => 0,
                };
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
    ///
    /// ## Why `busy` is filtered before the pull, never after
    ///
    /// The worker now runs several providers concurrently, one task each, so a lane whose
    /// provider is already in flight has nowhere to put a message. Because a lane *is* a
    /// provider, that is known before pulling — so the skip costs nothing and, crucially,
    /// keeps the property the section above depends on: every message this returns is
    /// executed immediately, so none is ever handed back and no delivery count moves.
    /// Pulling first and returning the message on a busy provider would reintroduce exactly
    /// the `nak` that section rejects, by a different route.
    async fn poll_round(&mut self, busy: &HashSet<String>) -> Option<BrokerMessage> {
        // Snapshotted up front so the cursor can advance while `self.lanes` is borrowed for
        // the pull, and so a tier whose providers are all in flight costs no round trips
        // at all rather than one per lane.
        let blocked: Vec<bool> = self.lanes.iter().map(|l| busy.contains(&l.slug)).collect();
        let mut free = blocked.iter().filter(|b| !**b).count();
        while free > 0 {
            free -= 1;
            let Some(idx) = next_free_lane(&mut self.cursor, &blocked) else {
                break;
            };
            let lane = &self.lanes[idx];
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

/// The next lane free to be served, advancing the cursor past every provider already in
/// flight. `None` when they all are.
///
/// A blocked lane **spends its turn** on the way past. That is the fairness rule: the provider
/// is already being served, so consuming its turn is what stops it being offered again ahead
/// of an idle provider the instant it frees. Leaving the turn unspent would let one fast
/// provider cycle through the queue while a slower one starved.
fn next_free_lane(cursor: &mut usize, blocked: &[bool]) -> Option<usize> {
    let lane_count = blocked.len();
    if lane_count == 0 {
        return None;
    }
    for _ in 0..lane_count {
        let idx = take_turn(cursor, lane_count);
        if !blocked[idx] {
            return Some(idx);
        }
    }
    None
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
    use super::{FULL_TIER_EVERY, TIERS, next_free_lane, take_turn, tier_order};
    use tankovault_domain::ScanMode;

    /// A fast run fans out a task per feed entry, so its lane stays non-empty for the whole run.
    /// Under strict priority that is not "the full tier waits" — it is every other provider's full
    /// scan not running at all for as long as any provider has a fast run in flight. The debt
    /// counter is the bound, and nothing about its absence would fail loudly.
    #[test]
    fn the_full_tier_gets_a_turn_before_the_fast_tier_can_starve_it() {
        assert_eq!(tier_order(0)[0], ScanMode::Fast, "fast wins by default");
        assert_eq!(
            tier_order(FULL_TIER_EVERY - 1)[0],
            ScanMode::Fast,
            "the concession is not made one turn early"
        );
        assert_eq!(
            tier_order(FULL_TIER_EVERY)[0],
            ScanMode::Full,
            "an unbroken run of fast tasks must eventually yield a turn"
        );
        // Both tiers are always offered; the debt reorders them, it never drops one.
        for streak in [0, FULL_TIER_EVERY] {
            let order = tier_order(streak);
            assert!(order.contains(&ScanMode::Fast) && order.contains(&ScanMode::Full));
        }
    }

    #[test]
    fn fast_scans_outrank_full_scans() {
        // `poll_round` exhausts each tier in `tier_order`, which starts from this array, so it
        // *is* the priority policy: a fast task is not left waiting behind a catalogue walk.
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

    #[test]
    fn a_lane_whose_provider_is_in_flight_is_passed_over() {
        // Lane 0 is busy, so the round serves lane 1 instead of waiting on it.
        let mut cursor = 0;
        assert_eq!(
            next_free_lane(&mut cursor, &[true, false, false]),
            Some(1),
            "a busy provider's lane must not be served"
        );
    }

    /// A skipped lane still spends its turn.
    ///
    /// The fairness rule, and the one that is silently wrong if it regresses: a provider that
    /// is already being served must not keep its place in the queue, or it is offered again
    /// the instant it frees — ahead of a provider that has been idle the whole time. Nothing
    /// fails loudly if this breaks; one provider simply gets scanned far more often than the
    /// rest.
    #[test]
    fn a_skipped_lane_does_not_keep_its_place_in_the_queue() {
        let mut cursor = 0;
        // Lane 0 busy: the round spends 0's turn and serves 1, so the cursor now points at 2.
        assert_eq!(next_free_lane(&mut cursor, &[true, false, false]), Some(1));
        assert_eq!(
            next_free_lane(&mut cursor, &[false, false, false]),
            Some(2),
            "the round after a skip must resume past the skipped lane, not at it"
        );
    }

    #[test]
    fn a_tier_with_every_provider_in_flight_serves_nothing() {
        // Returned without touching the broker: with no free lane there is nowhere to put a
        // message, and pulling one anyway is the hand-back that burns a delivery count.
        let mut cursor = 0;
        assert_eq!(next_free_lane(&mut cursor, &[true, true, true]), None);
    }

    #[test]
    fn a_tier_with_no_lanes_serves_nothing() {
        let mut cursor = 0;
        assert_eq!(next_free_lane(&mut cursor, &[]), None);
    }
}
