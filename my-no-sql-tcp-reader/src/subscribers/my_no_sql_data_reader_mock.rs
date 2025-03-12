use std::{collections::BTreeMap, sync::Arc};

use my_no_sql_abstractions::{MyNoSqlEntity, MyNoSqlEntitySerializer};

use crate::MyNoSqlDataReaderCallBacks;

use super::{GetEntitiesBuilder, GetEntityBuilder, MyNoSqlDataReader, MyNoSqlDataReaderMockInner};

pub struct MyNoSqlDataReaderMock<
    TMyNoSqlEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send + 'static,
> {
    pub inner: Arc<MyNoSqlDataReaderMockInner<TMyNoSqlEntity>>,
}

impl<TMyNoSqlEntity> MyNoSqlDataReaderMock<TMyNoSqlEntity>
where
    TMyNoSqlEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send + 'static,
{
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MyNoSqlDataReaderMockInner::new()),
        }
    }

    pub async fn update(&self, items: impl Iterator<Item = Arc<TMyNoSqlEntity>>) {
        self.inner.update(items).await;
    }
    pub async fn delete(&self, to_delete: impl Iterator<Item = (String, String)>) {
        self.inner.delete(to_delete).await;
    }
}

#[async_trait::async_trait]
impl<TMyNoSqlEntity> MyNoSqlDataReader<TMyNoSqlEntity> for MyNoSqlDataReaderMock<TMyNoSqlEntity>
where
    TMyNoSqlEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send + 'static,
{
    async fn get_table_snapshot_as_vec(&self) -> Option<Vec<Arc<TMyNoSqlEntity>>> {
        let result = self.inner.get_table_snapshot_as_vec().await;

        if result.len() == 0 {
            return None;
        }

        Some(result)
    }

    async fn get_by_partition_key(
        &self,
        partition_key: &str,
    ) -> Option<BTreeMap<String, Arc<TMyNoSqlEntity>>> {
        self.inner.get_by_partition_key(partition_key).await
    }

    async fn get_partition_keys(&self) -> Vec<String> {
        self.inner.get_partition_keys().await
    }

    async fn get_by_partition_key_as_vec(
        &self,
        partition_key: &str,
    ) -> Option<Vec<Arc<TMyNoSqlEntity>>> {
        self.inner.get_by_partition_key_as_vec(partition_key).await
    }

    async fn get_entity(&self, partition_key: &str, row_key: &str) -> Option<Arc<TMyNoSqlEntity>> {
        self.inner.get_entity(partition_key, row_key).await
    }

    fn get_entities<'s>(&self, partition_key: &'s str) -> GetEntitiesBuilder<TMyNoSqlEntity> {
        GetEntitiesBuilder::new_mock(partition_key.to_string(), self.inner.clone())
    }

    fn get_entity_with_callback_to_server<'s>(
        &'s self,
        partition_key: &'s str,
        row_key: &'s str,
    ) -> GetEntityBuilder<'s, TMyNoSqlEntity> {
        GetEntityBuilder::new_mock(partition_key, row_key, self.inner.clone())
    }

    async fn has_partition(&self, partition_key: &str) -> bool {
        self.inner.has_partition(partition_key).await
    }

    async fn wait_until_first_data_arrives(&self) {
        todo!("Not Implemented");
    }

    async fn assign_callback<
        TMyNoSqlDataReaderCallBacks: MyNoSqlDataReaderCallBacks<TMyNoSqlEntity> + Send + Sync + 'static,
    >(
        &self,
        callbacks: Arc<TMyNoSqlDataReaderCallBacks>,
    ) {
        self.inner.assign_callback(callbacks).await
    }
}
