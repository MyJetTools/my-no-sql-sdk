use std::{collections::BTreeMap, marker::PhantomData};

use my_no_sql_abstractions::{DataSynchronizationPeriod, MyNoSqlEntity, MyNoSqlEntitySerializer};

use crate::{DataWriterError, UpdateReadStatistics};

use super::fl_url_factory::FlUrlFactory;

pub struct MyNoSqlDataWriterWithRetries<TEntity: MyNoSqlEntity + Sync + Send> {
    fl_url_factory: FlUrlFactory,
    sync_period: DataSynchronizationPeriod,
    phantom: PhantomData<TEntity>,
    max_attempts: usize,
}

impl<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send>
    MyNoSqlDataWriterWithRetries<TEntity>
{
    pub fn new(
        fl_url_factory: FlUrlFactory,
        sync_period: DataSynchronizationPeriod,
        max_attempts: usize,
    ) -> Self {
        Self {
            phantom: PhantomData,
            sync_period,

            max_attempts,
            fl_url_factory,
        }
    }

    pub async fn insert_entity(&self, entity: &TEntity) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::insert_entity(fl_url, entity, &self.sync_period).await
    }

    pub async fn insert_or_replace_entity(&self, entity: &TEntity) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::insert_or_replace_entity(fl_url, entity, &self.sync_period).await
    }

    /// Optimistic-concurrency replace. See
    /// [`super::MyNoSqlDataWriter::replace_entity`]. The `with_retries` here retries the
    /// underlying HTTP request on transport errors; a 409 `RecordIsChanged` is a real
    /// conflict and is returned to the caller (drive the read-modify-write loop with
    /// [`Self::update_entity`]).
    pub async fn replace_entity(&self, entity: &TEntity) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::replace_entity(fl_url, entity, &self.sync_period).await
    }

    /// Read-modify-write with optimistic concurrency (default attempt limit). See
    /// [`super::MyNoSqlDataWriter::update_entity`]. Each `get`/`replace` in the loop also
    /// goes through the HTTP-level retries of this wrapper.
    pub async fn update_entity<TFn: FnMut(&mut TEntity)>(
        &self,
        partition_key: &str,
        row_key: &str,
        update: TFn,
    ) -> Result<Option<TEntity>, DataWriterError> {
        self.update_entity_with_max_attempts(
            partition_key,
            row_key,
            crate::DEFAULT_UPDATE_ENTITY_MAX_ATTEMPTS,
            update,
        )
        .await
    }

    /// [`Self::update_entity`] with an explicit optimistic-concurrency retry limit.
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
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::bulk_insert_or_replace(fl_url, entities, &self.sync_period).await
    }

    /// Insert-or-replace-if-new for a single entity. The `TimeStamp` is the object's
    /// version and is mandatory — a default/unset `Timestamp` makes the server answer
    /// HTTP 400. See [`super::MyNoSqlDataWriter::insert_or_replace_entity_if_new`].
    pub async fn insert_or_replace_entity_if_new(
        &self,
        entity: &TEntity,
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::insert_or_replace_entity_if_new(fl_url, entity, &self.sync_period).await
    }

    /// Bulk insert-or-replace-if-new. Mandatory-`TimeStamp` contract as above; empty slice
    /// is a no-op. The chunked flow is intentionally not offered on the retries wrapper:
    /// re-sending a chunk after a partial success would double-append rows into the
    /// server-side accumulator, so those requests must not be blindly retried.
    pub async fn bulk_insert_or_replace_if_new(
        &self,
        entities: &[TEntity],
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::bulk_insert_or_replace_if_new(fl_url, entities, &self.sync_period).await
    }

    /// Deletes rows described as PartitionKey -> RowKeys
    pub async fn bulk_delete(
        &self,
        rows_to_delete: &BTreeMap<String, Vec<String>>,
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::bulk_delete::<TEntity>(fl_url, rows_to_delete, &self.sync_period).await
    }

    pub async fn get_entity(
        &self,
        partition_key: &str,
        row_key: &str,
        update_read_statistics: Option<UpdateReadStatistics>,
    ) -> Result<Option<TEntity>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        let fl_url = fl_url.with_retries(self.max_attempts);
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
        let fl_url = fl_url.with_retries(self.max_attempts);
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
        let fl_url = fl_url.with_retries(self.max_attempts);
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
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::get_enum_case_model(fl_url, update_read_statistics.as_ref()).await
    }

    pub async fn get_by_row_key(
        &self,
        row_key: &str,
    ) -> Result<Option<Vec<TEntity>>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::get_by_row_key(fl_url, row_key).await
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
        let fl_url = fl_url.with_retries(self.max_attempts);
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
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::delete_enum_case_with_row_key(fl_url, row_key).await
    }

    pub async fn delete_row(
        &self,
        partition_key: &str,
        row_key: &str,
    ) -> Result<Option<TEntity>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::delete_row(fl_url, partition_key, row_key).await
    }

    pub async fn delete_partitions(&self, partition_keys: &[&str]) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::delete_partitions(fl_url, TEntity::TABLE_NAME, partition_keys).await
    }

    pub async fn get_all(&self) -> Result<Option<Vec<TEntity>>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::get_all(fl_url).await
    }

    pub async fn clean_table_and_bulk_insert(
        &self,
        entities: &[TEntity],
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::clean_table_and_bulk_insert(fl_url, entities, &self.sync_period).await
    }

    pub async fn clean_partition_and_bulk_insert(
        &self,
        partition_key: &str,
        entities: &[TEntity],
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        let fl_url = fl_url.with_retries(self.max_attempts);
        super::execution::clean_partition_and_bulk_insert(
            fl_url,
            partition_key,
            entities,
            &self.sync_period,
        )
        .await
    }

    pub async fn get_partition_keys(
        &self,
        skip: Option<i32>,
        limit: Option<i32>,
    ) -> Result<Vec<String>, DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::get_partition_keys(fl_url, TEntity::TABLE_NAME, skip, limit).await
    }
}
