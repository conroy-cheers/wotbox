use super::*;
use crate::{
    change::{ChangeScope, ChangeSet},
    qbittorrent::DownloadClientDelta,
};
use serde_json::json;
use std::collections::BTreeSet;

impl Database {
    pub async fn publish_client_observation(
        &self,
        name: &str,
        started_at: DateTime<Utc>,
        delta: &DownloadClientDelta,
        observations: &[DownloadObservation],
        profiles: &[String],
    ) -> Result<()> {
        let generation = delta.full_update.then(|| started_at.to_rfc3339());
        if let Some(generation) = &generation {
            self.stage_client_inventory(name, generation, observations)
                .await?;
        }
        let transaction = self.begin_write().await?;
        let changed = if let Some(generation) = &generation {
            Some(
                self.changed_client_inventory_on(&transaction, name, generation)
                    .await?,
            )
        } else {
            None
        };
        for observation in observations {
            anyhow::ensure!(
                observation.live.client == name,
                "Observation belongs to a different download client"
            );
            if changed.as_ref().is_none_or(|hashes| {
                hashes.contains(&observation.live.info_hash.to_ascii_lowercase())
            }) {
                self.observe_download_on(&transaction, observation).await?;
            }
        }
        if !delta.full_update {
            self.remove_observed_downloads_on(&transaction, name, &delta.removed)
                .await?;
            self.update_client_inventory_delta_on(&transaction, name, delta)
                .await?;
        }
        let observed = delta
            .downloads
            .iter()
            .map(|download| (download.live.info_hash.to_ascii_lowercase(), &download.live))
            .collect::<std::collections::HashMap<_, _>>();
        let mut missing = delta.removed.iter().cloned().collect::<BTreeSet<_>>();
        let affected = observed
            .keys()
            .chain(missing.iter())
            .cloned()
            .collect::<Vec<_>>();
        let jobs = self
            .jobs_for_client_observation_on(
                &transaction,
                name,
                profiles,
                &affected,
                delta.full_update,
            )
            .await?;
        let mut job_state_changed = false;
        for job in jobs {
            let Some(hash) = job.info_hash.as_deref() else {
                continue;
            };
            if let Some(live) = observed.get(hash) {
                let next = if live.progress >= 1.0 {
                    DownloadState::Complete
                } else {
                    DownloadState::Active
                };
                job_state_changed |= job.state != next;
                if job.state != next
                    || job.progress != live.progress
                    || job.download_speed != live.download_speed
                    || job.upload_speed != live.upload_speed
                    || job.eta != live.eta
                {
                    self.update_progress_on(
                        &transaction,
                        job.id,
                        next,
                        live.progress,
                        live.download_speed,
                        live.upload_speed,
                        live.eta,
                    )
                    .await?;
                }
            } else if delta.full_update || missing.contains(hash) {
                if matches!(job.state, DownloadState::Active | DownloadState::Complete)
                    && (!delta.full_update || job.updated_at <= started_at)
                {
                    self.set_job_state_on(
                        &transaction,
                        job.id,
                        DownloadState::Unknown,
                        Some((
                            "torrent_missing",
                            "Torrent is no longer present in qBittorrent",
                        )),
                    )
                    .await?;
                    missing.insert(hash.to_owned());
                }
            }
        }
        if delta.full_update {
            self.complete_client_scan_on(&transaction, name, started_at, generation.as_deref())
                .await?;
        }
        let mut resources = vec!["downloads"];
        let reset_client = delta.full_update || observed.len() + missing.len() > 500;
        if delta.inventory_changed || reset_client || !missing.is_empty() {
            resources.extend(["download-inventory", "download-jobs", "imports"]);
        }
        if job_state_changed && !resources.contains(&"download-jobs") {
            resources.push("download-jobs");
        }
        if delta.full_update {
            resources.push("library");
        }
        // A large full snapshot is reloaded through the read endpoint instead of being
        // repeated as a single unbounded SSE payload.
        let change = ChangeSet::new(
            "download_client_sync",
            [ChangeScope::Activity, ChangeScope::Channels],
        )
        .with_resources(resources);
        let change = if reset_client {
            change.with_payload(json!({ "resetClient": name }))
        } else {
            change.with_payload(json!({
            "downloads": observed.into_values().collect::<Vec<_>>(),
            "removed": missing.into_iter().map(|info_hash| json!({ "client": name, "infoHash": info_hash })).collect::<Vec<_>>()
        }))
        };
        let event_id = self
            .append_resource_change_event_on(
                &transaction,
                &change.encode_scopes(),
                &change.encode_resources(),
                change.reason(),
                change.payload(),
            )
            .await?;
        if let Some(generation) = generation {
            transaction.execute_raw(Statement::from_sql_and_values(
                transaction.get_database_backend(),
                "INSERT INTO client_inventory_generations(client, generation, event_id, published_at)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT(client) DO UPDATE SET generation = excluded.generation,
                    event_id = excluded.event_id, published_at = excluded.published_at",
                [name.into(), generation.into(), event_id.into(), Utc::now().to_rfc3339().into()],
            )).await?;
        } else {
            transaction.execute_raw(Statement::from_sql_and_values(
                transaction.get_database_backend(),
                "UPDATE client_inventory_generations SET event_id = ?, published_at = ? WHERE client = ?",
                [event_id.into(), Utc::now().to_rfc3339().into(), name.into()],
            )).await?;
        }
        transaction.commit().await?;
        Ok(())
    }
    async fn stage_client_inventory(
        &self,
        client: &str,
        generation: &str,
        observations: &[DownloadObservation],
    ) -> Result<()> {
        for batch in observations.chunks(250) {
            let rows = batch
                .iter()
                .map(|item| {
                    anyhow::ensure!(
                        item.live.client == client,
                        "Observation belongs to a different download client"
                    );
                    let name = item
                        .torrent_name
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(|value| value.chars().take(500).collect::<String>());
                    Ok(json!([
                        item.live.info_hash.to_ascii_lowercase(),
                        item.live,
                        name,
                        item.announce_host,
                        item.tracker
                    ]))
                })
                .collect::<Result<Vec<_>>>()?;
            let transaction = self.begin_write().await?;
            transaction.execute_raw(Statement::from_sql_and_values(
                transaction.get_database_backend(),
                "INSERT INTO client_inventory_members(client, generation, info_hash, observed_json, torrent_name, announce_host, tracker)
                 SELECT ?, ?, json_extract(value, '$[0]'), json_extract(value, '$[1]'),
                    json_extract(value, '$[2]'), json_extract(value, '$[3]'), json_extract(value, '$[4]')
                   FROM json_each(?) WHERE true
                 ON CONFLICT DO NOTHING",
                [client.into(), generation.into(), serde_json::to_string(&rows)?.into()],
            )).await?;
            transaction.commit().await?;
            tokio::task::yield_now().await;
        }
        Ok(())
    }

    async fn changed_client_inventory_on(
        &self,
        transaction: &DatabaseTransaction,
        client: &str,
        generation: &str,
    ) -> Result<HashSet<String>> {
        transaction.query_all_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "SELECT member.info_hash
               FROM client_inventory_members member
               LEFT JOIN download_release_links link ON link.client = member.client
                    AND link.info_hash = member.info_hash
              WHERE member.client = ? AND member.generation = ?
                AND (link.info_hash IS NULL OR NOT link.present
                  OR json(link.observed_json) IS NOT member.observed_json
                  OR link.announce_host IS NOT member.announce_host
                  OR (member.torrent_name IS NOT NULL AND link.torrent_name IS NOT member.torrent_name)
                  OR (link.resolution_state <> 'linked' AND link.tracker IS NOT member.tracker)
                  OR (member.tracker IS NOT NULL AND link.resolution_state = 'pending'
                      AND (link.next_retry_at IS NULL OR link.next_retry_at <= ?))
                  OR (member.tracker IS NOT NULL AND link.resolution_state = 'linked'
                      AND (link.group_id IS NULL OR link.torrent_id IS NULL)))",
            [client.into(), generation.into(), Utc::now().to_rfc3339().into()],
        )).await?.into_iter().map(|row| Ok(row.try_get("", "info_hash")?)).collect()
    }

    async fn update_client_inventory_delta_on(
        &self,
        transaction: &DatabaseTransaction,
        client: &str,
        delta: &DownloadClientDelta,
    ) -> Result<()> {
        if !delta.inventory_changed && delta.removed.is_empty() {
            return Ok(());
        }
        let hashes = delta
            .downloads
            .iter()
            .map(|item| item.live.info_hash.to_ascii_lowercase())
            .collect::<Vec<_>>();
        transaction
            .execute_raw(Statement::from_sql_and_values(
                transaction.get_database_backend(),
                "INSERT INTO client_inventory_members(client, generation, info_hash)
             SELECT head.client, head.generation, incoming.value
               FROM client_inventory_generations head, json_each(?) incoming WHERE head.client = ?
             ON CONFLICT DO NOTHING",
                [serde_json::to_string(&hashes)?.into(), client.into()],
            ))
            .await?;
        transaction.execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "DELETE FROM client_inventory_members WHERE client = ?
               AND generation = (SELECT generation FROM client_inventory_generations WHERE client = ?)
               AND info_hash IN (SELECT value FROM json_each(?))",
            [client.into(), client.into(), serde_json::to_string(&delta.removed)?.into()],
        )).await?;
        Ok(())
    }

    pub async fn prune_inactive_inventory(&self) -> Result<()> {
        // A newly staged generation is protected until it is either published or
        // old enough that a later restart and full poll can supplant it.
        let cutoff = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        loop {
            let transaction = self.begin_write().await?;
            let removed = transaction.execute_raw(Statement::from_sql_and_values(
                transaction.get_database_backend(),
                "DELETE FROM client_inventory_members
                 WHERE (client, generation, info_hash) IN (
                   SELECT member.client, member.generation, member.info_hash
                     FROM client_inventory_members member
                    WHERE member.generation < ?
                      AND NOT EXISTS (SELECT 1 FROM client_inventory_generations head
                                       WHERE head.client = member.client AND head.generation = member.generation)
                   LIMIT 1000
                 )",
                [cutoff.clone().into()],
            )).await?.rows_affected();
            transaction.commit().await?;
            if removed < 1_000 {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
    }
}
