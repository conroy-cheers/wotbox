use std::{collections::BTreeSet, sync::Arc, time::Duration};

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::{sync::watch, task::JoinHandle, time::MissedTickBehavior};

use crate::{
    api::{AppState, refresh_library_projection, spawn_channel_scheduler},
    db::{Database, DownloadObservation},
    model::DownloadState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeScope {
    Activity,
    Assets,
    Catalog,
    Channels,
    Operations,
    Providers,
    Settings,
    Global,
}

impl ChangeScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Activity => "activity",
            Self::Assets => "assets",
            Self::Catalog => "catalog",
            Self::Channels => "channels",
            Self::Operations => "operations",
            Self::Providers => "providers",
            Self::Settings => "settings",
            Self::Global => "global",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChangeSet {
    scopes: BTreeSet<ChangeScope>,
    resources: BTreeSet<String>,
    reason: String,
    payload: Option<Value>,
}

impl ChangeSet {
    pub fn new(reason: impl Into<String>, scopes: impl IntoIterator<Item = ChangeScope>) -> Self {
        Self {
            scopes: scopes.into_iter().collect(),
            resources: BTreeSet::new(),
            reason: reason.into(),
            payload: None,
        }
    }

    pub fn with_resources(
        mut self,
        resources: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.resources.extend(resources.into_iter().map(Into::into));
        self
    }

    pub fn with_payload(mut self, payload: Value) -> Self {
        self.payload = Some(payload);
        self
    }

    pub fn encode_scopes(&self) -> String {
        self.scopes
            .iter()
            .map(|scope| scope.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn encode_resources(&self) -> String {
        self.resources.iter().cloned().collect::<Vec<_>>().join(",")
    }

    pub fn payload(&self) -> Option<Value> {
        self.payload.clone()
    }
}

#[derive(Clone)]
pub struct ChangeHub {
    db: Database,
    cursor: watch::Sender<i64>,
}

impl ChangeHub {
    pub async fn new(db: Database) -> Result<Self> {
        let cursor = db.latest_change_cursor().await?;
        let (sender, _) = watch::channel(cursor);
        Ok(Self { db, cursor: sender })
    }

    pub async fn publish(&self, change: ChangeSet) -> Result<i64> {
        let cursor = self
            .db
            .append_resource_change_event(
                &change.encode_scopes(),
                &change.encode_resources(),
                change.reason(),
                change.payload(),
            )
            .await?;
        self.cursor.send_replace(cursor);
        tracing::debug!(cursor, scopes = %change.encode_scopes(), reason = %change.reason(), "published change event");
        Ok(cursor)
    }

    pub fn subscribe(&self) -> watch::Receiver<i64> {
        self.cursor.subscribe()
    }
}

pub struct ApplicationRuntime {
    stop: watch::Sender<bool>,
    handles: Vec<JoinHandle<()>>,
}

impl ApplicationRuntime {
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        for mut handle in self.handles {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() || tokio::time::timeout(remaining, &mut handle).await.is_err() {
                handle.abort();
            }
        }
    }
}

pub fn spawn_application_observers(state: Arc<AppState>) -> ApplicationRuntime {
    let (stop, receiver) = watch::channel(false);
    let handles = vec![
        tokio::spawn(download_client_observer(state.clone(), receiver.clone())),
        tokio::spawn(provider_observer(state.clone(), receiver.clone())),
        tokio::spawn(library_projection_observer(state.clone(), receiver.clone())),
        tokio::spawn(change_retention(state.clone(), receiver)),
        spawn_channel_scheduler(state),
    ];
    ApplicationRuntime { stop, handles }
}

async fn library_projection_observer(state: Arc<AppState>, mut stop: watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = interval.tick() => {}
        }
        if let Err(error) = refresh_library_projection(&state).await {
            tracing::warn!(%error, "could not refresh incremental library projection");
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

async fn provider_observer(state: Arc<AppState>, mut stop: watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut previous = String::new();
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = interval.tick() => {}
        }
        let fingerprint = provider_change_fingerprint(state.providers.statuses().await);
        if !previous.is_empty() && fingerprint != previous {
            state
                .publish(
                    ChangeSet::new("provider_state_changed", [ChangeScope::Providers])
                        .with_resources(["providers"]),
                )
                .await;
        }
        previous = fingerprint;
    }
}

fn provider_change_fingerprint(mut statuses: Vec<crate::model::ProviderStatus>) -> String {
    // Request/success timestamps are activity telemetry, not provider availability.
    // Background observers update them on every successful poll, so including them
    // would turn this state observer into a periodic invalidation loop.
    for status in &mut statuses {
        status.last_request_at = None;
        status.last_success_at = None;
        status.last_background_request_at = None;
    }
    serde_json::to_string(&statuses).unwrap_or_default()
}

async fn download_client_observer(state: Arc<AppState>, mut stop: watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut ticks = 0_u64;
    let mut failures = 0_u32;
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = interval.tick() => {}
        }
        ticks = ticks.wrapping_add(1);
        let reset = ticks == 1 || ticks.is_multiple_of(300);
        let mut changed = false;
        let mut inventory_changed = false;
        let mut live_updates = Vec::new();
        let mut live_removals = Vec::new();
        for (name, client) in &state.download_clients {
            let scan_started_at = Utc::now();
            match client.sync_downloads(reset).await {
                Ok(delta) => {
                    failures = 0;
                    if delta.downloads.is_empty() && delta.removed.is_empty() && !delta.full_update
                    {
                        continue;
                    }
                    let observations = delta
                        .downloads
                        .iter()
                        .map(|download| DownloadObservation {
                            torrent_name: Some(download.name.clone()),
                            live: download.live.clone(),
                            announce_host: download.announce_host.clone(),
                            tracker: download
                                .announce_host
                                .as_ref()
                                .and_then(|host| state.announce_hosts.get(host))
                                .cloned(),
                            plex_target: state
                                .plex
                                .as_ref()
                                .and_then(|plex| plex.target_for_path(&download.live.save_path)),
                        })
                        .collect::<Vec<_>>();
                    if let Err(error) = state.db.observe_downloads(&observations).await {
                        tracing::warn!(client = %name, %error, "could not persist qBittorrent delta");
                        continue;
                    }
                    if let Err(error) = state.db.mark_downloads_removed(name, &delta.removed).await
                    {
                        tracing::warn!(client = %name, %error, "could not persist removed torrents");
                        continue;
                    }
                    if delta.full_update
                        && let Err(error) =
                            state.db.complete_client_scan(name, scan_started_at).await
                    {
                        tracing::warn!(client = %name, %error, "could not complete qBittorrent snapshot");
                    }
                    reconcile_download_jobs(&state, name, &delta.downloads, &delta.removed).await;
                    live_updates
                        .extend(delta.downloads.iter().map(|download| download.live.clone()));
                    live_removals.extend(
                        delta
                            .removed
                            .iter()
                            .map(|info_hash| json!({ "client": name, "infoHash": info_hash })),
                    );
                    let _ = state.db.sync_import_tasks().await;
                    state.background_jobs.wake();
                    changed = true;
                    inventory_changed |= delta.inventory_changed;
                }
                Err(error) => {
                    failures = failures.saturating_add(1);
                    tracing::warn!(client = %name, failures, %error, "qBittorrent observer failed");
                    if failures == 1 {
                        state
                            .publish(
                                ChangeSet::new(
                                    "download_client_unavailable",
                                    [ChangeScope::Providers, ChangeScope::Activity],
                                )
                                .with_resources(["providers", "downloads"]),
                            )
                            .await;
                    }
                }
            }
        }
        if changed {
            let mut scopes = vec![ChangeScope::Activity, ChangeScope::Operations];
            let mut resources = vec!["downloads"];
            if inventory_changed {
                scopes.push(ChangeScope::Channels);
                resources.push("download-inventory");
            }
            state
                .publish(
                    ChangeSet::new("download_client_sync", scopes)
                        .with_resources(resources)
                        .with_payload(json!({
                            "downloads": live_updates,
                            "removed": live_removals,
                        })),
                )
                .await;
        }
        if failures > 0 {
            tokio::time::sleep(Duration::from_secs(2_u64.pow(failures.min(4)))).await;
        }
    }
}

async fn reconcile_download_jobs(
    state: &AppState,
    client_name: &str,
    downloads: &[crate::model::ObservedDownload],
    removed: &[String],
) {
    let Ok(jobs) = state.db.list_jobs().await else {
        return;
    };
    for job in jobs.into_iter().filter(|job| {
        job.info_hash.is_some()
            && state
                .profiles
                .get(&job.profile)
                .is_some_and(|profile| profile.client == client_name)
    }) {
        let Some(hash) = job.info_hash.as_deref() else {
            continue;
        };
        if let Some(download) = downloads
            .iter()
            .find(|download| download.live.info_hash.eq_ignore_ascii_case(hash))
        {
            let next = if download.live.progress >= 1.0 {
                DownloadState::Complete
            } else {
                DownloadState::Active
            };
            let _ = state
                .db
                .update_progress(
                    job.id,
                    next,
                    download.live.progress,
                    download.live.download_speed,
                    download.live.upload_speed,
                    download.live.eta,
                )
                .await;
        } else if removed
            .iter()
            .any(|removed| removed.eq_ignore_ascii_case(hash))
        {
            let _ = state
                .db
                .set_job_state(
                    job.id,
                    DownloadState::Unknown,
                    Some((
                        "torrent_missing",
                        "Torrent is no longer present in qBittorrent",
                    )),
                )
                .await;
        }
    }
}

async fn change_retention(state: Arc<AppState>, mut stop: watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(Duration::from_secs(3600));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = interval.tick() => {
                match state.db.prune_change_events(Utc::now() - chrono::Duration::hours(24)).await {
                    Ok(pruned) if pruned > 0 => tracing::debug!(pruned, "pruned change events"),
                    Ok(_) => {},
                    Err(error) => tracing::warn!(%error, "could not prune change events"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::provider_change_fingerprint;
    use crate::model::{ProviderCircuitState, ProviderQueueCounts, ProviderStatus};

    fn provider() -> ProviderStatus {
        ProviderStatus {
            id: "qbittorrent".into(),
            display_name: "qBittorrent".into(),
            kind: "download_client".into(),
            state: ProviderCircuitState::Available,
            reason_code: None,
            message: None,
            last_request_at: None,
            last_success_at: None,
            last_failure_at: None,
            retry_at: None,
            last_background_request_at: None,
            consecutive_failures: 0,
            minimum_interval_ms: 250,
            safe_minimum_interval_ms: 250,
            background_minimum_interval_ms: 250,
            safe_background_minimum_interval_ms: 250,
            max_concurrency: 1,
            safe_max_concurrency: 1,
            queued: ProviderQueueCounts::default(),
            can_pause: true,
            can_resume: false,
        }
    }

    #[test]
    fn provider_fingerprint_ignores_poll_telemetry_but_tracks_material_state() {
        let original = provider();
        let mut telemetry = original.clone();
        telemetry.last_request_at = Utc.timestamp_opt(1_700_000_000, 0).single();
        telemetry.last_success_at = Utc.timestamp_opt(1_700_000_001, 0).single();
        telemetry.last_background_request_at = Utc.timestamp_opt(1_700_000_002, 0).single();
        assert_eq!(
            provider_change_fingerprint(vec![original.clone()]),
            provider_change_fingerprint(vec![telemetry])
        );

        let mut unavailable = original;
        unavailable.state = ProviderCircuitState::Cooldown;
        assert_ne!(
            provider_change_fingerprint(vec![provider()]),
            provider_change_fingerprint(vec![unavailable])
        );
    }
}
