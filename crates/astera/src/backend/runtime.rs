//! Backend-independent event and repaint scheduling.
//!
//! Calloop callbacks enqueue typed events and request work here. They never render directly.
//! Keeping this state machine independent from DRM and winit makes its wakeup and fairness
//! invariants deterministic to test.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Instant,
};

use astera_core::OutputId;

const DEADLINE_PRIORITY_BURST: usize = 1;

/// Why an output needs another presentation opportunity.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RepaintReasons(u8);

impl RepaintReasons {
    pub const DAMAGE: Self = Self(1 << 0);
    pub const FRAME_CALLBACK: Self = Self(1 << 1);
    pub const ANIMATION: Self = Self(1 << 2);
    pub const CURSOR: Self = Self(1 << 3);
    pub const OUTPUT_CHANGE: Self = Self(1 << 4);
    pub const FULL_REPAINT: Self = Self(1 << 5);

    pub fn contains(self, reason: Self) -> bool {
        self.0 & reason.0 == reason.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn insert(&mut self, reason: Self) {
        self.0 |= reason.0;
    }
}

/// A backend-neutral item delivered by a calloop source.
#[derive(Debug)]
pub enum RuntimeEvent<Input, Command, BackendEvent> {
    Input(Input),
    IpcCommand(Command),
    SurfaceCommitted(OutputId),
    TimerFired(TimerKind),
    Backend(BackendEvent),
    Continue,
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerKind {
    Animation(OutputId),
    KeyRepeat,
    ConfigDebounce,
    IoTimeout(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputPhase {
    Idle,
    Scheduled,
    Rendering,
    AwaitingPresentation,
    Paused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderRequest {
    pub output: OutputId,
    pub frame_id: u64,
    pub reasons: RepaintReasons,
}

#[derive(Debug)]
struct OutputSchedule {
    phase: OutputPhase,
    reasons: RepaintReasons,
    dirty_after_present: RepaintReasons,
    deadline: Option<Instant>,
    immediate_pending: bool,
    dirty_immediate: bool,
    next_frame_id: u64,
    active_frame_id: Option<u64>,
}

impl Default for OutputSchedule {
    fn default() -> Self {
        Self {
            phase: OutputPhase::Idle,
            reasons: RepaintReasons::default(),
            dirty_after_present: RepaintReasons::default(),
            deadline: None,
            immediate_pending: false,
            dirty_immediate: false,
            next_frame_id: 1,
            active_frame_id: None,
        }
    }
}

/// Coalesces repaint requests while preserving per-output presentation backpressure.
#[derive(Debug, Default)]
pub struct RepaintScheduler {
    outputs: BTreeMap<OutputId, OutputSchedule>,
    ready: VecDeque<OutputId>,
    paused: bool,
    consecutive_deadline_priority: usize,
}

impl RepaintScheduler {
    pub fn add_output(&mut self, output: OutputId) {
        self.outputs
            .entry(output)
            .or_insert_with(|| OutputSchedule {
                phase: if self.paused {
                    OutputPhase::Paused
                } else {
                    OutputPhase::Idle
                },
                ..OutputSchedule::default()
            });
    }

    pub fn remove_output(&mut self, output: OutputId) {
        self.outputs.remove(&output);
        self.ready.retain(|candidate| *candidate != output);
    }

    pub fn phase(&self, output: OutputId) -> Option<OutputPhase> {
        self.outputs.get(&output).map(|state| state.phase)
    }

    /// Multiple requests before a frame are represented by one queue entry and a reason bitset.
    pub fn request(&mut self, output: OutputId, reason: RepaintReasons) {
        self.request_inner(output, reason, None);
    }

    fn request_inner(
        &mut self,
        output: OutputId,
        reason: RepaintReasons,
        deadline: Option<Instant>,
    ) {
        let Some(state) = self.outputs.get_mut(&output) else {
            return;
        };
        match state.phase {
            OutputPhase::Idle => {
                state.reasons.insert(reason);
                set_deadline(state, deadline, false);
                state.phase = OutputPhase::Scheduled;
                self.ready.push_back(output);
            }
            OutputPhase::Scheduled => {
                state.reasons.insert(reason);
                set_deadline(state, deadline, false);
            }
            OutputPhase::Rendering | OutputPhase::AwaitingPresentation | OutputPhase::Paused => {
                state.dirty_after_present.insert(reason);
                set_deadline(state, deadline, true);
            }
        }
    }

    pub fn request_at(&mut self, output: OutputId, reason: RepaintReasons, deadline: Instant) {
        self.request_inner(output, reason, Some(deadline));
    }

    pub fn earliest_deadline(&self) -> Option<Instant> {
        self.outputs
            .values()
            .filter(|state| state.phase == OutputPhase::Scheduled)
            .filter_map(|state| state.deadline)
            .min()
    }

    /// Returns at most `budget` outputs in round-robin order.
    pub fn begin_ready(&mut self, now: Instant, budget: usize) -> Vec<RenderRequest> {
        let candidates = self.ready.drain(..).collect::<Vec<_>>();
        let mut eligible = Vec::new();
        for (position, output) in candidates.iter().copied().enumerate() {
            let Some(state) = self.outputs.get(&output) else {
                continue;
            };
            if state.deadline.is_none_or(|deadline| deadline <= now) {
                eligible.push((position, output, state.deadline));
            }
        }
        // Missed deadlines go first. Stable queue position provides round-robin ordering for
        // equal deadlines and immediate work.
        eligible.sort_by_key(|(position, _, deadline)| (deadline.is_none(), *deadline, *position));
        if self.consecutive_deadline_priority >= DEADLINE_PRIORITY_BURST
            && let Some(immediate) = eligible
                .iter()
                .position(|(_, _, deadline)| deadline.is_none())
        {
            eligible.rotate_left(immediate);
        }
        let selected = eligible
            .into_iter()
            .take(budget)
            .map(|(_, output, _)| output)
            .collect::<Vec<_>>();
        let selected_set = selected.iter().copied().collect::<BTreeSet<_>>();
        for output in candidates {
            if !selected_set.contains(&output) && self.outputs.contains_key(&output) {
                self.ready.push_back(output);
            }
        }

        let mut requests = Vec::with_capacity(selected.len());
        for output in selected {
            let Some(state) = self.outputs.get_mut(&output) else {
                continue;
            };
            if state.deadline.is_some() {
                self.consecutive_deadline_priority += 1;
            } else {
                self.consecutive_deadline_priority = 0;
            }
            state.deadline = None;
            state.immediate_pending = false;
            state.phase = OutputPhase::Rendering;
            let reasons = std::mem::take(&mut state.reasons);
            let frame_id = state.next_frame_id;
            state.next_frame_id = state.next_frame_id.wrapping_add(1).max(1);
            state.active_frame_id = Some(frame_id);
            requests.push(RenderRequest {
                output,
                frame_id,
                reasons,
            });
        }
        requests
    }

    pub fn submitted(&mut self, output: OutputId, frame_id: u64) -> bool {
        let Some(state) = self.outputs.get_mut(&output) else {
            return false;
        };
        if state.phase != OutputPhase::Rendering || state.active_frame_id != Some(frame_id) {
            return false;
        }
        state.phase = OutputPhase::AwaitingPresentation;
        true
    }

    pub fn presented(&mut self, output: OutputId, frame_id: u64) -> bool {
        let Some(state) = self.outputs.get_mut(&output) else {
            return false;
        };
        if state.phase != OutputPhase::AwaitingPresentation
            || state.active_frame_id != Some(frame_id)
        {
            return false;
        }
        self.schedule_follow_up(output);
        true
    }

    pub fn failed(&mut self, output: OutputId, frame_id: u64) -> bool {
        let Some(state) = self.outputs.get_mut(&output) else {
            return false;
        };
        if !matches!(
            state.phase,
            OutputPhase::Rendering | OutputPhase::AwaitingPresentation
        ) || state.active_frame_id != Some(frame_id)
        {
            return false;
        }
        state
            .dirty_after_present
            .insert(RepaintReasons::FULL_REPAINT);
        // The first retry is immediate. A backend may explicitly schedule a later retry if it
        // exhausts its retry budget, but an unrelated animation deadline must not delay recovery.
        state.dirty_immediate = true;
        state.deadline = None;
        self.schedule_follow_up(output);
        true
    }

    /// Finish a failed render attempt and retry it at a backend-selected deadline.
    ///
    /// Native DRM uses this for empty frames (roughly one retrace later) and for
    /// transient KMS errors, avoiding both callback starvation and a busy retry loop.
    pub fn retry_at(&mut self, output: OutputId, frame_id: u64, deadline: Instant) -> bool {
        let Some(state) = self.outputs.get_mut(&output) else {
            return false;
        };
        if !matches!(
            state.phase,
            OutputPhase::Rendering | OutputPhase::AwaitingPresentation
        ) || state.active_frame_id != Some(frame_id)
        {
            return false;
        }
        state
            .dirty_after_present
            .insert(RepaintReasons::FULL_REPAINT);
        state.dirty_immediate = false;
        state.deadline = Some(deadline);
        self.schedule_follow_up(output);
        true
    }

    pub fn pause(&mut self) {
        self.paused = true;
        self.ready.clear();
        for state in self.outputs.values_mut() {
            if state.phase != OutputPhase::Idle {
                state.dirty_after_present.insert(state.reasons);
            }
            state.reasons = RepaintReasons::default();
            state.deadline = None;
            state.immediate_pending = false;
            state.dirty_immediate = false;
            state.active_frame_id = None;
            state.phase = OutputPhase::Paused;
        }
    }

    pub fn resume(&mut self) {
        self.paused = false;
        for (output, state) in &mut self.outputs {
            if state.phase != OutputPhase::Paused {
                continue;
            }
            state.reasons = std::mem::take(&mut state.dirty_after_present);
            state.reasons.insert(RepaintReasons::FULL_REPAINT);
            state.deadline = None;
            state.immediate_pending = true;
            state.dirty_immediate = false;
            state.phase = OutputPhase::Scheduled;
            self.ready.push_back(*output);
        }
    }

    pub fn has_ready_at(&self, now: Instant) -> bool {
        self.ready.iter().any(|output| {
            self.outputs
                .get(output)
                .is_some_and(|state| state.deadline.is_none_or(|deadline| deadline <= now))
        })
    }

    fn schedule_follow_up(&mut self, output: OutputId) {
        let state = self
            .outputs
            .get_mut(&output)
            .expect("output was checked before scheduling follow-up");
        state.active_frame_id = None;
        state.reasons = std::mem::take(&mut state.dirty_after_present);
        state.immediate_pending = std::mem::take(&mut state.dirty_immediate);
        if state.reasons.is_empty() {
            state.phase = OutputPhase::Idle;
        } else {
            state.phase = OutputPhase::Scheduled;
            self.ready.push_back(output);
        }
    }
}

fn set_deadline(state: &mut OutputSchedule, deadline: Option<Instant>, dirty: bool) {
    let immediate = if dirty {
        &mut state.dirty_immediate
    } else {
        &mut state.immediate_pending
    };
    match deadline {
        None => {
            *immediate = true;
            state.deadline = None;
        }
        Some(deadline) if !*immediate => {
            state.deadline = Some(
                state
                    .deadline
                    .map_or(deadline, |current| current.min(deadline)),
            );
        }
        Some(_) => {}
    }
}

/// Four-stage contract used by the runtime to avoid blocking on GPU or presentation work.
pub trait Backend {
    type FrameSnapshot;
    type PreparedFrame;
    type Fence;
    type Error;

    fn request_redraw(&mut self, output: OutputId);
    fn prepare(
        &mut self,
        request: RenderRequest,
        snapshot: Self::FrameSnapshot,
    ) -> Result<Prepared<Self::PreparedFrame, Self::Fence>, Self::Error>;
    fn submit(&mut self, frame: Self::PreparedFrame) -> Result<(), Self::Error>;
    fn pause(&mut self);
    fn resume(&mut self) -> Result<(), Self::Error>;
}

pub type Prepared<Frame, Fence> = (Frame, Option<Fence>);

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: OutputId = OutputId(1);
    const SECOND: OutputId = OutputId(2);

    #[test]
    fn requests_coalesce_and_keep_all_reasons() {
        let now = Instant::now();
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.request(FIRST, RepaintReasons::DAMAGE);
        scheduler.request(FIRST, RepaintReasons::FRAME_CALLBACK);

        let requests = scheduler.begin_ready(now, 4);
        assert_eq!(requests.len(), 1);
        assert!(requests[0].reasons.contains(RepaintReasons::DAMAGE));
        assert!(requests[0].reasons.contains(RepaintReasons::FRAME_CALLBACK));
    }

    #[test]
    fn dirty_while_pending_schedules_exactly_one_follow_up() {
        let now = Instant::now();
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.request(FIRST, RepaintReasons::DAMAGE);
        let frame = scheduler.begin_ready(now, 1)[0];
        assert!(scheduler.submitted(FIRST, frame.frame_id));

        scheduler.request(FIRST, RepaintReasons::CURSOR);
        scheduler.request(FIRST, RepaintReasons::DAMAGE);
        assert!(scheduler.presented(FIRST, frame.frame_id));

        let follow_up = scheduler.begin_ready(now, 4);
        assert_eq!(follow_up.len(), 1);
        assert!(follow_up[0].reasons.contains(RepaintReasons::CURSOR));
        assert!(follow_up[0].reasons.contains(RepaintReasons::DAMAGE));
    }

    #[test]
    fn dirty_while_rendering_is_not_lost() {
        let now = Instant::now();
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.request(FIRST, RepaintReasons::DAMAGE);
        let frame = scheduler.begin_ready(now, 1)[0];

        scheduler.request(FIRST, RepaintReasons::CURSOR);
        assert!(scheduler.submitted(FIRST, frame.frame_id));
        assert!(scheduler.presented(FIRST, frame.frame_id));

        let follow_up = scheduler.begin_ready(now, 1);
        assert_eq!(follow_up.len(), 1);
        assert!(follow_up[0].reasons.contains(RepaintReasons::CURSOR));
    }

    #[test]
    fn budget_rotates_ready_outputs_without_dropping_work() {
        let now = Instant::now();
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.add_output(SECOND);
        scheduler.request(FIRST, RepaintReasons::DAMAGE);
        scheduler.request(SECOND, RepaintReasons::DAMAGE);

        let first_batch = scheduler.begin_ready(now, 1);
        assert_eq!(first_batch[0].output, FIRST);
        assert!(scheduler.has_ready_at(now));
        let second_batch = scheduler.begin_ready(now, 1);
        assert_eq!(second_batch[0].output, SECOND);
    }

    #[test]
    fn deadline_uses_earliest_request_and_does_not_render_early() {
        let now = Instant::now();
        let later = now + std::time::Duration::from_millis(20);
        let earlier = now + std::time::Duration::from_millis(10);
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.request_at(FIRST, RepaintReasons::ANIMATION, later);
        scheduler.request_at(FIRST, RepaintReasons::FRAME_CALLBACK, earlier);

        assert_eq!(scheduler.earliest_deadline(), Some(earlier));
        assert!(scheduler.begin_ready(now, 1).is_empty());
        assert_eq!(scheduler.begin_ready(earlier, 1).len(), 1);
    }

    #[test]
    fn immediate_work_cancels_an_animation_deadline() {
        let now = Instant::now();
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.request_at(
            FIRST,
            RepaintReasons::ANIMATION,
            now + std::time::Duration::from_secs(1),
        );
        scheduler.request(FIRST, RepaintReasons::DAMAGE);

        let request = scheduler.begin_ready(now, 1)[0];
        assert!(request.reasons.contains(RepaintReasons::ANIMATION));
        assert!(request.reasons.contains(RepaintReasons::DAMAGE));
    }

    #[test]
    fn later_timed_request_does_not_delay_immediate_work() {
        let now = Instant::now();
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.request(FIRST, RepaintReasons::DAMAGE);
        scheduler.request_at(
            FIRST,
            RepaintReasons::ANIMATION,
            now + std::time::Duration::from_secs(1),
        );

        let request = scheduler.begin_ready(now, 1)[0];
        assert!(request.reasons.contains(RepaintReasons::DAMAGE));
        assert!(request.reasons.contains(RepaintReasons::ANIMATION));
    }

    #[test]
    fn missed_deadline_precedes_round_robin_ready_work() {
        let now = Instant::now();
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.add_output(SECOND);
        scheduler.request(FIRST, RepaintReasons::DAMAGE);
        scheduler.request_at(
            SECOND,
            RepaintReasons::ANIMATION,
            now - std::time::Duration::from_millis(10),
        );

        assert_eq!(scheduler.begin_ready(now, 1)[0].output, SECOND);
        assert_eq!(scheduler.begin_ready(now, 1)[0].output, FIRST);
    }

    #[test]
    fn repeated_missed_deadlines_cannot_starve_immediate_work() {
        let now = Instant::now();
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.add_output(SECOND);
        scheduler.request_at(
            FIRST,
            RepaintReasons::ANIMATION,
            now - std::time::Duration::from_millis(10),
        );
        scheduler.request(SECOND, RepaintReasons::DAMAGE);

        let deadline_frame = scheduler.begin_ready(now, 1)[0];
        assert_eq!(deadline_frame.output, FIRST);
        assert!(scheduler.failed(FIRST, deadline_frame.frame_id));
        scheduler.request_at(
            FIRST,
            RepaintReasons::ANIMATION,
            now - std::time::Duration::from_millis(10),
        );

        assert_eq!(scheduler.begin_ready(now, 1)[0].output, SECOND);
    }

    #[test]
    fn future_deadline_does_not_request_an_immediate_continuation() {
        let now = Instant::now();
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.request_at(
            FIRST,
            RepaintReasons::ANIMATION,
            now + std::time::Duration::from_secs(1),
        );

        assert!(!scheduler.has_ready_at(now));
        assert!(scheduler.has_ready_at(now + std::time::Duration::from_secs(1)));
    }

    #[test]
    fn pause_coalesces_work_and_resume_forces_full_repaint() {
        let now = Instant::now();
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.request(FIRST, RepaintReasons::DAMAGE);
        scheduler.pause();
        scheduler.request(FIRST, RepaintReasons::CURSOR);

        assert!(scheduler.begin_ready(now, 1).is_empty());
        scheduler.resume();
        let request = scheduler.begin_ready(now, 1)[0];
        assert!(request.reasons.contains(RepaintReasons::DAMAGE));
        assert!(request.reasons.contains(RepaintReasons::CURSOR));
        assert!(request.reasons.contains(RepaintReasons::FULL_REPAINT));
    }

    #[test]
    fn pause_clears_deadlines_and_hotplugged_output_repaints_on_resume() {
        let now = Instant::now();
        let future = now + std::time::Duration::from_secs(60);
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.request_at(FIRST, RepaintReasons::ANIMATION, future);
        scheduler.pause();
        scheduler.add_output(SECOND);

        assert_eq!(scheduler.earliest_deadline(), None);
        assert_eq!(scheduler.phase(SECOND), Some(OutputPhase::Paused));
        scheduler.resume();
        let requests = scheduler.begin_ready(now, 2);
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|request| request.reasons.contains(RepaintReasons::FULL_REPAINT))
        );
    }

    #[test]
    fn failed_frame_retries_as_full_repaint() {
        let now = Instant::now();
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.request(FIRST, RepaintReasons::DAMAGE);
        let frame = scheduler.begin_ready(now, 1)[0];

        assert!(scheduler.failed(FIRST, frame.frame_id));
        let retry = scheduler.begin_ready(now, 1)[0];
        assert!(retry.reasons.contains(RepaintReasons::FULL_REPAINT));
    }

    #[test]
    fn backend_can_defer_a_failed_frame_without_busy_retrying() {
        let now = Instant::now();
        let retry = now + std::time::Duration::from_millis(16);
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.request(FIRST, RepaintReasons::FRAME_CALLBACK);
        let frame = scheduler.begin_ready(now, 1)[0];

        assert!(scheduler.retry_at(FIRST, frame.frame_id, retry));
        assert!(!scheduler.has_ready_at(now));
        assert_eq!(scheduler.earliest_deadline(), Some(retry));
        let request = scheduler.begin_ready(retry, 1)[0];
        assert!(request.reasons.contains(RepaintReasons::FULL_REPAINT));
    }

    #[test]
    fn failed_frame_retry_is_not_delayed_by_pending_animation() {
        let now = Instant::now();
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.request(FIRST, RepaintReasons::DAMAGE);
        let frame = scheduler.begin_ready(now, 1)[0];
        scheduler.request_at(
            FIRST,
            RepaintReasons::ANIMATION,
            now + std::time::Duration::from_secs(1),
        );

        assert!(scheduler.failed(FIRST, frame.frame_id));
        assert_eq!(scheduler.begin_ready(now, 1).len(), 1);
    }

    #[test]
    fn stale_completion_cannot_finish_a_new_frame() {
        let now = Instant::now();
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.request(FIRST, RepaintReasons::DAMAGE);
        let first = scheduler.begin_ready(now, 1)[0];
        assert!(scheduler.failed(FIRST, first.frame_id));
        let second = scheduler.begin_ready(now, 1)[0];

        assert!(!scheduler.submitted(FIRST, first.frame_id));
        assert!(scheduler.submitted(FIRST, second.frame_id));
        assert!(!scheduler.presented(FIRST, first.frame_id));
        assert!(scheduler.presented(FIRST, second.frame_id));
    }

    #[test]
    fn maximum_frame_id_can_complete_before_wrapping() {
        let now = Instant::now();
        let mut scheduler = RepaintScheduler::default();
        scheduler.add_output(FIRST);
        scheduler.outputs.get_mut(&FIRST).unwrap().next_frame_id = u64::MAX;
        scheduler.request(FIRST, RepaintReasons::DAMAGE);

        let frame = scheduler.begin_ready(now, 1)[0];
        assert_eq!(frame.frame_id, u64::MAX);
        assert!(scheduler.submitted(FIRST, frame.frame_id));
        assert!(scheduler.presented(FIRST, frame.frame_id));
    }
}
