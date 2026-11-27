// Copyright 2026 Deno Land Inc. Apache-2.0 license.

//! Memory-pressure shedding as a pure classifier of a memory sample plus the
//! prior shedding state (the hysteresis latch). No I/O, no clock — the env
//! read that builds the config stays at the edge (`main.rs`), and so does the
//! measurement, which has to ask both the operating system and the allocator.
//!
//! Residency is deliberately *not* here. A node's cell count is a hard cap
//! enforced at admission ([`crate::State::has_capacity`]), self-limiting and
//! known exactly; it is not a resource that needs a proactive walk down. This
//! classifier answers only the other question — "is this node out of memory
//! and must give cells back to recover?" — which a cell count cannot answer.
//! Conflating the two produced the placement churn and the admission wedge;
//! splitting them is what keeps each decision small.

/// Resource watermarks. Built once from the environment by the caller; the
/// core never reads the environment itself. Residency has no watermark here —
/// it is capped at admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PressureConfig {
    /// The ceiling on the memory the cells hold. This is the ordinary limit,
    /// and `CELLD_MAX_RSS_MB` sets it.
    pub high_bytes: Option<u64>,
    /// An absolute ceiling on the cgroup charge, or process RSS when the
    /// process has no readable cgroup: 95% of the available memory.
    ///
    /// `high_bytes` applies to allocator-adjusted RSS and to the cgroup charge
    /// excluding file cache after the same allocator adjustment. This lets a
    /// node recover after a free, and the cgroup measurement includes
    /// kernel charges outside RSS. Neither measurement includes every charge
    /// that the cgroup limit uses.
    ///
    /// This is the floor under that trade. A container reads
    /// `memory.current`, so kernel charges outside process RSS cannot hide
    /// from it.
    ///
    /// It is a fixed share of the machine and is never derived from
    /// `high_bytes`. A first attempt placed it "at least 125% of the ceiling",
    /// which put it at exactly 100% of the machine for the default ceiling of
    /// 80% -- above anything the kernel would let the process reach, so the
    /// floor did not exist in the configuration that ships. See
    /// [`PressureConfig::from_limits`].
    pub rss_hard_bytes: Option<u64>,
}

/// Which ceilings are currently latched. Each one engages at its own ceiling
/// and releases at its own low watermark, so one crossing can never hold the
/// node against the other's watermark.
///
/// This is two booleans rather than the reported reason, because the reason
/// names only the more serious crossing. Deriving the latches from it lets a
/// hard-cap crossing arm the ordinary ceiling, and then a node stays closed on
/// a ceiling it never crossed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Latches {
    pub memory: bool,
    pub rss_hard: bool,
}

/// Which measurement a walk down is working against.
///
/// The futility stop compares a sample with the one the last cut measured, so
/// it has to know that the two samples are the same measurement. They are not
/// interchangeable: eviction moves the ordinary working-set figure at once and
/// may never move the complete cgroup charge at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    /// The greater of allocator-adjusted RSS and the allocator-adjusted
    /// cgroup charge excluding file cache. An eviction can return this memory.
    InUse,
    /// The complete cgroup charge, with process RSS as a host fallback.
    Rss,
}

/// A resource sample — the only input the classifier reads. `resident_cells`
/// is carried so a resource trigger can size its walk down as a proportion of
/// what is actually resident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Load {
    pub resident_cells: usize,
    /// Reported to peers and operators, never decided on.
    pub rss_bytes: u64,
    /// RSS minus the pages the allocator keeps but nothing uses. This is the
    /// ordinary pressure fallback when no cgroup working set is readable.
    pub in_use_bytes: u64,
    /// The cgroup charge after active and inactive file pages from `memory.stat`
    /// are removed, retaining shmem/tmpfs. `None` outside a readable Linux memory cgroup.
    pub cgroup_working_set_bytes: Option<u64>,
    /// The complete `memory.current` charge. This is the number the cgroup
    /// limit constrains, including charges that process RSS does not contain.
    /// `None` outside a readable Linux memory cgroup.
    pub cgroup_current_bytes: Option<u64>,
    /// Memory the node commits to its containers, outside this process.
    ///
    /// A container runs in its own cgroup, a sibling of the node's, so its
    /// memory is in neither the process RSS nor the node's cgroup charge. It
    /// is real memory on the machine all the same, and a container can grow
    /// to its instance-type cap at any moment, so the node counts each
    /// running container's cap as committed. Both measurements add it: the
    /// ordinary ceiling so the node stops taking cells before the machine
    /// fills, and the hard cap so the absolute limit sees the whole charge.
    pub container_reserved_bytes: u64,
}

impl Load {
    /// The ordinary pressure measurement excludes reclaimable file cache but
    /// retains shmem/tmpfs and kernel charges that process RSS omits. Subtract
    /// the same measured allocator slack, or retained pages would recreate the admission
    /// wedge through the cgroup path. The process measurement remains the floor
    /// because cgroup and RSS accounting can differ for shared pages.
    pub fn memory_bytes(self) -> u64 {
        self.in_use_bytes
            .max(self.cgroup_in_use_bytes().unwrap_or_default())
            .saturating_add(self.container_reserved_bytes)
    }

    fn cgroup_in_use_bytes(self) -> Option<u64> {
        let allocator_slack = self.rss_bytes.saturating_sub(self.in_use_bytes);
        self.cgroup_working_set_bytes
            .map(|bytes| bytes.saturating_sub(allocator_slack))
    }

    /// The hard pressure measurement. `memory.current` is authoritative when
    /// present because the cgroup limit constrains it, not process RSS.
    pub fn hard_bytes(self) -> u64 {
        self.cgroup_current_bytes
            .unwrap_or(self.rss_bytes)
            .saturating_add(self.container_reserved_bytes)
    }

    pub fn metric_bytes(self, metric: Metric) -> u64 {
        match metric {
            Metric::InUse => self.memory_bytes(),
            Metric::Rss => self.hard_bytes(),
        }
    }
}

/// The latch engaged because the memory the cells hold crossed the ceiling.
/// This is the ordinary case, and shedding relieves it.
pub const SHED_MEMORY: &str = "memory";

/// The latch engaged because the complete cgroup charge, or the RSS fallback,
/// crossed the absolute cap. The string keeps its established telemetry name,
/// but a Linux container decides it from `memory.current`.
pub const SHED_RSS_HARD: &str = "rss-hard";

const HARD_CAP_PERCENT: u64 = 95;
const HARD_CAP_EARLY_RESUME_PERCENT: u64 = 90;

impl PressureConfig {
    /// Build the watermarks from the machine and the operator's setting.
    ///
    /// The policy lives here, not at the edge, because it is arithmetic with a
    /// correctness argument and requires explicit validation. The shell only
    /// supplies the two facts it can read: how much memory the process can use, and what
    /// `CELLD_MAX_RSS_MB` says. `None` for the setting means the default, and
    /// `Some(0)` disables pressure shedding altogether.
    ///
    /// The absolute cap is 95% of the machine and is never derived from the
    /// ceiling. A cap derived from the ceiling either lands above the machine
    /// (so it never fires) or below the ceiling (so it fires first on every
    /// sample and the classifier is reading the hard measurement again). Both
    /// have shipped in this file; neither is a floor.
    ///
    /// When the machine size is unknown, the cap falls back to 125% of an
    /// explicit ceiling. That is worse than a share of the machine and better
    /// than no floor at all.
    pub fn from_limits(total_memory_bytes: Option<u64>, max_rss_mb: Option<u64>) -> Self {
        // A watermark of zero is worse than no watermark: every sample crosses
        // it and none can clear it, so the node latches on its first sample and
        // refuses every cell for its lifetime. A machine that reports nothing
        // usable therefore gets no watermark at all. Linux reports 0 for a
        // cgroup limit of `0` and for an unreadable `MemTotal`.
        let usable = |bytes: u64| (bytes > 0).then_some(bytes);
        let high_bytes = match max_rss_mb {
            None => total_memory_bytes.and_then(|total| usable(total / 5 * 4)),
            Some(0) => None,
            Some(megabytes) => usable(megabytes.saturating_mul(1024 * 1024)),
        };
        let rss_hard_bytes = high_bytes.and_then(|high| {
            // `checked_mul`, because the fallback multiplies an operator's
            // number: a saturating multiply followed by a divide would return a
            // cap *below* the ceiling, which is the shape this avoids.
            let fallback = high.checked_mul(5).map_or(u64::MAX, |scaled| scaled / 4);
            let cap = total_memory_bytes
                .and_then(|total| usable(total / 100 * HARD_CAP_PERCENT))
                .unwrap_or(fallback);
            usable(cap)
        });
        Self {
            high_bytes,
            rss_hard_bytes,
        }
    }

    /// Does the ceiling sit at or above the absolute cap?
    ///
    /// Then the cap is the effective limit and the node decides on its complete
    /// cgroup charge or RSS fallback. That is the operator's
    /// choice -- they asked for a ceiling above the safety floor -- but it
    /// gives up the recovery property of this module, so the shell reports it.
    pub fn ceiling_above_cap(self) -> bool {
        match (self.high_bytes, self.rss_hard_bytes) {
            (Some(high), Some(cap)) => high >= cap,
            _ => false,
        }
    }

    /// Fold a sample into the latches, and say why the node is shedding.
    ///
    /// Each ceiling latches on its own crossing: it engages at its ceiling and
    /// releases at its own low watermark. The hard cap can also release at
    /// 90% of the machine when cache-adjusted pressure is low. The hard cap
    /// is reported first, because it is the more serious of the two.
    ///
    /// The latches are carried separately and not derived from the reported
    /// reason. Sharing them lets one crossing hold the node against the other's
    /// watermark, in either direction: a crossing of the ordinary ceiling then
    /// holds the node on a resident-set watermark it did not cross,
    /// and a hard-cap crossing then holds it on a ceiling it never crossed.
    pub fn classify(self, s: Load, was: Latches) -> (Latches, Option<&'static str>) {
        let over = |ceiling: Option<u64>, metric: Metric, latched: bool| {
            let bytes = s.metric_bytes(metric);
            ceiling.is_some_and(|c| {
                bytes >= c
                    || (latched && self.resume_line(metric, s).is_some_and(|line| bytes > line))
            })
        };
        let latches = Latches {
            memory: over(self.high_bytes, Metric::InUse, was.memory),
            rss_hard: over(self.rss_hard_bytes, Metric::Rss, was.rss_hard),
        };
        let reason = if latches.rss_hard {
            Some(SHED_RSS_HARD)
        } else if latches.memory {
            Some(SHED_MEMORY)
        } else {
            None
        };
        (latches, reason)
    }

    /// The ordinary low watermark: 80% of the ceiling.
    fn low_watermark(ceiling: u64) -> u64 {
        ceiling.saturating_mul(4) / 5
    }

    /// Whether the sample leaves the reserve below every configured low
    /// watermark. A rollout uses this stricter state instead of `!pressured`:
    /// a node can be below the high watermark without enough room to absorb a
    /// donor. A disabled ceiling imposes no headroom condition.
    pub fn has_headroom(self, sample: Load) -> bool {
        let below = |ceiling: Option<u64>, bytes: u64| {
            ceiling.is_none_or(|ceiling| bytes <= Self::low_watermark(ceiling))
        };
        below(self.high_bytes, sample.memory_bytes())
            && below(self.rss_hard_bytes, sample.hard_bytes())
    }

    /// The release line for the latch and the walk down against `metric`.
    ///
    /// `None` means no ceiling is configured for that measurement, so nothing
    /// can latch on it and no walk down works against it.
    pub fn resume_line(self, metric: Metric, sample: Load) -> Option<u64> {
        self.resume_line_for(
            metric,
            sample
                .cgroup_working_set_bytes
                .map(|_| sample.memory_bytes()),
        )
    }

    fn resume_line_for(self, metric: Metric, adjusted: Option<u64>) -> Option<u64> {
        match metric {
            Metric::InUse => self.high_bytes.map(Self::low_watermark),
            Metric::Rss => self.rss_hard_bytes.map(|ceiling| {
                if adjusted
                    .zip(self.high_bytes)
                    .is_some_and(|(bytes, high)| bytes < Self::low_watermark(high))
                {
                    (u128::from(ceiling) * u128::from(HARD_CAP_EARLY_RESUME_PERCENT)
                        / u128::from(HARD_CAP_PERCENT)) as u64
                } else {
                    Self::low_watermark(ceiling)
                }
            }),
        }
    }

    /// Can the previous cut's measured yield bring the remaining cells to the release line?
    pub fn can_reach_resume_line(
        self,
        metric: Metric,
        cut: Load,
        sample: Load,
        remaining: usize,
    ) -> bool {
        let removed = cut.resident_cells.saturating_sub(remaining);
        if removed == 0 {
            // A landed cut can vacate nothing only when there were no cells to evict.
            return true;
        }
        let project = |before: u64, now: u64| {
            let returned = before.saturating_sub(now);
            // Scale before dividing so sub-byte per-cell yields are not rounded away.
            let projected = u128::from(returned) * remaining as u128 / removed as u128;
            u128::from(now).saturating_sub(projected) as u64
        };
        let floor = project(cut.metric_bytes(metric), sample.metric_bytes(metric));
        let adjusted_floor = sample.cgroup_in_use_bytes().map(|cgroup_now| {
            cut.cgroup_in_use_bytes().map_or_else(
                || sample.memory_bytes(),
                |cgroup_before| {
                    let project_committed = |before: u64, now: u64| {
                        project(
                            before.saturating_add(cut.container_reserved_bytes),
                            now.saturating_add(sample.container_reserved_bytes),
                        )
                    };
                    project_committed(cgroup_before, cgroup_now)
                        .max(project_committed(cut.in_use_bytes, sample.in_use_bytes))
                },
            )
        });
        self.resume_line_for(metric, adjusted_floor)
            .is_some_and(|line| floor <= line)
    }

    /// Which measurement the walk down must work against, given the latches.
    ///
    /// The ordinary ceiling wins whenever it is latched, even if the cap is
    /// latched too. Eviction relieves the ordinary ceiling and may do nothing
    /// at all for the cap, so choosing the cap's measurement while the node is
    /// genuinely over its ordinary ceiling stops the walk down exactly when it
    /// is needed.
    pub fn walk_metric(latches: Latches) -> Metric {
        if latches.memory {
            Metric::InUse
        } else {
            Metric::Rss
        }
    }

    /// How far to shed down to. A memory trigger takes a proportion of what was
    /// just measured because the effect of an eviction is not visible until
    /// the next sample.
    pub fn release_target(resident_cells: usize) -> usize {
        resident_cells.saturating_sub((resident_cells / 10).max(1))
    }
}

pub const MAX_OUTBOUND_PIN_PERCENT: usize = 50;

/// May another cell be pinned resident by an outbound WebSocket?
///
/// An outbound socket is not hibernatable, so eviction refuses its cell for as
/// long as it is open. That is correct — a live host transport cannot survive
/// eviction — but it means every pinned cell is removed from the eviction
/// pool. Pin the whole ceiling and a resource walk down has nothing to
/// nominate. The budget is node-wide, counted in pinned *cells* rather than
/// sockets, because one socket is enough to pin. `ceiling` is the hard resident
/// cap.
pub fn may_pin_outbound(pinned_cells: usize, ceiling: Option<usize>) -> bool {
    ceiling.is_none_or(|cap| {
        // At least one, always. The share alone rounds to zero below a ceiling
        // of two, which would make an outbound socket impossible on a small
        // node rather than merely budgeted.
        pinned_cells < (cap.saturating_mul(MAX_OUTBOUND_PIN_PERCENT) / 100).max(1)
    })
}
