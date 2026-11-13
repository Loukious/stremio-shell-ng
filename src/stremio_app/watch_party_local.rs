#![cfg_attr(not(feature = "steam-sync"), allow(dead_code))]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::sync_protocol::{LocalPlaybackState, MediaIdentity};

static LOCAL_SYNC_REVISION: AtomicU64 = AtomicU64::new(1);

pub fn current_local_sync_revision() -> u64 {
    LOCAL_SYNC_REVISION.load(Ordering::Acquire)
}

/// Invalidate already queued player commands before changing a session or media.
pub fn invalidate_local_sync() -> u64 {
    LOCAL_SYNC_REVISION.fetch_add(1, Ordering::AcqRel) + 1
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LocalSyncCommand {
    pub revision: u64,
    pub file_epoch: u64,
    pub paused: bool,
    pub seek_to: Option<f64>,
}

impl LocalSyncCommand {
    pub fn validate(&self) -> bool {
        self.revision != 0
            && self.file_epoch != 0
            && self
                .seek_to
                .is_none_or(|position| position.is_finite() && position >= 0.0)
    }
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SelectionRequest {
    pub generation: u64,
    pub request_id: String,
    pub media: MediaIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestStateChange {
    Ignored,
    Updated,
    Select(SelectionRequest),
}

#[derive(Debug, Clone)]
struct ReceivedState {
    state: LocalPlaybackState,
    received_at: Instant,
}

/// Guest source selection and MPV readiness are independent of the Steam loop.
/// The caller supplies times/epochs, allowing all transitions to be tested offline.
#[derive(Debug, Default)]
pub struct GuestLocalPlayback {
    latest: Option<ReceivedState>,
    selection: Option<SelectionRequest>,
    selected: bool,
    file_epoch: Option<u64>,
    ready: bool,
    revision: u64,
    force_seek: bool,
    suspended: bool,
    barrier_generation: u64,
    barrier_sequence: u64,
}

impl GuestLocalPlayback {
    pub fn ingest(
        &mut self,
        state: LocalPlaybackState,
        received_at: Instant,
    ) -> Result<GuestStateChange, String> {
        state.validate()?;
        if self.is_stale(state.generation, state.sequence) {
            return Ok(GuestStateChange::Ignored);
        }
        if let Some(previous) = self.latest.as_ref() {
            if state.generation == previous.state.generation && state.media != previous.state.media
            {
                return Err("The host changed media without a new generation.".to_string());
            }
        }
        let needs_selection = self
            .selection
            .as_ref()
            .is_none_or(|selection| selection.generation != state.generation);
        let change = if needs_selection {
            let request = SelectionRequest {
                generation: state.generation,
                request_id: uuid::Uuid::new_v4().to_string(),
                media: state.media.clone(),
            };
            self.clear_selection();
            self.selection = Some(request.clone());
            GuestStateChange::Select(request)
        } else {
            GuestStateChange::Updated
        };
        if self.suspended {
            self.revision = invalidate_local_sync();
            self.force_seek = self.ready;
        }
        self.suspended = false;
        self.barrier_generation = state.generation;
        self.barrier_sequence = state.sequence;
        self.latest = Some(ReceivedState { state, received_at });
        Ok(change)
    }

    fn is_stale(&self, generation: u64, sequence: u64) -> bool {
        generation < self.barrier_generation || sequence <= self.barrier_sequence
    }

    /// The host has temporarily unloaded or cannot identify its current media.
    /// Keep a loaded guest's binding so a same-generation recovery can resume,
    /// while rejecting all synchronization until a fresh identified state arrives.
    pub fn waiting(&mut self, generation: u64, sequence: u64) -> Result<bool, String> {
        if generation == 0 || sequence == 0 {
            return Err("Watch-party media generation or sequence is invalid.".to_string());
        }
        if self.is_stale(generation, sequence) {
            return Ok(false);
        }
        self.barrier_generation = generation;
        self.barrier_sequence = sequence;
        self.suspended = true;
        self.force_seek = false;
        self.revision = invalidate_local_sync();
        Ok(true)
    }

    pub fn selection(&self) -> Option<&SelectionRequest> {
        self.selection.as_ref()
    }

    #[cfg(test)]
    pub fn latest_state(&self) -> Option<&LocalPlaybackState> {
        self.latest.as_ref().map(|latest| &latest.state)
    }

    /// The caller invokes this only for a new local stream selection. Duplicate
    /// Web reports must be deduplicated using their local selection ID.
    pub fn select(&mut self, media: &MediaIdentity, request_id: &str) -> bool {
        let Some(selection) = self.selection.as_ref() else {
            return false;
        };
        if selection.request_id != request_id {
            return false;
        }
        if selection.media != *media {
            self.clear_selection();
            return false;
        }
        self.clear_selection();
        self.selected = true;
        true
    }

    pub fn clear_selection(&mut self) {
        self.selected = false;
        self.file_epoch = None;
        self.ready = false;
        self.force_seek = false;
        self.revision = invalidate_local_sync();
    }

    /// Called when a new MPV load is accepted, before any FileLoaded event.
    pub fn loading(&mut self, file_epoch: u64) -> bool {
        self.ready = false;
        self.force_seek = false;
        self.revision = invalidate_local_sync();
        self.file_epoch = (self.selected && file_epoch != 0).then_some(file_epoch);
        self.file_epoch.is_some()
    }

    pub fn loaded(&mut self, file_epoch: u64) -> bool {
        if !self.selected || self.file_epoch != Some(file_epoch) || self.ready {
            return false;
        }
        self.ready = true;
        self.force_seek = true;
        self.revision = invalidate_local_sync();
        true
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn is_waiting(&self) -> bool {
        self.suspended
    }

    pub fn file_epoch(&self) -> Option<u64> {
        self.file_epoch
    }

    pub fn command(
        &mut self,
        now: Instant,
        local_time: f64,
        file_epoch: u64,
    ) -> Option<LocalSyncCommand> {
        if self.suspended || !self.ready || self.file_epoch != Some(file_epoch) {
            return None;
        }
        let latest = self.latest.as_ref()?;
        let elapsed = now
            .saturating_duration_since(latest.received_at)
            .as_secs_f64();
        let mut target = latest.state.time_pos;
        if !latest.state.paused {
            target += elapsed;
        }
        if latest.state.duration > 0.0 {
            target = target.min(latest.state.duration);
        }
        let seek = self.force_seek || !local_time.is_finite() || (local_time - target).abs() > 2.0;
        self.force_seek = false;
        Some(LocalSyncCommand {
            revision: self.revision,
            file_epoch,
            paused: latest.state.paused,
            seek_to: seek.then_some(target),
        })
    }

    pub fn clear(&mut self) {
        self.clear_selection();
        self.latest = None;
        self.selection = None;
        self.suspended = false;
        self.barrier_generation = 0;
        self.barrier_sequence = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn media(video_id: &str) -> MediaIdentity {
        MediaIdentity {
            content_type: "series".to_string(),
            metadata_id: "tt1234567".to_string(),
            video_id: video_id.to_string(),
        }
    }

    fn state(generation: u64, sequence: u64, video_id: &str) -> LocalPlaybackState {
        LocalPlaybackState {
            media: media(video_id),
            generation,
            sequence,
            time_pos: 42.0,
            duration: 3600.0,
            paused: false,
        }
    }

    fn ready(guest: &mut GuestLocalPlayback, state: LocalPlaybackState, at: Instant, epoch: u64) {
        let identity = state.media.clone();
        guest.ingest(state, at).unwrap();
        let request = guest.selection().unwrap().request_id.clone();
        assert!(guest.select(&identity, &request));
        assert!(guest.loading(epoch));
        assert!(guest.loaded(epoch));
    }

    #[test]
    fn late_join_waits_for_selection_and_current_file_loaded_then_catches_up() {
        let now = Instant::now();
        let mut guest = GuestLocalPlayback::default();
        let initial = state(1, 1, "tt1234567:1:1");
        assert!(matches!(
            guest.ingest(initial.clone(), now).unwrap(),
            GuestStateChange::Select(_)
        ));
        assert!(guest.command(now, 0.0, 5).is_none());
        assert!(!guest.loaded(5));
        let request = guest.selection().unwrap().request_id.clone();
        assert!(guest.select(&initial.media, &request));
        assert!(guest.loading(5));
        assert!(guest.command(now, 0.0, 5).is_none());
        assert!(!guest.loaded(4));
        assert!(guest.loaded(5));
        let command = guest.command(now + Duration::from_secs(8), 0.0, 5).unwrap();
        assert_eq!(command.seek_to, Some(50.0));
        assert!(!command.paused);
    }

    #[test]
    fn selection_delay_uses_latest_paused_state_instead_of_first_snapshot() {
        let now = Instant::now();
        let mut guest = GuestLocalPlayback::default();
        let initial = state(1, 1, "tt1234567:1:1");
        guest.ingest(initial.clone(), now).unwrap();
        let request = guest.selection().unwrap().request_id.clone();
        let mut paused = initial.clone();
        paused.sequence = 2;
        paused.time_pos = 100.0;
        paused.paused = true;
        assert_eq!(
            guest.ingest(paused, now).unwrap(),
            GuestStateChange::Updated
        );
        assert_eq!(guest.selection().unwrap().request_id, request);
        assert!(guest.select(&initial.media, &request));
        guest.loading(1);
        guest.loaded(1);
        let command = guest
            .command(now + Duration::from_secs(30), 0.0, 1)
            .unwrap();
        assert_eq!(command.seek_to, Some(100.0));
        assert!(command.paused);
    }

    #[test]
    fn episode_change_rejects_old_packets_selections_and_file_loaded_events() {
        let now = Instant::now();
        let mut guest = GuestLocalPlayback::default();
        let initial = state(1, 1, "tt1234567:1:1");
        ready(&mut guest, initial.clone(), now, 1);
        let old_request = guest.selection().unwrap().request_id.clone();
        let next = state(2, 3, "tt1234567:1:2");
        guest.ingest(next.clone(), now).unwrap();
        let new_request = guest.selection().unwrap().request_id.clone();
        assert_ne!(old_request, new_request);
        assert!(!guest.select(&initial.media, &old_request));
        assert!(!guest.loaded(1));
        assert!(guest.command(now, 42.0, 1).is_none());
        assert_eq!(
            guest.ingest(initial, now).unwrap(),
            GuestStateChange::Ignored
        );
        assert!(guest.select(&next.media, &new_request));
        guest.loading(2);
        assert!(!guest.loaded(1));
        assert!(guest.loaded(2));
        assert!(guest.command(now, 42.0, 1).is_none());
        assert!(guest.command(now, 42.0, 2).is_some());
    }

    #[test]
    fn wrong_source_clears_ready_but_stale_request_does_not_disrupt_new_source() {
        let now = Instant::now();
        let mut guest = GuestLocalPlayback::default();
        let initial = state(1, 1, "tt1234567:1:1");
        ready(&mut guest, initial.clone(), now, 4);
        assert!(!guest.select(&media("tt1234567:1:2"), "old-request"));
        assert!(guest.is_ready());
        let request = guest.selection().unwrap().request_id.clone();
        assert!(!guest.select(&media("tt1234567:1:2"), &request));
        assert!(!guest.is_ready());
        assert!(!guest.loading(5));
        assert!(!guest.loaded(5));
        assert!(guest.command(now, 42.0, 4).is_none());
    }

    #[test]
    fn same_episode_reload_waits_for_new_epoch_and_forces_one_resume_seek() {
        let now = Instant::now();
        let mut guest = GuestLocalPlayback::default();
        ready(&mut guest, state(1, 1, "tt1234567:1:1"), now, 7);
        let first = guest.command(now, 42.0, 7).unwrap();
        assert_eq!(first.seek_to, Some(42.0));
        assert_eq!(guest.command(now, 42.0, 7).unwrap().seek_to, None);
        assert!(guest.loading(8));
        assert!(guest.command(now, 42.0, 7).is_none());
        assert!(!guest.loaded(7));
        assert!(guest.loaded(8));
        assert!(!guest.loaded(8));
        let next = guest.command(now, 42.0, 8).unwrap();
        assert_ne!(first.revision, next.revision);
        assert_eq!(next.seek_to, Some(42.0));
    }

    #[test]
    fn invalid_and_out_of_order_states_cannot_replace_active_media() {
        let now = Instant::now();
        let mut guest = GuestLocalPlayback::default();
        let initial = state(2, 10, "tt1234567:1:2");
        ready(&mut guest, initial.clone(), now, 1);
        assert_eq!(
            guest.ingest(initial.clone(), now).unwrap(),
            GuestStateChange::Ignored
        );
        assert_eq!(
            guest.ingest(state(3, 9, "tt1234567:1:3"), now).unwrap(),
            GuestStateChange::Ignored
        );
        assert!(guest.ingest(state(2, 11, "tt1234567:1:3"), now).is_err());
        let mut invalid = initial.clone();
        invalid.sequence = 11;
        invalid.time_pos = f64::NAN;
        assert!(guest.ingest(invalid, now).is_err());
        assert_eq!(guest.latest_state(), Some(&initial));
        assert!(guest.is_ready());
    }

    #[test]
    fn leaving_clears_state_and_cannot_apply_late_load_notifications() {
        let now = Instant::now();
        let mut guest = GuestLocalPlayback::default();
        ready(&mut guest, state(1, 1, "tt1234567:1:1"), now, 1);
        guest.clear();
        assert!(guest.latest_state().is_none());
        assert!(guest.selection().is_none());
        assert!(!guest.loaded(1));
        assert!(guest.command(now, 42.0, 1).is_none());
    }

    #[test]
    fn waiting_before_first_state_sets_ordering_barriers_without_selecting_media() {
        let now = Instant::now();
        let mut guest = GuestLocalPlayback::default();
        assert!(guest.waiting(2, 5).unwrap());
        assert!(guest.is_waiting());
        assert!(guest.selection().is_none());
        assert!(!guest.is_ready());
        assert!(guest.command(now, 0.0, 1).is_none());
        assert_eq!(
            guest.ingest(state(1, 6, "tt1234567:1:1"), now).unwrap(),
            GuestStateChange::Ignored
        );
        assert_eq!(
            guest.ingest(state(2, 4, "tt1234567:1:2"), now).unwrap(),
            GuestStateChange::Ignored
        );
        assert!(matches!(
            guest.ingest(state(2, 6, "tt1234567:1:2"), now).unwrap(),
            GuestStateChange::Select(_)
        ));
        assert!(!guest.is_waiting());
        assert!(guest.waiting(0, 1).is_err());
        assert!(guest.waiting(1, 0).is_err());
    }

    #[test]
    fn host_waiting_keeps_loaded_binding_and_resumes_same_episode_without_a_chooser() {
        let now = Instant::now();
        let mut guest = GuestLocalPlayback::default();
        let initial = state(1, 1, "tt1234567:1:1");
        ready(&mut guest, initial.clone(), now, 7);
        let request = guest.selection().unwrap().request_id.clone();
        let first = guest.command(now, 42.0, 7).unwrap();
        assert!(guest.waiting(1, 2).unwrap());
        assert!(guest.is_ready());
        assert_eq!(guest.file_epoch(), Some(7));
        assert!(guest
            .command(now + Duration::from_secs(20), 42.0, 7)
            .is_none());
        let mut resumed = initial;
        resumed.sequence = 3;
        resumed.time_pos = 48.0;
        assert_eq!(
            guest
                .ingest(resumed, now + Duration::from_secs(20))
                .unwrap(),
            GuestStateChange::Updated
        );
        assert_eq!(guest.selection().unwrap().request_id, request);
        let command = guest
            .command(now + Duration::from_secs(22), 42.0, 7)
            .unwrap();
        assert_eq!(command.seek_to, Some(50.0));
        assert_ne!(command.revision, first.revision);
        assert!(!guest.is_waiting());
    }

    #[test]
    fn stale_waiting_or_pre_waiting_state_cannot_suspend_or_resume_newer_media() {
        let now = Instant::now();
        let mut guest = GuestLocalPlayback::default();
        ready(&mut guest, state(2, 10, "tt1234567:1:2"), now, 2);
        assert!(!guest.waiting(1, 11).unwrap());
        assert!(!guest.waiting(2, 9).unwrap());
        assert!(!guest.is_waiting());
        assert!(guest.command(now, 42.0, 2).is_some());
        assert!(guest.waiting(2, 11).unwrap());
        assert_eq!(
            guest.ingest(state(2, 10, "tt1234567:1:2"), now).unwrap(),
            GuestStateChange::Ignored
        );
        assert!(guest.is_waiting());
        assert!(guest.command(now, 42.0, 2).is_none());
        assert_eq!(
            guest.ingest(state(3, 12, "tt1234567:1:3"), now).unwrap(),
            GuestStateChange::Select(guest.selection().unwrap().clone())
        );
        assert!(!guest.waiting(2, 13).unwrap());
        assert!(!guest.is_waiting());
        assert!(!guest.is_ready());
    }

    #[test]
    fn reload_during_host_waiting_stays_suspended_until_identified_state_resumes() {
        let now = Instant::now();
        let mut guest = GuestLocalPlayback::default();
        ready(&mut guest, state(1, 1, "tt1234567:1:1"), now, 1);
        guest.waiting(1, 2).unwrap();
        assert!(guest.loading(2));
        assert!(!guest.loaded(1));
        assert!(guest.loaded(2));
        assert!(guest.is_ready());
        assert!(guest.is_waiting());
        assert!(guest.command(now, 42.0, 2).is_none());
        assert_eq!(
            guest.ingest(state(1, 3, "tt1234567:1:1"), now).unwrap(),
            GuestStateChange::Updated
        );
        assert_eq!(guest.command(now, 42.0, 2).unwrap().seek_to, Some(42.0));
        guest.clear();
        assert!(!guest.is_waiting());
        assert!(matches!(
            guest.ingest(state(1, 1, "tt1234567:1:1"), now).unwrap(),
            GuestStateChange::Select(_)
        ));
    }
}
