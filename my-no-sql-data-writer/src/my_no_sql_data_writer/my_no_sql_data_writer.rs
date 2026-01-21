use std::{marker::PhantomData, sync::Arc};

use flurl::FlUrl;

use my_no_sql_abstractions::{DataSynchronizationPeriod, MyNoSqlEntity, MyNoSqlEntitySerializer};

use serde::{Deserialize, Serialize};

use crate::{MyNoSqlDataWriterBuilder, MyNoSqlDataWriterWithRetries, MyNoSqlWriterSettings};

use super::{fl_url_factory::FlUrlFactory, DataWriterError, UpdateReadStatistics};

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
        let settings_cloned = settings.clone();
        tokio::spawn(async move {
            crate::PING_POOL
                .register(settings_cloned, TEntity::TABLE_NAME)
                .await;
        });

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

    #[cfg(feature = "with-ssh")]
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

    pub async fn bulk_insert_or_replace(
        &self,
        entities: &[TEntity],
    ) -> Result<(), DataWriterError> {
        let (fl_url, _) = self.fl_url_factory.get_fl_url().await?;
        super::execution::bulk_insert_or_replace(fl_url, entities, &self.sync_period).await
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
}

#[derive(Serialize, Deserialize, Debug)]
pub struct OperationFailHttpContract {
    pub reason: String,
    pub message: String,
}
