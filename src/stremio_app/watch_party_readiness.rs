#![cfg_attr(not(feature = "steam-sync"), allow(dead_code))]

use std::collections::BTreeMap;

use super::sync_protocol::{
    GuestReadiness, MediaIdentity, MemberReadiness, PartyReadiness, ReadinessStatus,
};

#[derive(Debug, Clone)]
struct MemberState {
    supported: bool,
    status: Option<ReadinessStatus>,
    last_report_sequence: u64,
}

/// The host owns the membership list; an incoming report cannot add a peer.
/// Report sequence barriers belong to each peer and reset for a new episode.
#[derive(Debug, Default)]
pub struct HostReadinessRoster {
    media: Option<MediaIdentity>,
    generation: u64,
    sequence: u64,
    members: BTreeMap<u64, MemberState>,
}

impl HostReadinessRoster {
    fn next_sequence(&self) -> Result<u64, String> {
        self.sequence
            .checked_add(1)
            .ok_or_else(|| "Watch-party readiness sequence exhausted.".to_string())
    }

    pub fn set_media(&mut self, media: MediaIdentity, generation: u64) -> Result<bool, String> {
        media.validate()?;
        if generation == 0 {
            return Err("Watch-party readiness generation is invalid.".to_string());
        }
        if generation < self.generation {
            return Ok(false);
        }
        if generation == self.generation {
            if self.media.as_ref() == Some(&media) {
                return Ok(false);
            }
            return Err("The host changed readiness media without a new generation.".to_string());
        }
        let sequence = self.next_sequence()?;
        self.media = Some(media);
        self.generation = generation;
        self.sequence = sequence;
        for member in self.members.values_mut() {
            member.status = member.supported.then_some(ReadinessStatus::Selecting);
            member.last_report_sequence = 0;
        }
        Ok(true)
    }

    pub fn add_member(&mut self, steam_id: u64, supported: bool) -> Result<bool, String> {
        if steam_id == 0 {
            return Err("Watch-party readiness Steam ID is invalid.".to_string());
        }
        if self
            .members
            .get(&steam_id)
            .is_some_and(|member| member.supported == supported)
        {
            return Ok(false);
        }
        if !self.members.contains_key(&steam_id) && self.members.len() >= 16 {
            return Err("Watch-party readiness member limit reached.".to_string());
        }
        let sequence = self.next_sequence()?;
        let last_report_sequence = self
            .members
            .get(&steam_id)
            .map_or(0, |member| member.last_report_sequence);
        self.members.insert(
            steam_id,
            MemberState {
                supported,
                status: (supported && self.media.is_some()).then_some(ReadinessStatus::Selecting),
                last_report_sequence,
            },
        );
        self.sequence = sequence;
        Ok(true)
    }

    pub fn remove_member(&mut self, steam_id: u64) -> Result<bool, String> {
        if !self.members.contains_key(&steam_id) {
            return Ok(false);
        }
        let sequence = self.next_sequence()?;
        self.members.remove(&steam_id);
        self.sequence = sequence;
        Ok(true)
    }

    /// Used for the host's own native state, whose source is not a guest report.
    pub fn set_status(&mut self, steam_id: u64, status: ReadinessStatus) -> Result<bool, String> {
        let Some(member) = self.members.get(&steam_id) else {
            return Ok(false);
        };
        if !member.supported || self.media.is_none() || member.status == Some(status) {
            return Ok(false);
        }
        let sequence = self.next_sequence()?;
        self.members.get_mut(&steam_id).unwrap().status = Some(status);
        self.sequence = sequence;
        Ok(true)
    }

    pub fn report(&mut self, steam_id: u64, report: &GuestReadiness) -> Result<bool, String> {
        report.validate()?;
        if report.generation != self.generation || self.media.as_ref() != Some(&report.media) {
            return Ok(false);
        }
        let Some(member) = self.members.get(&steam_id) else {
            return Ok(false);
        };
        if !member.supported || report.sequence <= member.last_report_sequence {
            return Ok(false);
        }
        let changed = member.status != Some(report.status);
        let sequence = if changed {
            self.next_sequence()?
        } else {
            self.sequence
        };
        let member = self.members.get_mut(&steam_id).unwrap();
        member.last_report_sequence = report.sequence;
        member.status = Some(report.status);
        self.sequence = sequence;
        Ok(changed)
    }

    pub fn snapshot(&self) -> Option<PartyReadiness> {
        if self.members.is_empty() {
            return None;
        }
        Some(PartyReadiness {
            media: self.media.clone()?,
            generation: self.generation,
            sequence: self.sequence,
            members: self
                .members
                .iter()
                .map(|(&steam_id, member)| MemberReadiness {
                    steam_id,
                    status: member.status,
                })
                .collect(),
        })
    }
}

/// The invitation's host is authenticated by the transport. This additional
/// gate ensures a roster refers to the guest's current host-selected episode.
#[derive(Debug, Default)]
pub struct GuestReadinessRoster {
    latest: Option<PartyReadiness>,
}

impl GuestReadinessRoster {
    pub fn ingest(
        &mut self,
        state: PartyReadiness,
        media: &MediaIdentity,
        generation: u64,
    ) -> Result<bool, String> {
        state.validate()?;
        if state.generation != generation || state.media != *media {
            return Ok(false);
        }
        if self.latest.as_ref().is_some_and(|previous| {
            state.generation < previous.generation || state.sequence <= previous.sequence
        }) {
            return Ok(false);
        }
        self.latest = Some(state);
        Ok(true)
    }

    pub fn current(&self, media: &MediaIdentity, generation: u64) -> Option<&PartyReadiness> {
        self.latest
            .as_ref()
            .filter(|latest| latest.media == *media && latest.generation == generation)
    }

    pub fn clear(&mut self) {
        self.latest = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(episode: u8) -> MediaIdentity {
        MediaIdentity {
            content_type: "series".to_string(),
            metadata_id: "tt1234567".to_string(),
            video_id: format!("tt1234567:1:{}", episode),
        }
    }

    fn report(
        generation: u64,
        sequence: u64,
        episode: u8,
        status: ReadinessStatus,
    ) -> GuestReadiness {
        GuestReadiness {
            media: media(episode),
            generation,
            sequence,
            status,
        }
    }

    fn party() -> HostReadinessRoster {
        let mut host = HostReadinessRoster::default();
        host.add_member(1, true).unwrap();
        host.add_member(2, true).unwrap();
        host.add_member(3, false).unwrap();
        host.set_media(media(1), 1).unwrap();
        host.set_status(1, ReadinessStatus::Ready).unwrap();
        host
    }

    fn status(host: &HostReadinessRoster, steam_id: u64) -> Option<ReadinessStatus> {
        host.snapshot()
            .unwrap()
            .members
            .into_iter()
            .find(|member| member.steam_id == steam_id)
            .unwrap()
            .status
    }

    #[test]
    fn source_reload_loading_cannot_be_overridden_by_delayed_ready() {
        let mut host = party();
        assert!(host
            .report(2, &report(1, 1, 1, ReadinessStatus::Loading))
            .unwrap());
        assert!(host
            .report(2, &report(1, 2, 1, ReadinessStatus::Ready))
            .unwrap());
        let delayed = report(1, 2, 1, ReadinessStatus::Ready);
        assert!(host
            .report(2, &report(1, 3, 1, ReadinessStatus::Loading))
            .unwrap());
        assert!(!host.report(2, &delayed).unwrap());
        assert_eq!(status(&host, 2), Some(ReadinessStatus::Loading));
        assert!(host
            .report(2, &report(1, 4, 1, ReadinessStatus::Ready))
            .unwrap());
        assert_eq!(status(&host, 2), Some(ReadinessStatus::Ready));
    }

    #[test]
    fn new_episode_resets_supported_statuses_and_rejects_old_or_wrong_identity() {
        let mut host = party();
        host.report(2, &report(1, 10, 1, ReadinessStatus::Ready))
            .unwrap();
        assert!(host.set_media(media(2), 2).unwrap());
        assert_eq!(status(&host, 1), Some(ReadinessStatus::Selecting));
        assert_eq!(status(&host, 2), Some(ReadinessStatus::Selecting));
        assert_eq!(status(&host, 3), None);
        assert!(!host
            .report(2, &report(1, 100, 1, ReadinessStatus::Ready))
            .unwrap());
        assert!(!host
            .report(2, &report(2, 100, 1, ReadinessStatus::Ready))
            .unwrap());
        assert!(!host
            .report(2, &report(3, u64::MAX, 2, ReadinessStatus::Ready))
            .unwrap());
        assert!(host
            .report(2, &report(2, 1, 2, ReadinessStatus::Ready))
            .unwrap());
        assert_eq!(status(&host, 2), Some(ReadinessStatus::Ready));
    }

    #[test]
    fn same_episode_host_reload_preserves_guest_status_and_returning_episode_uses_new_generation() {
        let mut host = party();
        host.report(2, &report(1, 1, 1, ReadinessStatus::Ready))
            .unwrap();
        assert!(!host.set_media(media(1), 1).unwrap());
        assert!(host.set_status(1, ReadinessStatus::Loading).unwrap());
        assert_eq!(status(&host, 2), Some(ReadinessStatus::Ready));
        host.set_media(media(2), 2).unwrap();
        host.set_media(media(1), 3).unwrap();
        assert!(!host
            .report(2, &report(1, 10, 1, ReadinessStatus::Ready))
            .unwrap());
        assert_eq!(status(&host, 2), Some(ReadinessStatus::Selecting));
        assert!(host
            .report(2, &report(3, 1, 1, ReadinessStatus::Ready))
            .unwrap());
        assert!(host.set_media(media(2), 3).is_err());
        assert!(!host.set_media(media(1), 1).unwrap());
    }

    #[test]
    fn reports_are_scoped_to_supported_existing_peers_and_per_peer_sequences() {
        let mut host = party();
        let before = host.snapshot().unwrap();
        assert!(!host
            .report(99, &report(1, 100, 1, ReadinessStatus::Ready))
            .unwrap());
        assert!(!host
            .report(3, &report(1, 100, 1, ReadinessStatus::Ready))
            .unwrap());
        assert_eq!(host.snapshot().unwrap(), before);
        assert!(host
            .report(2, &report(1, 1, 1, ReadinessStatus::Loading))
            .unwrap());
        let before_duplicate = host.snapshot().unwrap();
        assert!(!host
            .report(2, &report(1, 10, 1, ReadinessStatus::Loading))
            .unwrap());
        assert_eq!(host.snapshot().unwrap(), before_duplicate);
        assert!(!host
            .report(2, &report(1, 2, 1, ReadinessStatus::Ready))
            .unwrap());
        assert!(host
            .report(2, &report(1, 11, 1, ReadinessStatus::Ready))
            .unwrap());
        assert!(!host.add_member(2, true).unwrap());
        assert_eq!(status(&host, 2), Some(ReadinessStatus::Ready));
        assert!(host.add_member(4, true).unwrap());
        assert!(host
            .report(4, &report(1, 1, 1, ReadinessStatus::Ready))
            .unwrap());
        assert_eq!(status(&host, 4), Some(ReadinessStatus::Ready));
    }

    #[test]
    fn joins_leaves_and_capability_changes_keep_other_viewers_ready() {
        let mut host = party();
        host.report(2, &report(1, 1, 1, ReadinessStatus::Ready))
            .unwrap();
        let initial_sequence = host.snapshot().unwrap().sequence;
        assert!(host.add_member(4, true).unwrap());
        assert_eq!(status(&host, 4), Some(ReadinessStatus::Selecting));
        assert_eq!(status(&host, 2), Some(ReadinessStatus::Ready));
        assert!(host.add_member(2, false).unwrap());
        assert_eq!(status(&host, 2), None);
        assert!(host.add_member(2, true).unwrap());
        assert!(!host
            .report(2, &report(1, 1, 1, ReadinessStatus::Ready))
            .unwrap());
        assert_eq!(status(&host, 2), Some(ReadinessStatus::Selecting));
        host.report(2, &report(1, 2, 1, ReadinessStatus::Ready))
            .unwrap();
        assert!(host.add_member(3, true).unwrap());
        assert_eq!(status(&host, 3), Some(ReadinessStatus::Selecting));
        assert!(host.remove_member(4).unwrap());
        assert!(!host.remove_member(4).unwrap());
        let snapshot = host.snapshot().unwrap();
        assert!(snapshot.sequence > initial_sequence);
        assert_eq!(snapshot.members.len(), 3);
        assert_eq!(status(&host, 2), Some(ReadinessStatus::Ready));
        assert!(host.add_member(0, true).is_err());
        assert!(snapshot.validate().is_ok());
    }

    #[test]
    fn readiness_members_and_unknown_media_are_bounded() {
        let mut host = HostReadinessRoster::default();
        assert!(host.snapshot().is_none());
        for id in 1..=16 {
            host.add_member(id, true).unwrap();
        }
        assert!(host.snapshot().is_none());
        assert!(host.add_member(17, true).is_err());
        assert!(host.set_media(media(1), 0).is_err());
        host.set_media(media(1), 1).unwrap();
        assert_eq!(host.snapshot().unwrap().members.len(), 16);
    }

    #[test]
    fn guest_ignores_stale_wrong_episode_and_future_rosters_without_poisoning_order() {
        let mut host = party();
        let first = host.snapshot().unwrap();
        let mut guest = GuestReadinessRoster::default();
        assert!(guest.ingest(first.clone(), &media(1), 1).unwrap());
        host.report(2, &report(1, 1, 1, ReadinessStatus::Ready))
            .unwrap();
        let ready = host.snapshot().unwrap();
        assert!(guest.ingest(ready.clone(), &media(1), 1).unwrap());
        assert!(!guest.ingest(first, &media(1), 1).unwrap());
        assert_eq!(guest.current(&media(1), 1), Some(&ready));
        host.set_media(media(2), 2).unwrap();
        let next = host.snapshot().unwrap();
        assert!(!guest.ingest(next.clone(), &media(1), 1).unwrap());
        assert!(guest.current(&media(2), 2).is_none());
        assert!(guest.ingest(next.clone(), &media(2), 2).unwrap());
        assert!(!guest.ingest(ready, &media(2), 2).unwrap());
        assert_eq!(guest.current(&media(2), 2), Some(&next));
        guest.clear();
        assert!(guest.current(&media(2), 2).is_none());
        assert!(guest
            .ingest(party().snapshot().unwrap(), &media(1), 1)
            .unwrap());
    }
}
