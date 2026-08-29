use std::collections::HashMap;

use chrono::Utc;
use sha2::{Digest, Sha256};

use crate::{
    db::ReleaseInventoryRecord,
    model::{
        AcquisitionPhase, AcquisitionScope, AcquisitionState, ChannelPackItem, ClientDownloadState,
        DownloadState, FulfillmentAction, FulfillmentActionKind, FulfillmentActivity,
        FulfillmentActivityKind, FulfillmentRequirement, FulfillmentSatisfaction, ImportTaskState,
        LibraryAvailability, PackItemDisposition, PackItemPlanState, RecommendationMatchState,
        ReleaseFulfillment, ReleaseHolding, VariantKey,
    },
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

pub fn project_channel_item(
    item: &mut ChannelPackItem,
    import_states: &[ImportTaskState],
    inventory_records: &[ReleaseInventoryRecord],
) {
    let release_id = item.release.as_ref().and_then(|release| release.id);
    let scope = if item.replacement.is_some() {
        AcquisitionScope::ExactVariant
    } else {
        AcquisitionScope::Release
    };
    let target = item.replacement.as_ref().map(|replacement| VariantKey {
        tracker: replacement.tracker.clone(),
        torrent_id: replacement.torrent_id,
    });
    let requirement = FulfillmentRequirement {
        scope: scope.clone(),
        release_id,
        target,
    };
    let holdings = build_holdings(item, inventory_records);
    let satisfaction = fulfillment_satisfaction(item, &requirement, &holdings);
    let activities = build_activities(item, import_states, &holdings, &satisfaction);
    let actions = build_actions(item, &requirement, &holdings, &satisfaction, &activities);
    let downloads = item.replacement.as_ref().map_or_else(
        || item.downloads.clone(),
        |replacement| replacement.downloads.clone(),
    );
    let signals = AcquisitionSignals {
        owned: satisfaction == FulfillmentSatisfaction::Satisfied,
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
        downloading: inventory_records.iter().any(|record| {
            record.present
                && !record.in_library
                && record.live.as_ref().is_some_and(|live| live.progress < 1.0)
        }) || downloads
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
    let disposition =
        if item.replacement.is_some() && item.plan_state == PackItemPlanState::CleanupReady {
            PackItemDisposition::Cleanup
        } else if matches!(
            satisfaction,
            FulfillmentSatisfaction::Satisfied | FulfillmentSatisfaction::NotRequired
        ) {
            PackItemDisposition::Resolved
        } else {
            disposition(
                &phase,
                &item.plan_state,
                item.plan.is_some(),
                item.replacement.is_some(),
            )
        };
    let selectable = disposition == PackItemDisposition::Actionable
        || disposition == PackItemDisposition::Cleanup
        || (item.replacement.is_some() && disposition == PackItemDisposition::Waiting);
    item.acquisition = Some(AcquisitionState {
        scope,
        phase,
        release_id,
        tracker: requirement
            .target
            .as_ref()
            .map(|target| target.tracker.clone())
            .or_else(|| item.plan.as_ref().map(|plan| plan.tracker.clone())),
        torrent_id: requirement
            .target
            .as_ref()
            .map(|target| target.torrent_id)
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
    let revision = fulfillment_revision(&requirement, &satisfaction, &holdings, &activities);
    item.fulfillment = Some(ReleaseFulfillment {
        requirement,
        satisfaction,
        holdings,
        activities,
        actions,
        revision,
    });
    item.disposition = disposition;
    item.selectable = selectable;
}

fn build_holdings(
    item: &ChannelPackItem,
    inventory_records: &[ReleaseInventoryRecord],
) -> Vec<ReleaseHolding> {
    let mut holdings: HashMap<Option<VariantKey>, ReleaseHolding> = HashMap::new();
    for record in inventory_records {
        let variant =
            record
                .tracker
                .as_ref()
                .zip(record.torrent_id)
                .map(|(tracker, torrent_id)| VariantKey {
                    tracker: tracker.clone(),
                    torrent_id,
                });
        let holding = holdings
            .entry(variant.clone())
            .or_insert_with(|| ReleaseHolding {
                variant,
                in_library: false,
                present: false,
                downloads: Vec::new(),
            });
        holding.in_library |= record.in_library;
        holding.present |= record.present;
        if record.present
            && let Some(live) = &record.live
            && !holding.downloads.iter().any(|download| {
                download.client == live.client
                    && download.info_hash.eq_ignore_ascii_case(&live.info_hash)
            })
        {
            holding.downloads.push(live.clone());
        }
    }
    for variant in &item.variants {
        let key = VariantKey {
            tracker: variant.tracker.clone(),
            torrent_id: variant.torrent_id,
        };
        let holding = holdings
            .entry(Some(key.clone()))
            .or_insert_with(|| ReleaseHolding {
                variant: Some(key),
                in_library: false,
                present: false,
                downloads: Vec::new(),
            });
        holding.in_library |= variant
            .library
            .as_ref()
            .is_some_and(|library| library.availability != LibraryAvailability::Missing);
        holding.present |= !variant.downloads.is_empty();
        for live in &variant.downloads {
            if let Some(existing) = holding.downloads.iter_mut().find(|download| {
                download.client == live.client
                    && download.info_hash.eq_ignore_ascii_case(&live.info_hash)
            }) {
                *existing = live.clone();
            } else {
                holding.downloads.push(live.clone());
            }
        }
    }
    for download in &item.downloads {
        let holding = holdings.entry(None).or_insert_with(|| ReleaseHolding {
            variant: None,
            in_library: false,
            present: false,
            downloads: Vec::new(),
        });
        holding.in_library |= download.in_library;
        holding.present = true;
        if let Some(existing) = holding.downloads.iter_mut().find(|live| {
            live.client == download.live.client
                && live
                    .info_hash
                    .eq_ignore_ascii_case(&download.live.info_hash)
        }) {
            *existing = download.live.clone();
        } else {
            holding.downloads.push(download.live.clone());
        }
    }
    let mut holdings = holdings.into_values().collect::<Vec<_>>();
    holdings.retain(|holding| holding.in_library || holding.present);
    holdings.sort_by(|left, right| {
        right
            .in_library
            .cmp(&left.in_library)
            .then_with(|| right.present.cmp(&left.present))
            .then_with(|| {
                left.variant
                    .as_ref()
                    .map(|key| key.tracker.as_str())
                    .cmp(&right.variant.as_ref().map(|key| key.tracker.as_str()))
            })
            .then_with(|| {
                left.variant
                    .as_ref()
                    .map(|key| key.torrent_id)
                    .cmp(&right.variant.as_ref().map(|key| key.torrent_id))
            })
    });
    holdings
}

fn fulfillment_satisfaction(
    item: &ChannelPackItem,
    requirement: &FulfillmentRequirement,
    holdings: &[ReleaseHolding],
) -> FulfillmentSatisfaction {
    if item.plan_state == PackItemPlanState::Excluded {
        return FulfillmentSatisfaction::NotRequired;
    }
    if item.match_state != RecommendationMatchState::Matched || item.release.is_none() {
        return FulfillmentSatisfaction::Unknown;
    }
    let satisfied = match requirement.scope {
        AcquisitionScope::Release => {
            holdings.iter().any(|holding| holding.in_library)
                || item.plan_state == PackItemPlanState::AlreadyOwned
        }
        AcquisitionScope::ExactVariant => requirement.target.as_ref().is_some_and(|target| {
            holdings.iter().any(|holding| {
                holding.variant.as_ref() == Some(target)
                    && (holding.in_library
                        || holding
                            .downloads
                            .iter()
                            .any(|download| download.progress >= 1.0))
            }) || item.replacement.as_ref().is_some_and(|replacement| {
                replacement.tracker.eq_ignore_ascii_case(&target.tracker)
                    && replacement.torrent_id == target.torrent_id
                    && matches!(
                        replacement.state,
                        crate::model::ReplacementTargetState::Complete
                    )
            })
        }),
    };
    if satisfied {
        FulfillmentSatisfaction::Satisfied
    } else {
        FulfillmentSatisfaction::Unsatisfied
    }
}

fn build_activities(
    item: &ChannelPackItem,
    import_states: &[ImportTaskState],
    holdings: &[ReleaseHolding],
    satisfaction: &FulfillmentSatisfaction,
) -> Vec<FulfillmentActivity> {
    let mut activities = Vec::new();
    for holding in holdings {
        for download in &holding.downloads {
            let kind = match download.state {
                ClientDownloadState::Downloading => FulfillmentActivityKind::Downloading,
                ClientDownloadState::Seeding | ClientDownloadState::Complete => {
                    FulfillmentActivityKind::Seeding
                }
                ClientDownloadState::Paused => FulfillmentActivityKind::Paused,
                ClientDownloadState::Queued => FulfillmentActivityKind::Queued,
                ClientDownloadState::Checking => FulfillmentActivityKind::Checking,
                ClientDownloadState::Stalled => FulfillmentActivityKind::Stalled,
                ClientDownloadState::Error => FulfillmentActivityKind::Failed,
                ClientDownloadState::Unknown => continue,
            };
            activities.push(FulfillmentActivity {
                kind,
                target: holding.variant.clone(),
                job_id: None,
                client: Some(download.client.clone()),
                info_hash: Some(download.info_hash.clone()),
            });
        }
    }
    if let Some(job) = &item.job {
        let kind = match job.state {
            DownloadState::Queued | DownloadState::FetchingMetadata | DownloadState::Submitting => {
                Some(FulfillmentActivityKind::Queued)
            }
            DownloadState::Active => Some(FulfillmentActivityKind::Downloading),
            DownloadState::Complete
                if *satisfaction != FulfillmentSatisfaction::Satisfied
                    && !activities.iter().any(|activity| {
                        activity.target.as_ref().is_some_and(|target| {
                            target.tracker.eq_ignore_ascii_case(&job.tracker)
                                && target.torrent_id == job.torrent_id
                        })
                    }) =>
            {
                Some(FulfillmentActivityKind::Downloaded)
            }
            DownloadState::Failed => Some(FulfillmentActivityKind::Failed),
            _ => None,
        };
        if let Some(kind) = kind {
            activities.push(FulfillmentActivity {
                kind,
                target: Some(VariantKey {
                    tracker: job.tracker.clone(),
                    torrent_id: job.torrent_id,
                }),
                job_id: Some(job.id),
                client: None,
                info_hash: job.info_hash.clone(),
            });
        }
    }
    for state in import_states {
        let kind = match state {
            ImportTaskState::Downloading => FulfillmentActivityKind::Downloading,
            ImportTaskState::Resolving | ImportTaskState::Ready | ImportTaskState::Processing => {
                FulfillmentActivityKind::Importing
            }
            ImportTaskState::Complete => continue,
            ImportTaskState::Failed | ImportTaskState::Blocked => FulfillmentActivityKind::Failed,
            ImportTaskState::NeedsReview | ImportTaskState::Dismissed => continue,
        };
        activities.push(FulfillmentActivity {
            kind,
            target: None,
            job_id: None,
            client: None,
            info_hash: None,
        });
    }
    activities
}

fn build_actions(
    item: &ChannelPackItem,
    requirement: &FulfillmentRequirement,
    holdings: &[ReleaseHolding],
    satisfaction: &FulfillmentSatisfaction,
    activities: &[FulfillmentActivity],
) -> Vec<FulfillmentAction> {
    let mut actions = Vec::new();
    if item.match_state != RecommendationMatchState::Matched || item.release.is_none() {
        actions.push(FulfillmentAction {
            kind: FulfillmentActionKind::ReviewMatch,
            target: None,
            primary: true,
            enabled: true,
            reason: item.reason.clone(),
        });
        return actions;
    }
    actions.push(FulfillmentAction {
        kind: FulfillmentActionKind::ChangeMatch,
        target: None,
        primary: false,
        enabled: true,
        reason: None,
    });
    if item.plan_state == PackItemPlanState::CleanupReady {
        actions.push(FulfillmentAction {
            kind: FulfillmentActionKind::Cleanup,
            target: requirement.target.clone(),
            primary: true,
            enabled: true,
            reason: item.reason.clone(),
        });
        return actions;
    }
    let active = activities.iter().any(|activity| {
        matches!(
            activity.kind,
            FulfillmentActivityKind::Queued
                | FulfillmentActivityKind::Downloading
                | FulfillmentActivityKind::Downloaded
                | FulfillmentActivityKind::Importing
        )
    });
    if *satisfaction == FulfillmentSatisfaction::Unsatisfied
        && !active
        && let Some(plan) = &item.plan
    {
        actions.push(FulfillmentAction {
            kind: if item
                .job
                .as_ref()
                .is_some_and(|job| job.state == DownloadState::Failed)
            {
                FulfillmentActionKind::Retry
            } else {
                FulfillmentActionKind::Add
            },
            target: Some(VariantKey {
                tracker: plan.tracker.clone(),
                torrent_id: plan.torrent_id,
            }),
            primary: true,
            enabled: true,
            reason: item.reason.clone(),
        });
    }
    if *satisfaction == FulfillmentSatisfaction::Satisfied
        && requirement.scope == AcquisitionScope::Release
    {
        for variant in &item.variants {
            let key = VariantKey {
                tracker: variant.tracker.clone(),
                torrent_id: variant.torrent_id,
            };
            let held = holdings.iter().any(|holding| {
                holding.variant.as_ref() == Some(&key) && (holding.in_library || holding.present)
            });
            let eligible = variant
                .eligibility
                .as_ref()
                .is_some_and(|eligibility| eligibility.eligible);
            if !held && eligible {
                actions.push(FulfillmentAction {
                    kind: FulfillmentActionKind::AddAnother,
                    target: Some(key),
                    primary: false,
                    enabled: true,
                    reason: None,
                });
            }
        }
    }
    actions
}

fn fulfillment_revision(
    requirement: &FulfillmentRequirement,
    satisfaction: &FulfillmentSatisfaction,
    holdings: &[ReleaseHolding],
    activities: &[FulfillmentActivity],
) -> String {
    let mut digest = Sha256::new();
    digest.update(format!("{requirement:?}|{satisfaction:?}"));
    for holding in holdings {
        digest.update(format!("{holding:?}"));
    }
    for activity in activities {
        digest.update(format!("{activity:?}"));
    }
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use crate::{
        db::ReleaseInventoryRecord,
        model::{
            AcquisitionPhase, ChannelPackItem, ClientDownloadState, FulfillmentSatisfaction,
            LeechStatus, LiveDownloadStatus, PackItemDisposition, PackItemPlanState,
            RecommendationMatchState, RecommendationSource, ReleaseSummary, TorrentVariant,
        },
    };

    use super::{AcquisitionSignals, disposition, project_channel_item};

    fn matched_item(variant: TorrentVariant) -> ChannelPackItem {
        ChannelPackItem {
            ordinal: 1,
            source: RecommendationSource {
                id: "source:1".into(),
                rank: 1,
                artist: "Artist".into(),
                title: "Album".into(),
                year: Some(2026),
                artwork: None,
                url: None,
                mbid: None,
                score: None,
                catalog_country: None,
                substituted_from: None,
                trumped_downloads: Vec::new(),
                lookup_files: Vec::new(),
            },
            match_state: RecommendationMatchState::Matched,
            release: Some(ReleaseSummary {
                id: Some(Uuid::new_v4()),
                tracker: "ops".into(),
                group_id: 10,
                title: "Album".into(),
                artist: Some("Artist".into()),
                artists: Vec::new(),
                year: Some(2026),
                artwork: None,
                release_type: Some("Album".into()),
                sources: Vec::new(),
                album_coverage: None,
            }),
            variants: vec![variant],
            candidates: Vec::new(),
            downloads: Vec::new(),
            plan_state: PackItemPlanState::AlreadyOwned,
            plan: None,
            replacement: None,
            reason: Some("Already present in the Library".into()),
            job_id: None,
            job: None,
            acquisition: None,
            fulfillment: None,
            disposition: Default::default(),
            selectable: false,
        }
    }

    fn variant(
        tracker: &str,
        torrent_id: i64,
        downloads: Vec<LiveDownloadStatus>,
    ) -> TorrentVariant {
        TorrentVariant {
            tracker: tracker.into(),
            torrent_id,
            group_id: 10,
            info_hash: None,
            format: Some("FLAC".into()),
            encoding: Some("24bit Lossless".into()),
            media: Some("WEB".into()),
            size: Some(100),
            seeders: Some(10),
            leechers: Some(0),
            snatched: Some(1),
            freeleech: false,
            leech_status: LeechStatus::Regular,
            can_use_token: true,
            token_eligibility_known: true,
            eligibility: None,
            remaster_title: None,
            downloads,
            library: None,
        }
    }

    fn seeding() -> LiveDownloadStatus {
        LiveDownloadStatus {
            client: "music".into(),
            info_hash: "abc".into(),
            state: ClientDownloadState::Seeding,
            client_state: "stalledUP".into(),
            diagnostic: None,
            progress: 1.0,
            size: 100,
            downloaded: 100,
            uploaded: 10,
            download_speed: 0,
            upload_speed: 0,
            eta: None,
            ratio: 0.1,
            save_path: "/music".into(),
            content_path: None,
            added_at: Some(Utc::now()),
            completed_at: Some(Utc::now()),
        }
    }

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

    #[test]
    fn fulfillment_keeps_release_satisfaction_and_seeding_as_separate_facts() {
        let mut item = matched_item(variant("red", 20, vec![seeding()]));
        let release_id = item
            .release
            .as_ref()
            .and_then(|release| release.id)
            .unwrap();
        project_channel_item(
            &mut item,
            &[],
            &[ReleaseInventoryRecord {
                tracker: Some("red".into()),
                torrent_id: Some(20),
                present: true,
                in_library: true,
                live: None,
            }],
        );
        let fulfillment = item.fulfillment.expect("fulfillment");
        assert_eq!(fulfillment.requirement.release_id, Some(release_id));
        assert_eq!(fulfillment.satisfaction, FulfillmentSatisfaction::Satisfied);
        assert!(
            fulfillment.activities.iter().any(|activity| {
                activity.kind == crate::model::FulfillmentActivityKind::Seeding
            })
        );
        assert_eq!(item.disposition, PackItemDisposition::Resolved);
        assert!(!item.selectable);
    }
}
