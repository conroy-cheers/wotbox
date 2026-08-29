use chrono::Utc;

use crate::model::{
    AcquisitionPhase, AcquisitionScope, AcquisitionState, ChannelPackItem, DownloadState,
    ImportTaskState, LibraryAvailability, PackItemDisposition, PackItemPlanState,
    RecommendationMatchState,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AcquisitionSignals {
    pub owned: bool,
    pub importing: bool,
    pub downloaded: bool,
    pub downloading: bool,
    pub queued: bool,
    pub needs_review: bool,
    pub failed: bool,
}

impl AcquisitionSignals {
    pub fn phase(self) -> AcquisitionPhase {
        if self.owned {
            AcquisitionPhase::Owned
        } else if self.importing {
            AcquisitionPhase::Importing
        } else if self.downloaded {
            AcquisitionPhase::Downloaded
        } else if self.downloading {
            AcquisitionPhase::Downloading
        } else if self.queued {
            AcquisitionPhase::Queued
        } else if self.needs_review {
            AcquisitionPhase::NeedsReview
        } else if self.failed {
            AcquisitionPhase::Failed
        } else {
            AcquisitionPhase::Missing
        }
    }
}

pub fn disposition(
    phase: &AcquisitionPhase,
    plan_state: &PackItemPlanState,
    has_plan: bool,
    replacement: bool,
) -> PackItemDisposition {
    if replacement && *plan_state == PackItemPlanState::CleanupReady {
        return PackItemDisposition::Cleanup;
    }
    match phase {
        AcquisitionPhase::Owned => PackItemDisposition::Resolved,
        AcquisitionPhase::Queued
        | AcquisitionPhase::Downloading
        | AcquisitionPhase::Downloaded
        | AcquisitionPhase::Importing => PackItemDisposition::Waiting,
        AcquisitionPhase::Failed if has_plan => PackItemDisposition::Actionable,
        AcquisitionPhase::NeedsReview | AcquisitionPhase::Failed => PackItemDisposition::Review,
        AcquisitionPhase::Missing => match plan_state {
            PackItemPlanState::Executable => PackItemDisposition::Actionable,
            PackItemPlanState::CleanupReady => PackItemDisposition::Cleanup,
            PackItemPlanState::AlreadyOwned
            | PackItemPlanState::Excluded
            | PackItemPlanState::Submitted => PackItemDisposition::Resolved,
            PackItemPlanState::AlreadyDownloading => PackItemDisposition::Waiting,
            _ => PackItemDisposition::Review,
        },
    }
}

pub fn project_channel_item(item: &mut ChannelPackItem, import_states: &[ImportTaskState]) {
    let release_id = item.release.as_ref().and_then(|release| release.id);
    let scope = if item.replacement.is_some() {
        AcquisitionScope::ExactVariant
    } else {
        AcquisitionScope::Release
    };
    let downloads = item.replacement.as_ref().map_or_else(
        || item.downloads.clone(),
        |replacement| replacement.downloads.clone(),
    );
    let signals = AcquisitionSignals {
        owned: scope == AcquisitionScope::Release
            && (item.downloads.iter().any(|download| download.in_library)
                || item.variants.iter().any(|variant| {
                    variant
                        .library
                        .as_ref()
                        .is_some_and(|library| library.availability != LibraryAvailability::Missing)
                })),
        importing: import_states.iter().any(|state| {
            matches!(
                state,
                ImportTaskState::Resolving | ImportTaskState::Ready | ImportTaskState::Processing
            )
        }),
        downloaded: downloads
            .iter()
            .any(|download| download.live.progress >= 1.0)
            || item
                .job
                .as_ref()
                .is_some_and(|job| job.state == DownloadState::Complete)
            || import_states.contains(&ImportTaskState::Complete),
        downloading: downloads
            .iter()
            .any(|download| !download.in_library && download.live.progress < 1.0)
            || item.variants.iter().any(|variant| {
                variant
                    .downloads
                    .iter()
                    .any(|download| download.progress < 1.0)
            })
            || item
                .job
                .as_ref()
                .is_some_and(|job| job.state == DownloadState::Active)
            || import_states.contains(&ImportTaskState::Downloading),
        queued: item.job.as_ref().is_some_and(|job| {
            matches!(
                job.state,
                DownloadState::Queued | DownloadState::FetchingMetadata | DownloadState::Submitting
            )
        }),
        needs_review: item.match_state != RecommendationMatchState::Matched
            || import_states.iter().any(|state| {
                matches!(
                    state,
                    ImportTaskState::NeedsReview | ImportTaskState::Blocked
                )
            }),
        failed: item
            .job
            .as_ref()
            .is_some_and(|job| job.state == DownloadState::Failed)
            || import_states.contains(&ImportTaskState::Failed),
    };
    let phase = signals.phase();
    let disposition = disposition(
        &phase,
        &item.plan_state,
        item.plan.is_some(),
        item.replacement.is_some(),
    );
    let selectable = disposition == PackItemDisposition::Actionable
        || disposition == PackItemDisposition::Cleanup
        || (item.replacement.is_some() && disposition == PackItemDisposition::Waiting);
    item.acquisition = Some(AcquisitionState {
        scope,
        phase,
        release_id,
        tracker: item
            .replacement
            .as_ref()
            .map(|replacement| replacement.tracker.clone())
            .or_else(|| item.plan.as_ref().map(|plan| plan.tracker.clone())),
        torrent_id: item
            .replacement
            .as_ref()
            .map(|replacement| replacement.torrent_id)
            .or_else(|| item.plan.as_ref().map(|plan| plan.torrent_id)),
        job_ids: item.job_id.into_iter().collect(),
        downloads,
        reason: item
            .job
            .as_ref()
            .and_then(|job| job.error_message.clone())
            .or_else(|| item.reason.clone()),
        updated_at: item
            .job
            .as_ref()
            .map(|job| job.updated_at)
            .unwrap_or_else(Utc::now),
    });
    item.disposition = disposition;
    item.selectable = selectable;
}

#[cfg(test)]
mod tests {
    use crate::model::{AcquisitionPhase, PackItemDisposition, PackItemPlanState};

    use super::{AcquisitionSignals, disposition};

    #[test]
    fn acquisition_precedence_keeps_completed_unowned_work_waiting() {
        assert_eq!(
            AcquisitionSignals {
                downloaded: true,
                failed: true,
                ..Default::default()
            }
            .phase(),
            AcquisitionPhase::Downloaded
        );
        assert_eq!(
            disposition(
                &AcquisitionPhase::Downloaded,
                &PackItemPlanState::Executable,
                true,
                false,
            ),
            PackItemDisposition::Waiting
        );
    }

    #[test]
    fn active_download_beats_a_stale_missing_plan_and_failed_work_is_retryable() {
        assert_eq!(
            AcquisitionSignals {
                downloading: true,
                ..Default::default()
            }
            .phase(),
            AcquisitionPhase::Downloading
        );
        assert_eq!(
            disposition(
                &AcquisitionPhase::Downloading,
                &PackItemPlanState::Executable,
                true,
                false,
            ),
            PackItemDisposition::Waiting
        );
        assert_eq!(
            disposition(
                &AcquisitionPhase::Failed,
                &PackItemPlanState::Executable,
                true,
                false,
            ),
            PackItemDisposition::Actionable
        );
    }

    #[test]
    fn exact_replacement_waiters_remain_selectable_via_projection_policy() {
        assert_eq!(
            disposition(
                &AcquisitionPhase::Downloading,
                &PackItemPlanState::AlreadyDownloading,
                false,
                true,
            ),
            PackItemDisposition::Waiting
        );
    }
}
