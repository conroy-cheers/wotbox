use super::*;

impl Database {
    pub async fn replace_channel_plan(
        &self,
        id: Uuid,
        expected_version: i32,
        fingerprint: &str,
        items: &[ChannelPackItem],
        preferences: &RuntimePreferences,
        manual_match: Option<(&crate::model::RecommendationSource, Uuid)>,
    ) -> Result<Option<i32>> {
        let transaction = self.begin_write().await?;
        let model = channel_pack::Entity::find_by_id(id.to_string())
            .one(&transaction)
            .await?
            .context("channel pack disappeared")?;
        if model.decision != "open"
            || model.plan_version != expected_version
            || self.pack_has_legacy_effects(&transaction, id).await?
            || self.get_runtime_preferences_on(&transaction).await? != *preferences
        {
            return Ok(None);
        }
        let version = model
            .plan_version
            .checked_add(1)
            .context("pack version exhausted")?;
        let mut active = model.into_active_model();
        active.plan_version = Set(version);
        active.preference_fingerprint = Set(fingerprint.into());
        active.update(&transaction).await?;
        channel_pack_item::Entity::delete_many()
            .filter(channel_pack_item::Column::PackId.eq(id.to_string()))
            .exec(&transaction)
            .await?;
        for item in items {
            channel_pack_item::ActiveModel {
                pack_id: Set(id.to_string()),
                ordinal: Set(item.ordinal as i32),
                item_json: Set(serde_json::to_value(item)?),
            }
            .insert(&transaction)
            .await?;
        }
        if let Some((source, release_id)) = manual_match
            && let Some((provider, key, identity)) =
                crate::channel::external_source_identity(source)
        {
            self.put_external_release_link_on(
                &transaction, provider, key, release_id, "manual", &identity, None,
                serde_json::json!({"artist": source.artist, "title": source.title, "year": source.year}),
            ).await?;
        }
        self.append_resource_change_event_on(
            &transaction,
            "channels,catalog",
            "channels",
            "pack_plan_replaced",
            None,
        )
        .await?;
        transaction.commit().await?;
        Ok(Some(version))
    }

    async fn pack_has_legacy_effects(
        &self,
        transaction: &DatabaseTransaction,
        id: Uuid,
    ) -> Result<bool> {
        // Existing versions stored the pack request key when creating a download job.
        // Item markers also cover replacements that did not need a download job.
        Ok(transaction.query_one_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            r#"
            SELECT 1 FROM channel_pack_items
             WHERE pack_id = ? AND (json_extract(item_json, '$.jobId') IS NOT NULL
                     OR json_extract(item_json, '$.planState') = 'submitted'
                     OR json_extract(item_json, '$.reason') = 'Excluded from the accepted pack by the user')
            UNION ALL
            SELECT 1 FROM download_request_receipts WHERE request_key GLOB ?
            LIMIT 1
            "#,
            [id.to_string().into(), format!("channel-pack:{id}:*").into()],
        )).await?.is_some())
    }

    pub async fn pack_receipt(
        &self,
        id: Uuid,
        request: &Value,
    ) -> Result<Option<PackDecisionResult>> {
        self.pack_receipt_on(&self.connection, id, request).await
    }

    async fn pack_receipt_on<C: ConnectionTrait>(
        &self,
        connection: &C,
        id: Uuid,
        request: &Value,
    ) -> Result<Option<PackDecisionResult>> {
        let receipt = channel_pack_receipt::Entity::find_by_id(id.to_string())
            .one(connection)
            .await?;
        receipt
            .map(|receipt| {
                if receipt.request_json != *request {
                    return Ok(PackDecisionResult::Conflict);
                }
                Ok(PackDecisionResult::Committed(serde_json::from_value(
                    receipt.result_json,
                )?))
            })
            .transpose()
    }

    pub async fn pack_input_revision(&self) -> Result<i64> {
        self.pack_input_revision_on(&self.connection).await
    }

    async fn pack_input_revision_on<C: ConnectionTrait>(&self, db: &C) -> Result<i64> {
        let row = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                "SELECT revision FROM pack_input_revision WHERE id = 1",
            ))
            .await?
            .context("missing pack input revision")?;
        Ok(row.try_get("", "revision")?)
    }

    pub async fn commit_channel_pack_decision(
        &self,
        mut pack: ChannelPack,
        request: Value,
        items: Option<&HashSet<u32>>,
        input_revision: Option<i64>,
        fingerprint: &str,
        decision: ChannelPackDecision,
        preferences: &RuntimePreferences,
        profiles: &HashMap<String, DownloadProfile>,
        free_bytes: &HashMap<String, i64>,
    ) -> Result<PackDecisionResult> {
        let transaction = self.begin_write().await?;
        if let Some(result) = self
            .pack_receipt_on(&transaction, pack.id, &request)
            .await?
        {
            return Ok(result);
        }
        let model = channel_pack::Entity::find_by_id(pack.id.to_string())
            .one(&transaction)
            .await?
            .context("channel pack disappeared")?;
        if let Some(expected) = input_revision
            && self.pack_input_revision_on(&transaction).await? != expected
        {
            return Ok(PackDecisionResult::Conflict);
        }
        if model.decision != "open"
            || model.plan_version != pack.plan_version
            || model.preference_fingerprint != fingerprint
            || serde_json::to_value(self.get_runtime_preferences_on(&transaction).await?)?
                != serde_json::to_value(preferences)?
        {
            return Ok(PackDecisionResult::Conflict);
        }
        let mut result = ChannelBatchResult {
            pack_id: pack.id,
            submitted: 0,
            skipped: pack.items.len(),
            jobs: Vec::new(),
        };
        if self.pack_has_legacy_effects(&transaction, pack.id).await? {
            return Ok(PackDecisionResult::Conflict);
        }
        let mut remaining = HashMap::new();
        for (client, free) in free_bytes {
            let row = transaction.query_one_raw(Statement::from_sql_and_values(
                transaction.get_database_backend(),
                r#"
                SELECT coalesce(sum(reservation.reserved_bytes), 0) AS reserved
                  FROM pack_capacity_reservations reservation
                  JOIN download_jobs job ON job.id = reservation.job_id
                 WHERE reservation.client = ?
                   AND job.state IN ('queued', 'fetching_metadata', 'submitting', 'active', 'unknown')
                "#,
                [client.as_str().into()],
            )).await?.context("missing capacity aggregate")?;
            remaining.insert(
                client.as_str(),
                free.saturating_sub(row.try_get("", "reserved")?),
            );
        }
        if decision == ChannelPackDecision::Accepted {
            for item in &mut pack.items {
                if !item.selectable {
                    continue;
                }
                if items.is_some_and(|selected| !selected.contains(&item.ordinal)) {
                    item.plan_state = crate::model::PackItemPlanState::Excluded;
                    item.plan = None;
                    item.reason = Some("Excluded from the accepted pack by the user".into());
                } else {
                    let mut job = None;
                    if let Some(plan) = &item.plan {
                        let key = format!(
                            "channel-pack:{}:item:{}:plan:{}",
                            pack.id, item.ordinal, pack.plan_version
                        );
                        let profile = profiles
                            .get(&plan.profile)
                            .context("missing acquisition profile")?;
                        let (download, created) = self
                            .create_job_on(
                                &transaction,
                                &plan.tracker,
                                plan.torrent_id,
                                &plan.profile,
                                plan.use_token,
                                Some(&key),
                            )
                            .await?;
                        self.store_download_destination(
                            &transaction,
                            download.id,
                            profile,
                            created,
                        )
                        .await?;
                        if created {
                            transaction
                                .execute_raw(Statement::from_sql_and_values(
                                    transaction.get_database_backend(),
                                    "DELETE FROM pack_capacity_reservations WHERE job_id = ?",
                                    [download.id.to_string().into()],
                                ))
                                .await?;
                            if let Some(size) = plan.size.filter(|size| *size > 0) {
                                if let Some(free) = remaining.get_mut(profile.client.as_str()) {
                                    if size > *free {
                                        return Ok(PackDecisionResult::CapacityChanged(
                                            profile.client.clone(),
                                        ));
                                    }
                                    *free -= size;
                                }
                                transaction.execute_raw(Statement::from_sql_and_values(
                                    transaction.get_database_backend(),
                                    "INSERT INTO pack_capacity_reservations (job_id, client, reserved_bytes) VALUES (?, ?, ?)",
                                    [download.id.to_string().into(), profile.client.clone().into(), size.into()],
                                )).await?;
                            }
                        }
                        job = Some(download);
                    }
                    if let Some(replacement) = &item.replacement {
                        let target = replacement
                            .downloads
                            .iter()
                            .find(|download| download.live.progress >= 1.0)
                            .or_else(|| replacement.downloads.first());
                        let id = self
                            .create_replacement_import_on(
                                &transaction,
                                CreateReplacementImport {
                                    request_key: Some(&format!(
                                        "pack:{}:{}:{}",
                                        pack.id, pack.plan_version, item.ordinal
                                    )),
                                    download_job_id: job.as_ref().map(|job| job.id),
                                    target_client: target
                                        .map(|download| download.live.client.as_str()),
                                    target_info_hash: target
                                        .map(|download| download.live.info_hash.as_str()),
                                    release_id: item
                                        .release
                                        .as_ref()
                                        .and_then(|release| release.id),
                                    tracker: &replacement.tracker,
                                    torrent_id: replacement.torrent_id,
                                    display_name: &item.source.title,
                                    target_complete: target
                                        .is_some_and(|download| download.live.progress >= 1.0),
                                    sources: &item.source.trumped_downloads,
                                    cleanup_mode: preferences.imports.trumped_cleanup,
                                },
                            )
                            .await?;
                        let key = format!("process-import:{id}:v1");
                        self.reactivate_terminal_background_job_on(&transaction, &key)
                            .await?;
                        self.enqueue_background_job_on(
                            &transaction,
                            EnqueueBackgroundJob {
                                deduplication_key: &key,
                                kind: crate::background::PROCESS_IMPORT,
                                payload: serde_json::json!({ "importId": id }),
                                provider_id: None,
                                lane: "maintenance",
                                priority: 40,
                                max_attempts: 20,
                                next_run_at: None,
                                parent_id: None,
                                recurring_interval_seconds: None,
                            },
                        )
                        .await?;
                    }
                    item.plan_state = crate::model::PackItemPlanState::Submitted;
                    item.job_id = job.as_ref().map(|job| job.id);
                    item.job = job.clone();
                    result.jobs.extend(job);
                    result.submitted += 1;
                    result.skipped -= 1;
                }
                let row = channel_pack_item::Entity::find_by_id((
                    pack.id.to_string(),
                    item.ordinal as i32,
                ))
                .one(&transaction)
                .await?
                .context("pack item disappeared")?;
                let mut row = row.into_active_model();
                row.item_json = Set(serde_json::to_value(item)?);
                row.update(&transaction).await?;
            }
        }
        let mut active = model.into_active_model();
        active.decision = Set(channel_pack_decision(&decision).into());
        active.decided_at = Set(Some(Utc::now().to_rfc3339()));
        active.update(&transaction).await?;
        channel_pack_receipt::ActiveModel {
            pack_id: Set(pack.id.to_string()),
            request_json: Set(request),
            result_json: Set(serde_json::to_value(&result)?),
        }
        .insert(&transaction)
        .await?;
        self.append_resource_change_event_on(
            &transaction,
            "channels,activity,operations",
            "channels,download-jobs,imports,background-jobs",
            "pack_decided",
            None,
        )
        .await?;
        transaction.commit().await?;
        Ok(PackDecisionResult::Committed(result))
    }

    #[cfg(test)]
    pub async fn create_job(
        &self,
        tracker: &str,
        torrent_id: i64,
        profile: &str,
        use_token: bool,
        idempotency_key: Option<&str>,
    ) -> Result<(DownloadJob, bool)> {
        let transaction = self.begin_write().await?;
        let result = self
            .create_job_on(
                &transaction,
                tracker,
                torrent_id,
                profile,
                use_token,
                idempotency_key,
            )
            .await?;
        transaction.commit().await?;
        Ok(result)
    }

    pub async fn accept_download(
        &self,
        tracker: &str,
        torrent_id: i64,
        profile: &DownloadProfile,
        use_token: bool,
        idempotency_key: Option<&str>,
    ) -> Result<DownloadJob> {
        let transaction = self.begin_write().await?;
        let (job, created) = self
            .create_job_on(
                &transaction,
                tracker,
                torrent_id,
                &profile.name,
                use_token,
                idempotency_key,
            )
            .await?;
        self.store_download_destination(&transaction, job.id, profile, created)
            .await?;
        if created {
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    transaction.get_database_backend(),
                    "DELETE FROM pack_capacity_reservations WHERE job_id = ?",
                    [job.id.to_string().into()],
                ))
                .await?;
        }
        transaction.commit().await?;
        Ok(job)
    }

    async fn store_download_destination(
        &self,
        tx: &DatabaseTransaction,
        id: Uuid,
        profile: &DownloadProfile,
        new_attempt: bool,
    ) -> Result<()> {
        // Existing receipts may be replayed after profile edits. Only a newly
        // authorized attempt can change its destination.
        if new_attempt {
            tx.execute_raw(Statement::from_sql_and_values(
                tx.get_database_backend(),
                "INSERT INTO download_destinations (job_id, profile_json, client, client_instance)
                 VALUES (?, ?, ?, (SELECT instance FROM configured_client_instances WHERE client = ?))
                 ON CONFLICT(job_id) DO UPDATE SET profile_json = excluded.profile_json, client = excluded.client,
                     attempt = download_destinations.attempt + 1, client_instance = excluded.client_instance",
                [id.to_string().into(), serde_json::to_string(profile)?.into(), profile.client.clone().into(), profile.client.clone().into()],
            )).await?;
        }
        Ok(())
    }

    pub async fn download_destination(&self, id: Uuid) -> Result<Option<DownloadProfile>> {
        let row = self
            .connection
            .query_one_raw(Statement::from_sql_and_values(
                self.connection.get_database_backend(),
                "SELECT profile_json FROM download_destinations WHERE job_id = ?",
                [id.to_string().into()],
            ))
            .await?;
        row.map(|row| {
            Ok(serde_json::from_str(
                &row.try_get::<String>("", "profile_json")?,
            )?)
        })
        .transpose()
    }

    pub async fn download_destination_matches(
        &self,
        id: Uuid,
        configured: Option<&str>,
    ) -> Result<bool> {
        let row = self
            .connection
            .query_one_raw(Statement::from_sql_and_values(
                self.connection.get_database_backend(),
                "SELECT client_instance FROM download_destinations WHERE job_id = ?",
                [id.to_string().into()],
            ))
            .await?;
        let stored = row
            .map(|row| row.try_get::<Option<String>>("", "client_instance"))
            .transpose()?
            .flatten();
        Ok(stored.as_deref() == configured)
    }

    pub async fn download_attempt(&self, job_id: Uuid) -> Result<Option<i64>> {
        let row = self
            .connection
            .query_one_raw(Statement::from_sql_and_values(
                self.connection.get_database_backend(),
                "SELECT attempt FROM download_destinations WHERE job_id = ?",
                [job_id.to_string().into()],
            ))
            .await?;
        row.map(|row| row.try_get("", "attempt"))
            .transpose()
            .map_err(Into::into)
    }

    async fn store_download_request(
        &self,
        tx: &DatabaseTransaction,
        key: Option<&str>,
        job: &DownloadJob,
        use_token: bool,
    ) -> Result<()> {
        if let Some(key) = key {
            tx.execute_raw(Statement::from_sql_and_values(
                tx.get_database_backend(),
                "INSERT INTO download_request_receipts (request_key, job_id, tracker, torrent_id, profile, use_token)
                 VALUES (?, ?, ?, ?, ?, ?)",
                [key.into(), job.id.to_string().into(), job.tracker.clone().into(),
                 job.torrent_id.into(), job.profile.clone().into(), use_token.into()],
            )).await?;
        }
        Ok(())
    }

    async fn create_job_on(
        &self,
        transaction: &DatabaseTransaction,
        tracker: &str,
        torrent_id: i64,
        profile: &str,
        use_token: bool,
        idempotency_key: Option<&str>,
    ) -> Result<(DownloadJob, bool)> {
        if let Some(key) = idempotency_key
            && let Some(row) = transaction.query_one_raw(Statement::from_sql_and_values(
                transaction.get_database_backend(),
                "SELECT job_id, tracker, torrent_id, profile, use_token FROM download_request_receipts WHERE request_key = ?",
                [key.into()],
            )).await?
        {
            if row.try_get::<String>("", "tracker")? != tracker
                || row.try_get::<i64>("", "torrent_id")? != torrent_id
                || row.try_get::<String>("", "profile")? != profile
                || row.try_get::<bool>("", "use_token")? != use_token
            {
                return Err(DownloadRequestConflict.into());
            }
            let model = download_job::Entity::find_by_id(row.try_get::<String>("", "job_id")?)
                .one(transaction).await?.context("download receipt has no job")?;
            let existing = job_from_model(model)?;
            if matches!(
                existing.state,
                DownloadState::Queued | DownloadState::FetchingMetadata | DownloadState::Submitting
            ) {
                self.ensure_download_submission_on(transaction, &existing, false)
                    .await?;
            }
                return Ok((existing, false));
        }
        if let Some(model) = download_job::Entity::find()
            .filter(download_job::Column::Tracker.eq(tracker))
            .filter(download_job::Column::TorrentId.eq(torrent_id))
            .filter(download_job::Column::Profile.eq(profile))
            .one(transaction)
            .await?
        {
            let existing = job_from_model(model.clone())?;
            if existing.state == DownloadState::Failed {
                let now = Utc::now();
                let mut active = model.into_active_model();
                active.idempotency_key = Set(idempotency_key.map(str::to_owned));
                active.use_token = Set(use_token);
                active.group_id = Set(None);
                active.info_hash = Set(None);
                active.name = Set(None);
                active.state = Set("queued".into());
                active.progress = Set(0.0);
                active.download_speed = Set(0);
                active.upload_speed = Set(0);
                active.eta = Set(None);
                active.error_code = Set(None);
                active.error_message = Set(None);
                active.updated_at = Set(now.to_rfc3339());
                active.update(transaction).await?;
                self.add_event_on(transaction, existing.id, &DownloadState::Queued, None)
                    .await?;
                let job = DownloadJob {
                    use_token,
                    group_id: None,
                    info_hash: None,
                    name: None,
                    state: DownloadState::Queued,
                    progress: 0.0,
                    download_speed: 0,
                    upload_speed: 0,
                    eta: None,
                    error_code: None,
                    error_message: None,
                    updated_at: now,
                    ..existing
                };
                self.ensure_download_submission_on(transaction, &job, true)
                    .await?;
                self.store_download_request(transaction, idempotency_key, &job, use_token)
                    .await?;
                return Ok((job, true));
            }
            if matches!(
                existing.state,
                DownloadState::Queued | DownloadState::FetchingMetadata | DownloadState::Submitting
            ) {
                self.ensure_download_submission_on(transaction, &existing, false)
                    .await?;
            }
            self.store_download_request(transaction, idempotency_key, &existing, use_token)
                .await?;
            return Ok((existing, false));
        }
        let now = Utc::now();
        let job = DownloadJob {
            id: Uuid::new_v4(),
            tracker: tracker.into(),
            torrent_id,
            group_id: None,
            profile: profile.into(),
            use_token,
            info_hash: None,
            name: None,
            state: DownloadState::Queued,
            progress: 0.0,
            download_speed: 0,
            upload_speed: 0,
            eta: None,
            error_code: None,
            error_message: None,
            created_at: now,
            updated_at: now,
        };
        download_job::ActiveModel {
            id: Set(job.id.to_string()),
            idempotency_key: Set(idempotency_key.map(str::to_owned)),
            tracker: Set(tracker.into()),
            torrent_id: Set(torrent_id),
            group_id: Set(None),
            profile: Set(profile.into()),
            use_token: Set(use_token),
            info_hash: Set(None),
            name: Set(None),
            state: Set(job.state.as_str().into()),
            progress: Set(0.0),
            download_speed: Set(0),
            upload_speed: Set(0),
            eta: Set(None),
            error_code: Set(None),
            error_message: Set(None),
            created_at: Set(now.to_rfc3339()),
            updated_at: Set(now.to_rfc3339()),
        }
        .insert(transaction)
        .await?;
        self.add_event_on(transaction, job.id, &job.state, None)
            .await?;
        self.ensure_download_submission_on(transaction, &job, true)
            .await?;
        self.store_download_request(transaction, idempotency_key, &job, use_token)
            .await?;
        Ok((job, true))
    }

    pub(super) async fn ensure_download_submission_on<C: ConnectionTrait>(
        &self,
        connection: &C,
        job: &DownloadJob,
        rearm: bool,
    ) -> Result<Uuid> {
        let key = format!("submit-download:{}:v1", job.id);
        if let Some(model) = background_job::Entity::find()
            .filter(background_job::Column::DeduplicationKey.eq(&key))
            .one(connection)
            .await?
        {
            let id = Uuid::parse_str(&model.id)?;
            let should_rearm = rearm || matches!(model.state.as_str(), "completed" | "failed");
            let mut active = model.into_active_model();
            active.provider_id = Set(Some(format!("tracker:{}", job.tracker)));
            active.lane = Set("download".into());
            active.priority = Set(100);
            active.max_attempts = Set(20);
            active.updated_at = Set(Utc::now().to_rfc3339());
            if should_rearm {
                active.state = Set("pending".into());
                active.attempts = Set(0);
                active.deferrals = Set(0);
                active.next_run_at = Set(None);
                active.lease_owner = Set(None);
                active.lease_until = Set(None);
                active.last_error_code = Set(None);
                active.last_error_message = Set(None);
                active.finished_at = Set(None);
                active.cancelled_at = Set(None);
            }
            active.update(connection).await?;
            return Ok(id);
        }
        self.enqueue_background_job_on(
            connection,
            EnqueueBackgroundJob {
                deduplication_key: &key,
                kind: "submit_download",
                payload: serde_json::json!({ "jobId": job.id }),
                provider_id: Some(format!("tracker:{}", job.tracker)),
                lane: "download",
                priority: 100,
                max_attempts: 20,
                next_run_at: None,
                parent_id: None,
                recurring_interval_seconds: None,
            },
        )
        .await
    }
}
