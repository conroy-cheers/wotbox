use std::{
    collections::HashSet,
    fs::File,
    io::Write,
    path::{Path as StdPath, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use chrono::{Duration as ChronoDuration, Utc};
use uuid::Uuid;

use crate::{
    api::AppState,
    background::WorkLease,
    model::{DownloadJob, DownloadState},
    provider::{ProviderRequestError, RequestClass},
    qbittorrent::DownloadClient,
    tracker::{TrackerClient, torrent_info_hash},
};

pub(crate) async fn download_stage_path(state: &AppState, job_id: Uuid) -> Result<PathBuf> {
    let name = match state.db.download_attempt(job_id).await? {
        Some(attempt) => format!("{job_id}.{attempt}.torrent"),
        None => format!("{job_id}.torrent"),
    };
    Ok(state.download_staging_dir.join(name))
}

pub(crate) fn write_staged_download(path: &StdPath, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("download staging path has no parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("persist staged torrent {}", path.display()))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

pub(crate) async fn cleanup_download_stage(path: &StdPath) {
    if let Err(error) = tokio::fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, path = %path.display(), "could not remove staged torrent");
    }
}

pub(crate) async fn cleanup_orphaned_download_stages(state: &AppState) -> Result<()> {
    let incomplete = state
        .db
        .list_jobs()
        .await?
        .into_iter()
        .filter(|job| {
            matches!(
                job.state,
                DownloadState::Queued
                    | DownloadState::FetchingMetadata
                    | DownloadState::Submitting
                    | DownloadState::Failed
            )
        })
        .map(|job| job.id)
        .collect::<HashSet<_>>();
    let mut entries = tokio::fs::read_dir(&state.download_staging_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let job_id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .and_then(|value| value.split('.').next())
            .and_then(|value| Uuid::parse_str(value).ok());
        let keep = job_id.is_some_and(|id| incomplete.contains(&id));
        if !keep {
            tokio::fs::remove_file(&path)
                .await
                .with_context(|| format!("remove orphaned download stage {}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) async fn download_torrent_payload_for_job(
    tracker: &dyn TrackerClient,
    job: &DownloadJob,
) -> Result<(Vec<u8>, String)> {
    // Gazelle applies a freeleech token to the torrent-file request itself, so
    // the first request must carry the final token decision. The returned file
    // is also sufficient to calculate the missing info hash.
    let bytes = tracker
        .download_torrent(job.torrent_id, job.use_token)
        .await?;
    let info_hash = torrent_info_hash(&bytes)?;
    Ok((bytes, info_hash))
}

#[derive(Debug, thiserror::Error)]
#[error(
    "The tracker may have charged a token. Review the tracker account before requesting the download again."
)]
pub(crate) struct TokenDispatchUncertain;

#[derive(Debug, thiserror::Error)]
#[error(
    "The configured download client differs from the accepted destination. Review the client and submit a new attempt."
)]
pub(crate) struct DownloadDestinationChanged;

async fn stage_download_payload(path: &StdPath, bytes: Vec<u8>) -> Result<Vec<u8>> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        write_staged_download(&path, &bytes)?;
        Ok(bytes)
    })
    .await?
}

async fn fetch_and_stage_download(
    state: &AppState,
    tracker: &dyn TrackerClient,
    job: &DownloadJob,
    stage: &StdPath,
    lease: WorkLease<'_>,
) -> Result<Vec<u8>> {
    state
        .db
        .set_submission_state(lease.key(), job.id, DownloadState::Submitting, None)
        .await?;
    lease.check(&state.db).await?;
    let bytes = match tracker
        .download_torrent(job.torrent_id, job.use_token)
        .await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            if job.use_token {
                let undispatched = error
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<ProviderRequestError>())
                    .is_some_and(|error| !matches!(error, ProviderRequestError::Upstream { .. }));
                if undispatched {
                    state
                        .db
                        .set_submission_state(
                            lease.key(),
                            job.id,
                            DownloadState::FetchingMetadata,
                            None,
                        )
                        .await?;
                } else {
                    return Err(error.context(TokenDispatchUncertain));
                }
            }
            return Err(error);
        }
    };
    stage_download_payload(stage, bytes).await.map_err(|error| {
        if job.use_token {
            error.context(TokenDispatchUncertain)
        } else {
            error
        }
    })
}

async fn find_previous_stage(state: &AppState, id: Uuid) -> Result<Option<PathBuf>> {
    let attempt = state.db.download_attempt(id).await?.unwrap_or(0);
    if attempt == 0 {
        return Ok(None);
    }
    let mut previous = None;
    let mut entries = tokio::fs::read_dir(&state.download_staging_dir).await?;
    let prefix = id.to_string();
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let Some(name) = name.to_str().and_then(|name| name.strip_suffix(".torrent")) else {
            continue;
        };
        let version = if name == prefix {
            Some(0)
        } else {
            name.strip_prefix(&prefix)
                .and_then(|suffix| suffix.strip_prefix('.'))
                .and_then(|suffix| suffix.parse::<i64>().ok())
        };
        if let Some(version) = version
            && version >= 0
            && version < attempt
            && previous.as_ref().is_none_or(|(known, _)| version > *known)
        {
            previous = Some((version, entry.path()));
        }
    }
    Ok(previous.map(|(_, path)| path))
}

async fn confirm_visible_torrent(
    state: &AppState,
    client: &dyn DownloadClient,
    lease: WorkLease<'_>,
    job: Uuid,
    hash: &str,
    stage: &StdPath,
) -> Result<()> {
    let observed = client
        .download_with_class(hash, RequestClass::Download)
        .await?
        .context("Waiting for qBittorrent to report the submitted torrent")?;
    state
        .db
        .confirm_submission(lease.key(), job, &observed.live)
        .await?;
    cleanup_download_stage(stage).await;
    Ok(())
}

pub(crate) async fn process_download(
    state: &AppState,
    job: DownloadJob,
    lease: WorkLease<'_>,
) -> Result<()> {
    let frozen = state.db.download_destination(job.id).await?;
    let profile = frozen
        .as_ref()
        .or_else(|| state.profiles.get(&job.profile))
        .ok_or_else(|| anyhow!("download profile disappeared from configuration"))?;
    let client = state
        .download_clients
        .get(&profile.client)
        .ok_or_else(|| anyhow!("download client disappeared from configuration"))?;
    if !state
        .db
        .download_destination_matches(job.id, client.instance_id().as_deref())
        .await?
    {
        return Err(DownloadDestinationChanged.into());
    }
    let stage_path = download_stage_path(state, job.id).await?;
    let staged_payload = match tokio::fs::read(&stage_path).await {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(previous) = find_previous_stage(state, job.id).await? {
                let bytes = tokio::fs::read(&previous)
                    .await
                    .context("read previous submission stage")?;
                torrent_info_hash(&bytes)?;
                Some(stage_download_payload(&stage_path, bytes).await?)
            } else {
                None
            }
        }
        Err(error) => return Err(error).context("read staged torrent"),
    };

    if let Some(info_hash) = job.info_hash.as_deref() {
        if let Some(existing) = client
            .download_with_class(info_hash, RequestClass::Download)
            .await?
        {
            state
                .db
                .confirm_submission(lease.key(), job.id, &existing.live)
                .await?;
            cleanup_download_stage(&stage_path).await;
            return Ok(());
        }
        if let Some(bytes) = staged_payload.as_ref() {
            anyhow::ensure!(
                torrent_info_hash(bytes)?.eq_ignore_ascii_case(info_hash),
                "Staged torrent hash differs from the stored submission hash"
            );
            let file_name = format!("{}-{}.torrent", job.tracker, job.torrent_id);
            lease.check(&state.db).await?;
            client
                .add_torrent(bytes.clone(), &file_name, profile)
                .await?;
            return confirm_visible_torrent(
                state,
                client.as_ref(),
                lease,
                job.id,
                info_hash,
                &stage_path,
            )
            .await;
        }
    }

    if job.use_token && job.state == DownloadState::Submitting && staged_payload.is_none() {
        return Err(TokenDispatchUncertain.into());
    }

    state
        .db
        .set_submission_state(lease.key(), job.id, DownloadState::FetchingMetadata, None)
        .await?;
    let tracker = state
        .trackers
        .get(&job.tracker)
        .ok_or_else(|| anyhow!("tracker disappeared from configuration"))?;

    let (mut metadata, mut canonical, raw) = tracker
        .torrent_with_class(job.torrent_id, RequestClass::Download)
        .await?;
    let preferences = state.db.get_runtime_preferences().await?;
    let token_available_or_unknown = metadata.can_use_token || !metadata.token_eligibility_known;
    let eligibility = preferences.release.eligibility(
        &job.tracker,
        canonical.variant.format.as_deref(),
        canonical.variant.encoding.as_deref(),
        canonical.variant.media.as_deref(),
        canonical.variant.size,
        canonical.variant.leech_status,
        token_available_or_unknown,
    );
    if !eligibility.eligible {
        let message = match eligibility.reason {
            crate::model::DownloadEligibilityReason::BelowQualityCutoff => format!(
                "release quality '{}' is below the configured cutoff",
                crate::model::ReleasePreferences::quality_class(
                    canonical.variant.format.as_deref(),
                    canonical.variant.encoding.as_deref(),
                )
            ),
            crate::model::DownloadEligibilityReason::BelowMediaCutoff => format!(
                "release media '{}' is below the configured cutoff",
                canonical.variant.media.as_deref().unwrap_or("Other")
            ),
            crate::model::DownloadEligibilityReason::TrackerDisabled => {
                format!("downloads from {} are disabled by preferences", job.tracker)
            }
            crate::model::DownloadEligibilityReason::FreeleechRequired => {
                format!(
                    "{} is configured for already-free torrents only",
                    job.tracker
                )
            }
            crate::model::DownloadEligibilityReason::TokenUnavailable => {
                "a freeleech token is required but unavailable for this torrent".into()
            }
            crate::model::DownloadEligibilityReason::TokenCostUnknown => {
                "an OPS freeleech token was requested, but the torrent size is unavailable".into()
            }
            crate::model::DownloadEligibilityReason::Eligible => {
                "torrent is not eligible under the configured tracker policy".into()
            }
        };
        return Err(anyhow!(message));
    }
    if eligibility.requires_token && !job.use_token {
        return Err(anyhow!(
            "a freeleech token is required by the configured tracker policy"
        ));
    }
    if job.use_token && !eligibility.requires_token {
        let policy = preferences.release.tracker_policy(&job.tracker);
        if matches!(
            policy.mode,
            crate::model::TrackerDownloadMode::Disabled
                | crate::model::TrackerDownloadMode::FreeleechOnly
        ) {
            return Err(anyhow!(
                "freeleech token use is disabled by the configured tracker policy"
            ));
        }
    }
    if job.use_token && eligibility.token_cost.is_none() {
        return Err(anyhow!(
            "freeleech token cost is unknown because the tracker has no supported cost model or the torrent size is unavailable"
        ));
    }
    if job.use_token && metadata.token_eligibility_known && !metadata.can_use_token {
        return Err(anyhow!("torrent is not eligible for a freeleech token"));
    }
    let mut payload = staged_payload;
    let info_hash = if let Some(info_hash) = metadata
        .info_hash
        .clone()
        .or_else(|| canonical.variant.info_hash.clone())
    {
        info_hash.to_ascii_lowercase()
    } else if let Some(bytes) = payload.as_ref() {
        torrent_info_hash(bytes)?
    } else {
        let bytes =
            fetch_and_stage_download(state, tracker.as_ref(), &job, &stage_path, lease).await?;
        let info_hash = torrent_info_hash(&bytes)?;
        payload = Some(bytes);
        info_hash
    };
    metadata.info_hash = Some(info_hash.clone());
    canonical.variant.info_hash = Some(info_hash.clone());
    state
        .db
        .put_snapshot(
            &job.tracker,
            "torrent",
            &job.torrent_id.to_string(),
            &metadata,
            &raw,
            Utc::now(),
            Utc::now() + ChronoDuration::minutes(15),
        )
        .await?;
    state
        .db
        .put_canonical(
            &canonical,
            Utc::now(),
            Utc::now() + ChronoDuration::hours(24),
        )
        .await?;
    state
        .db
        .update_job_metadata(job.id, metadata.group_id, &info_hash, &metadata.name)
        .await?;
    state
        .db
        .seed_download_link(
            &profile.client,
            &info_hash,
            &job.tracker,
            metadata.group_id,
            metadata.torrent_id,
            true,
        )
        .await?;

    if let Some(existing) = client
        .download_with_class(&info_hash, RequestClass::Download)
        .await?
    {
        state
            .db
            .confirm_submission(lease.key(), job.id, &existing.live)
            .await?;
        cleanup_download_stage(&stage_path).await;
        return Ok(());
    }

    let bytes = match payload {
        Some(bytes) => bytes,
        None => fetch_and_stage_download(state, tracker.as_ref(), &job, &stage_path, lease).await?,
    };
    anyhow::ensure!(
        torrent_info_hash(&bytes)?.eq_ignore_ascii_case(&info_hash),
        "Tracker torrent payload differs from the expected hash"
    );
    let file_name = format!("{}-{}.torrent", job.tracker, job.torrent_id);
    lease.check(&state.db).await?;
    client.add_torrent(bytes, &file_name, profile).await?;
    confirm_visible_torrent(
        state,
        client.as_ref(),
        lease,
        job.id,
        &info_hash,
        &stage_path,
    )
    .await
}
