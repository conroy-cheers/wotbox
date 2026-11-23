use std::{future::Future, sync::Arc};

use anyhow::{Result, anyhow};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr, ExecResult,
    QueryResult, SqliteTransactionMode, Statement, TransactionOptions, TransactionTrait,
};

tokio::task_local! {
    static READ_SNAPSHOT: (Arc<()>, Arc<DatabaseTransaction>);
}

#[derive(Clone)]
pub(super) struct SnapshotConnection {
    pub(super) pool: DatabaseConnection,
    identity: Arc<()>,
}

impl SnapshotConnection {
    pub(super) fn new(pool: DatabaseConnection) -> Self {
        Self {
            pool,
            identity: Arc::new(()),
        }
    }

    fn snapshot(&self) -> Option<Arc<DatabaseTransaction>> {
        READ_SNAPSHOT
            .try_with(|(identity, transaction)| {
                Arc::ptr_eq(identity, &self.identity).then(|| transaction.clone())
            })
            .ok()
            .flatten()
    }

    pub(super) fn has_snapshot(&self) -> bool {
        self.snapshot().is_some()
    }

    pub(super) fn reject_snapshot_write(&self) -> Result<(), DbErr> {
        if self.snapshot().is_some() {
            return Err(DbErr::Custom("Cannot write inside a read snapshot".into()));
        }
        Ok(())
    }

    async fn guarded_write(&self) -> Result<Option<DatabaseTransaction>, DbErr> {
        self.reject_snapshot_write()?;
        let Ok((id, owner)) = super::JOB_CLAIM.try_with(Clone::clone) else {
            return Ok(None);
        };
        let transaction = self
            .pool
            .begin_with_options(TransactionOptions {
                sqlite_transaction_mode: Some(SqliteTransactionMode::Immediate),
                ..Default::default()
            })
            .await?;
        let row = transaction
            .query_one_raw(Statement::from_sql_and_values(
                transaction.get_database_backend(),
                "SELECT 1 FROM background_jobs
             WHERE id = ? AND state = 'running' AND lease_owner = ? AND lease_until > ?",
                [
                    id.to_string().into(),
                    owner.into(),
                    chrono::Utc::now().to_rfc3339().into(),
                ],
            ))
            .await?;
        if row.is_none() {
            return Err(DbErr::Custom(
                "Background job no longer owns its lease".into(),
            ));
        }
        Ok(Some(transaction))
    }

    pub(super) async fn read_snapshot<T>(
        &self,
        operation: impl Future<Output = T>,
    ) -> Result<(i64, T)> {
        let transaction = Arc::new(self.pool.begin().await?);
        let cursor = cursor_on(transaction.as_ref()).await?;
        let result = READ_SNAPSHOT
            .scope((self.identity.clone(), transaction.clone()), operation)
            .await;
        Arc::try_unwrap(transaction)
            .map_err(|_| anyhow!("Read snapshot outlived its request"))?
            .commit()
            .await?;
        Ok((cursor, result))
    }
}

pub(super) async fn cursor_on(db: &impl ConnectionTrait) -> Result<i64> {
    let row = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT seq FROM sqlite_sequence WHERE name = 'change_events'",
        ))
        .await?;
    Ok(row
        .map(|row| row.try_get("", "seq"))
        .transpose()?
        .unwrap_or(0))
}

// SeaORM uses the query methods for mutations with a RETURNING clause.
fn mutation_statement(statement: &Statement) -> bool {
    statement
        .sql
        .split_ascii_whitespace()
        .next()
        .is_some_and(|keyword| {
            ["INSERT", "UPDATE", "DELETE", "REPLACE"]
                .iter()
                .any(|operation| keyword.eq_ignore_ascii_case(operation))
        })
}

#[async_trait::async_trait]
impl ConnectionTrait for SnapshotConnection {
    fn get_database_backend(&self) -> DbBackend {
        self.pool.get_database_backend()
    }

    async fn execute_raw(&self, stmt: Statement) -> Result<ExecResult, DbErr> {
        if let Some(transaction) = self.guarded_write().await? {
            let result = transaction.execute_raw(stmt).await?;
            transaction.commit().await?;
            Ok(result)
        } else {
            self.pool.execute_raw(stmt).await
        }
    }

    async fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
        if let Some(transaction) = self.guarded_write().await? {
            let result = transaction.execute_unprepared(sql).await?;
            transaction.commit().await?;
            Ok(result)
        } else {
            self.pool.execute_unprepared(sql).await
        }
    }

    async fn query_one_raw(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
        if mutation_statement(&stmt)
            && let Some(transaction) = self.guarded_write().await?
        {
            let result = transaction.query_one_raw(stmt).await?;
            transaction.commit().await?;
            return Ok(result);
        }
        if let Some(snapshot) = self.snapshot() {
            snapshot.query_one_raw(stmt).await
        } else {
            self.pool.query_one_raw(stmt).await
        }
    }

    async fn query_all_raw(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
        if mutation_statement(&stmt)
            && let Some(transaction) = self.guarded_write().await?
        {
            let result = transaction.query_all_raw(stmt).await?;
            transaction.commit().await?;
            return Ok(result);
        }
        if let Some(snapshot) = self.snapshot() {
            snapshot.query_all_raw(stmt).await
        } else {
            self.pool.query_all_raw(stmt).await
        }
    }
}
