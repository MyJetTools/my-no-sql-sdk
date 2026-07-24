use std::{collections::BTreeMap, marker::PhantomData, sync::Arc};

use flurl::FlUrl;

use my_no_sql_abstractions::{DataSynchronizationPeriod, MyNoSqlEntity, MyNoSqlEntitySerializer};

use serde::{Deserialize, Serialize};

use crate::{MyNoSqlDataWriterBuilder, MyNoSqlDataWriterWithRetries, MyNoSqlWriterSettings};

use super::{fl_url_factory::FlUrlFactory, DataWriterError, UpdateReadStatistics};

/// Default number of read-modify-write attempts for [`MyNoSqlDataWriter::update_entity`]
/// before a persistent optimistic-concurrency conflict is surfaced.
pub const DEFAULT_UPDATE_ENTITY_MAX_ATTEMPTS: usize = 5;

pub struct CreateTableParams {
    pub persist: bool,
    pub max_partitions_amount: Option<usize>,
    pub max_rows_per_partition_amount: Option<usize>,
}

impl CreateTableParams {
    pub fn populate_params(&self, mut fl_url: FlUrl) -> FlUrl {
        if let Some(max_partitions_amount) = self.max_partitions_amount {
            fl_url = fl_url.append_query_param(
                "maxPartitionsAmount",
                Some(max_partitions_amount.to_string()),
            )
        };

        if let Some(max_rows_per_partition_amount) = self.max_rows_per_partition_amount {
            fl_url = fl_url.append_query_param(
                "maxRowsPerPartitionAmount",
                Some(max_rows_per_partition_amount.to_string()),
            )
        };

        if !self.persist {
            fl_url = fl_url.append_query_param("persist", Some("false"));
        };

        fl_url
    }
}

pub struct MyNoSqlDataWriter<TEntity: MyNoSqlEntity + Sync + Send> {
    sync_period: DataSynchronizationPeriod,
    phantom: PhantomData<TEntity>,
    fl_url_factory: FlUrlFactory,
}

impl<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send> MyNoSqlDataWriter<TEntity> {
    pub fn create_with_builder(
        settings: Arc<dyn MyNoSqlWriterSettings + Send + Sync + 'static>,
    ) -> MyNoSqlDataWriterBuilder<TEntity> {
        MyNoSqlDataWriterBuilder::new(settings)
    }
    pub fn new(
        settings: Arc<dyn MyNoSqlWriterSettings + Send + Sync + 'static>,
        auto_create_table_params: Option<CreateTableParams>,
        sync_period: DataSynchronizationPeriod,
    ) -> Self {
        crate::PING_POOL.register(settings.clone(), TEntity::TABLE_NAME);

        Self {
            phantom: PhantomData,
            sync_period,
            fl_url_factory: FlUrlFactory::new(
                settings,
                auto_create_table_params.map(|itm| itm.into()),
                TEntity::TABLE_NAME,
            ),
        }
    }

    /// Falls back to HTTP/1.1 - for MyNoSqlServer instances which do not support HTTP/2.
    pub fn use_h1(&mut self) {
        self.fl_url_factory.use_h1();
        crate::PING_POOL.use_h1(self.fl_url_factory.get_settings(), TEntity::TABLE_NAME);
    }

    pub async fn create_table(&self, params: CreateTableParams) -> Result<(), DataWriterError> {
        let (fl_url, url) = self.fl_url_factory.get_fl_url().await?;

        super::execution::create_table(
            fl_url,
            url.as_str(),
            TEntity::TABLE_NAME,
            params,
            &self.sync_period,
        )
        .await
    }

    #[cfg(all(unix, feature = "with-ssh"))]
    pub fn set_ssh_security_credentials_resolver(
        &mut self,
        resolver: Arc<
            dyn flurl::my_ssh::ssh_settings::SshSecurityCredentialsResolver + Send + Sync,
        >,
    ) {
        self.fl_url_factory.ssh_security_credentials_resolver = Some(resolver);
    }

    pub async fn create_table_if_not_exists(
        &self,
        params: &CreateTableParams,
    ) -> Result<(), DataWriterError> {
        let (fl_url, url) = self.fl_url_factory.get_fl_url().await?;
        super::execution::create_table_if_not_exists(
            fl_url,
            url.as_str(),
            TEntity::TABLE_NAME,
            params,
            self.sync_period,
        )
        .await
    }

    pub fn with_retries(&self, max_attempts: usize) -> MyNoSqlDataWriterWithRetries<TEntity> {
        MyNoSqlDataWriterWithRetries::new(
            self.fl_url_factory.clone(),
            self.sync_period,
            max_attempts,
        )
    }

    pub async fn insert_entity(&self, entity: &TEntity) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::insert_entity(fl_url, entity, &self.sync_period).await
    }

    pub async fn insert_or_replace_entity(&self, entity: &TEntity) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::insert_or_replace_entity(fl_url, entity, &self.sync_period).await
    }

    /// Optimistic-concurrency replace: the entity must carry the `TimeStamp` it was read
    /// with. If the stored row's `TimeStamp` still matches, it is replaced; otherwise this
    /// fails with [`DataWriterError::RecordIsChanged`] (409). A missing row yields
    /// [`DataWriterError::RecordNotFound`] (404), a missing `TimeStamp` an HTTP 400.
    ///
    /// Prefer [`Self::update_entity`] which drives the read → mutate → replace → retry loop
    /// for you.
    pub async fn replace_entity(&self, entity: &TEntity) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::replace_entity(fl_url, entity, &self.sync_period).await
    }

    /// Read-modify-write with optimistic concurrency, retrying `RecordIsChanged` conflicts
    /// with the default attempt limit ([`DEFAULT_UPDATE_ENTITY_MAX_ATTEMPTS`]).
    ///
    /// Reads the row, calls `update` on it (mutate whatever fields you need — do **not**
    /// touch `time_stamp`, it carries the read version), and replaces it. On a concurrent
    /// change (409) it re-reads the fresh version and re-applies `update`. Returns
    /// `Ok(None)` if the row does not exist, `Ok(Some(updated))` on success.
    pub async fn update_entity<TFn: FnMut(&mut TEntity)>(
        &self,
        partition_key: &str,
        row_key: &str,
        update: TFn,
    ) -> Result<Option<TEntity>, DataWriterError> {
        self.update_entity_with_max_attempts(
            partition_key,
            row_key,
            DEFAULT_UPDATE_ENTITY_MAX_ATTEMPTS,
            update,
        )
        .await
    }

    /// [`Self::update_entity`] with an explicit retry limit for the optimistic-concurrency
    /// loop. On exhaustion the last [`DataWriterError::RecordIsChanged`] is returned.
    pub async fn update_entity_with_max_attempts<TFn: FnMut(&mut TEntity)>(
        &self,
        partition_key: &str,
        row_key: &str,
        max_attempts: usize,
        update: TFn,
    ) -> Result<Option<TEntity>, DataWriterError> {
        super::execution::run_read_modify_write(
            max_attempts,
            update,
            || self.get_entity(partition_key, row_key, None),
            |entity| async move {
                let result = self.replace_entity(&entity).await;
                (entity, result)
            },
        )
        .await
    }

    pub async fn bulk_insert_or_replace(
        &self,
        entities: &[TEntity],
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::bulk_insert_or_replace(fl_url, entities, &self.sync_period).await
    }

    /// Deletes rows described as PartitionKey -> RowKeys
    pub async fn bulk_delete(
        &self,
        rows_to_delete: &BTreeMap<String, Vec<String>>,
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::bulk_delete::<TEntity>(fl_url, rows_to_delete, &self.sync_period).await
    }

    pub async fn get_entity(
        &self,
        partition_key: &str,
        row_key: &str,
        update_read_statistics: Option<UpdateReadStatistics>,
    ) -> Result<Option<TEntity>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::get_entity(
            fl_url,
            partition_key,
            row_key,
            update_read_statistics.as_ref(),
        )
        .await
    }

    pub async fn get_by_partition_key(
        &self,
        partition_key: &str,
        update_read_statistics: Option<UpdateReadStatistics>,
    ) -> Result<Option<Vec<TEntity>>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::get_by_partition_key(
            fl_url,
            partition_key,
            update_read_statistics.as_ref(),
        )
        .await
    }

    pub async fn get_enum_case_models_by_partition_key<
        TResult: MyNoSqlEntity
            + my_no_sql_abstractions::GetMyNoSqlEntitiesByPartitionKey
            + From<TEntity>
            + Sync
            + Send
            + 'static,
    >(
        &self,
        update_read_statistics: Option<UpdateReadStatistics>,
    ) -> Result<Option<Vec<TResult>>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::get_enum_case_models_by_partition_key(
            fl_url,
            update_read_statistics.as_ref(),
        )
        .await
    }

    pub async fn get_enum_case_model<
        TResult: MyNoSqlEntity
            + From<TEntity>
            + my_no_sql_abstractions::GetMyNoSqlEntity
            + Sync
            + Send
            + 'static,
    >(
        &self,
        update_read_statistics: Option<UpdateReadStatistics>,
    ) -> Result<Option<TResult>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::get_enum_case_model(fl_url, update_read_statistics.as_ref()).await
    }

    pub async fn get_by_row_key(
        &self,
        row_key: &str,
    ) -> Result<Option<Vec<TEntity>>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::get_by_row_key(fl_url, row_key).await
    }

    pub async fn get_partition_keys(
        &self,
        skip: Option<i32>,
        limit: Option<i32>,
    ) -> Result<Vec<String>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::get_partition_keys(fl_url, TEntity::TABLE_NAME, skip, limit).await
    }

    pub async fn delete_enum_case<
        TResult: MyNoSqlEntity
            + From<TEntity>
            + my_no_sql_abstractions::GetMyNoSqlEntity
            + Sync
            + Send
            + 'static,
    >(
        &self,
    ) -> Result<Option<TResult>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::delete_enum_case(fl_url).await
    }

    pub async fn delete_enum_case_with_row_key<
        TResult: MyNoSqlEntity
            + From<TEntity>
            + my_no_sql_abstractions::GetMyNoSqlEntitiesByPartitionKey
            + Sync
            + Send
            + 'static,
    >(
        &self,
        row_key: &str,
    ) -> Result<Option<TResult>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::delete_enum_case_with_row_key(fl_url, row_key).await
    }

    pub async fn delete_row(
        &self,
        partition_key: &str,
        row_key: &str,
    ) -> Result<Option<TEntity>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::delete_row(fl_url, partition_key, row_key).await
    }

    pub async fn delete_partitions(&self, partition_keys: &[&str]) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::delete_partitions(fl_url, TEntity::TABLE_NAME, partition_keys).await
    }

    pub async fn get_all(&self) -> Result<Option<Vec<TEntity>>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::get_all(fl_url).await
    }

    pub async fn clean_table_and_bulk_insert(
        &self,
        entities: &[TEntity],
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::clean_table_and_bulk_insert(fl_url, entities, &self.sync_period).await
    }

    pub async fn clean_partition_and_bulk_insert(
        &self,
        partition_key: &str,
        entities: &[TEntity],
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::clean_partition_and_bulk_insert(
            fl_url,
            partition_key,
            entities,
            &self.sync_period,
        )
        .await
    }

    /// Insert-or-replace-if-new for a single entity: the row is written only when it is
    /// missing or the incoming `TimeStamp` is strictly greater than the stored one.
    ///
    /// The `TimeStamp` here is the object's version in a distributed system and is
    /// **mandatory** — set `entity.time_stamp` to your real version (e.g.
    /// `DateTimeAsMicroseconds::now().into()`) before calling. A default/unset `Timestamp`
    /// serializes to `null`/omitted and the server rejects the request with HTTP 400.
    pub async fn insert_or_replace_entity_if_new(
        &self,
        entity: &TEntity,
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::insert_or_replace_entity_if_new(fl_url, entity, &self.sync_period).await
    }

    /// Bulk insert-or-replace-if-new. Same per-row rule and the same mandatory-`TimeStamp`
    /// contract as [`Self::insert_or_replace_entity_if_new`]. An empty slice is a no-op.
    pub async fn bulk_insert_or_replace_if_new(
        &self,
        entities: &[TEntity],
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::bulk_insert_or_replace_if_new(fl_url, entities, &self.sync_period).await
    }

    /// One-call chunked insert-or-replace-if-new: slices `entities` into chunks of
    /// `chunk_size`, starts a server-side process with the first chunk, uploads the rest
    /// under the issued `processId`, then commits. If any step fails, a best-effort
    /// `Cancel` is issued and the original error is returned (so a half-uploaded process
    /// never lingers on the server). An empty slice is a no-op.
    ///
    /// Same mandatory-`TimeStamp` contract as [`Self::insert_or_replace_entity_if_new`].
    /// Use this when you already have all entities in memory; for streaming, drive the
    /// [`Self::insert_or_replace_if_new_by_chunks_start`] / `..._append` / `..._commit` /
    /// `..._cancel` primitives directly.
    pub async fn bulk_insert_or_replace_if_new_by_chunks(
        &self,
        entities: &[TEntity],
        chunk_size: usize,
    ) -> Result<(), DataWriterError> {
        assert!(chunk_size > 0, "chunk_size must be greater than 0");

        if entities.is_empty() {
            return Ok(());
        }

        let mut chunks = entities.chunks(chunk_size);

        // The first chunk starts the process and yields the id every following request
        // must carry. Once it succeeds the process exists on the server, so from here on
        // any failure has to be cleaned up with a Cancel.
        let process_id = self
            .insert_or_replace_if_new_by_chunks_start(chunks.next().unwrap())
            .await?;

        let result = async {
            for chunk in chunks {
                self.insert_or_replace_if_new_by_chunks_append(&process_id, chunk)
                    .await?;
            }
            self.insert_or_replace_if_new_by_chunks_commit(&process_id)
                .await
        }
        .await;

        if let Err(err) = result {
            // Best-effort cleanup — keep the original error regardless of the cancel result.
            let _ = self
                .insert_or_replace_if_new_by_chunks_cancel(&process_id)
                .await;
            return Err(err);
        }

        Ok(())
    }

    /// Low-level chunked flow: starts a new process by uploading the first chunk and
    /// returns the server-issued `processId` to be replayed on every following call.
    /// Nothing is applied to the table until [`Self::insert_or_replace_if_new_by_chunks_commit`].
    /// Each row must carry its own non-default `TimeStamp`.
    pub async fn insert_or_replace_if_new_by_chunks_start(
        &self,
        entities: &[TEntity],
    ) -> Result<String, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::insert_or_replace_if_new_by_chunks_upload(fl_url, entities, None).await
    }

    /// Low-level chunked flow: appends a further chunk to an already-started process.
    pub async fn insert_or_replace_if_new_by_chunks_append(
        &self,
        process_id: &str,
        entities: &[TEntity],
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::insert_or_replace_if_new_by_chunks_upload(
            fl_url,
            entities,
            Some(process_id),
        )
        .await?;
        Ok(())
    }

    /// Low-level chunked flow: commits an accumulated process (applies every uploaded row).
    pub async fn insert_or_replace_if_new_by_chunks_commit(
        &self,
        process_id: &str,
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::insert_or_replace_if_new_by_chunks_commit(
            fl_url,
            process_id,
            &self.sync_period,
        )
        .await
    }

    /// Low-level chunked flow: cancels an accumulated process, dropping the uploaded rows.
    pub async fn insert_or_replace_if_new_by_chunks_cancel(
        &self,
        process_id: &str,
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::insert_or_replace_if_new_by_chunks_cancel(fl_url, process_id).await
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct OperationFailHttpContract {
    pub reason: String,
    pub message: String,
}
