use flurl::{body::FlUrlBody, FlUrl, FlUrlResponse};
use my_json::{
    json_reader::JsonArrayIterator,
    json_writer::{JsonArrayWriter, RawJsonObject},
};
use my_logger::LogEventCtx;
use my_no_sql_abstractions::{DataSynchronizationPeriod, MyNoSqlEntity, MyNoSqlEntitySerializer};
use serde::{Deserialize, Serialize};

use crate::{CreateTableParams, DataWriterError, OperationFailHttpContract, UpdateReadStatistics};

use super::fl_url_ext::FlUrlExt;

const API_SEGMENT: &str = "api";

const ROW_CONTROLLER: &str = "Row";
const ROWS_CONTROLLER: &str = "Rows";
const BULK_CONTROLLER: &str = "Bulk";
const PARTITIONS_CONTROLLER: &str = "Partitions";

pub async fn create_table_if_not_exists(
    flurl: FlUrl,
    url: &str,
    table_name: &'static str,
    params: &CreateTableParams,
    sync_period: DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    let fl_url = flurl
        .append_path_segment("Tables")
        .append_path_segment("CreateIfNotExists")
        .append_data_sync_period(&sync_period)
        .with_table_name_as_query_param(table_name);

    let fl_url = params.populate_params(fl_url);

    let mut response = fl_url.post(FlUrlBody::Empty).await?;

    create_table_errors_handler(&mut response, "create_table_if_not_exists", url).await
}

pub async fn create_table(
    flurl: FlUrl,
    url: &str,
    table_name: &str,
    params: CreateTableParams,
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    let fl_url = flurl
        .append_path_segment("Tables")
        .append_path_segment("Create")
        .with_table_name_as_query_param(table_name)
        .append_data_sync_period(sync_period);

    let fl_url = params.populate_params(fl_url);

    let mut response = fl_url.post(FlUrlBody::Empty).await?;

    create_table_errors_handler(&mut response, "create_table", url).await
}

pub async fn insert_entity<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send>(
    flurl: FlUrl,
    entity: &TEntity,
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    let response = flurl
        .append_path_segment(ROW_CONTROLLER)
        .append_path_segment("Insert")
        .append_data_sync_period(sync_period)
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .post(FlUrlBody::Json(entity.serialize_entity()))
        .await?;

    if is_ok_result(&response) {
        return Ok(());
    }

    let reason = response.receive_body().await?;
    let reason = String::from_utf8(reason)?;
    return Err(DataWriterError::Error(reason));
}

pub async fn insert_or_replace_entity<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
>(
    flurl: FlUrl,
    entity: &TEntity,
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    let entity = entity.serialize_entity();

    let response = flurl
        .append_path_segment(ROW_CONTROLLER)
        .append_path_segment("InsertOrReplace")
        .append_data_sync_period(sync_period)
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .post(FlUrlBody::Json(entity))
        .await?;

    if is_ok_result(&response) {
        return Ok(());
    }

    let body = response.receive_body().await?;
    let body = String::from_utf8(body)?;

    return Err(DataWriterError::Error(body));
}

pub async fn bulk_insert_or_replace<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
>(
    flurl: FlUrl,
    entities: &[TEntity],
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    if entities.is_empty() {
        return Ok(());
    }

    let response = flurl
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("InsertOrReplace")
        .append_data_sync_period(sync_period)
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .post(serialize_entities_to_body(entities))
        .await?;

    if is_ok_result(&response) {
        return Ok(());
    }

    let reason = response.receive_body().await?;
    let reason = String::from_utf8(reason)?;
    return Err(DataWriterError::Error(reason));
}

pub async fn get_entity<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send>(
    flurl: FlUrl,
    partition_key: &str,
    row_key: &str,
    update_read_statistics: Option<&UpdateReadStatistics>,
) -> Result<Option<TEntity>, DataWriterError> {
    let mut request = flurl
        .append_path_segment(ROW_CONTROLLER)
        .with_partition_key_as_query_param(partition_key)
        .with_row_key_as_query_param(row_key)
        .with_table_name_as_query_param(TEntity::TABLE_NAME);

    if let Some(update_read_statistics) = update_read_statistics {
        request = update_read_statistics.fill_fields(request);
    }

    let mut response = request.get().await?;

    if response.get_status_code() == 404 {
        return Ok(None);
    }

    check_error(&mut response).await?;

    if is_ok_result(&response) {
        let entity = TEntity::deserialize_entity(response.get_body_as_slice().await?).unwrap();
        return Ok(Some(entity));
    }

    return Ok(None);
}

pub async fn get_by_partition_key<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
>(
    flurl: FlUrl,
    partition_key: &str,
    update_read_statistics: Option<&UpdateReadStatistics>,
) -> Result<Option<Vec<TEntity>>, DataWriterError> {
    let mut request = flurl
        .append_path_segment(ROW_CONTROLLER)
        .with_partition_key_as_query_param(partition_key)
        .with_table_name_as_query_param(TEntity::TABLE_NAME);

    if let Some(update_read_statistics) = update_read_statistics {
        request = update_read_statistics.fill_fields(request);
    }

    let mut response = request.get().await?;

    if response.get_status_code() == 404 {
        return Ok(None);
    }

    check_error(&mut response).await?;

    if is_ok_result(&response) {
        let entities = deserialize_entities(response.get_body_as_slice().await?)?;
        return Ok(Some(entities));
    }

    return Ok(None);
}

pub async fn get_enum_case_models_by_partition_key<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
    TResult: MyNoSqlEntity
        + my_no_sql_abstractions::GetMyNoSqlEntitiesByPartitionKey
        + From<TEntity>
        + Sync
        + Send
        + 'static,
>(
    flurl: FlUrl,
    update_read_statistics: Option<&UpdateReadStatistics>,
) -> Result<Option<Vec<TResult>>, DataWriterError> {
    let result: Option<Vec<TEntity>> =
        get_by_partition_key(flurl, TResult::PARTITION_KEY, update_read_statistics).await?;

    match result {
        Some(entities) => {
            let mut result = Vec::with_capacity(entities.len());

            for entity in entities {
                result.push(entity.into());
            }

            Ok(Some(result))
        }
        None => Ok(None),
    }
}

pub async fn get_enum_case_model<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
    TResult: MyNoSqlEntity
        + From<TEntity>
        + my_no_sql_abstractions::GetMyNoSqlEntity
        + Sync
        + Send
        + 'static,
>(
    flurl: FlUrl,
    update_read_statistics: Option<&UpdateReadStatistics>,
) -> Result<Option<TResult>, DataWriterError> {
    let entity: Option<TEntity> = get_entity(
        flurl,
        TResult::PARTITION_KEY,
        TResult::ROW_KEY,
        update_read_statistics,
    )
    .await?;

    match entity {
        Some(entity) => Ok(Some(entity.into())),
        None => Ok(None),
    }
}

pub async fn get_by_row_key<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send>(
    flurl: FlUrl,
    row_key: &str,
) -> Result<Option<Vec<TEntity>>, DataWriterError> {
    let mut response = flurl
        .append_path_segment(API_SEGMENT)
        .append_path_segment(ROW_CONTROLLER)
        .with_row_key_as_query_param(row_key)
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .get()
        .await?;

    if response.get_status_code() == 404 {
        return Ok(None);
    }

    check_error(&mut response).await?;

    if is_ok_result(&response) {
        let entities = deserialize_entities(response.get_body_as_slice().await?)?;
        return Ok(Some(entities));
    }

    return Ok(None);
}

pub async fn get_partition_keys(
    flurl: FlUrl,
    table_name: &str,
    skip: Option<i32>,
    limit: Option<i32>,
) -> Result<Vec<String>, DataWriterError> {
    #[derive(Serialize, Deserialize)]
    pub struct GetPartitionsJsonResult {
        pub amount: usize,
        pub data: Vec<String>,
    }
    let mut response = flurl
        .append_path_segment(API_SEGMENT)
        .append_path_segment(PARTITIONS_CONTROLLER)
        .with_table_name_as_query_param(table_name)
        .with_skip_as_query_param(skip)
        .with_limit_as_query_param(limit)
        .get()
        .await?;

    if response.get_status_code() == 404 {
        return Err(DataWriterError::TableNotFound(table_name.to_string()));
    }

    check_error(&mut response).await?;

    if is_ok_result(&response) {
        let result: Result<GetPartitionsJsonResult, _> =
            serde_json::from_slice(response.get_body_as_slice().await?);
        match result {
            Ok(result) => return Ok(result.data),
            Err(err) => {
                return Err(DataWriterError::Error(format!(
                    "Failed to deserialize: {:?}",
                    err
                )))
            }
        }
    }

    return Ok(vec![]);
}

pub async fn delete_enum_case<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
    TResult: MyNoSqlEntity
        + From<TEntity>
        + my_no_sql_abstractions::GetMyNoSqlEntity
        + Sync
        + Send
        + 'static,
>(
    flurl: FlUrl,
) -> Result<Option<TResult>, DataWriterError> {
    let entity: Option<TEntity> =
        delete_row(flurl, TResult::PARTITION_KEY, TResult::ROW_KEY).await?;

    match entity {
        Some(entity) => Ok(Some(entity.into())),
        None => Ok(None),
    }
}

pub async fn delete_enum_case_with_row_key<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
    TResult: MyNoSqlEntity
        + From<TEntity>
        + my_no_sql_abstractions::GetMyNoSqlEntitiesByPartitionKey
        + Sync
        + Send
        + 'static,
>(
    flurl: FlUrl,
    row_key: &str,
) -> Result<Option<TResult>, DataWriterError> {
    let entity: Option<TEntity> = delete_row(flurl, TResult::PARTITION_KEY, row_key).await?;

    match entity {
        Some(entity) => Ok(Some(entity.into())),
        None => Ok(None),
    }
}

pub async fn delete_row<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send>(
    flurl: FlUrl,
    partition_key: &str,
    row_key: &str,
) -> Result<Option<TEntity>, DataWriterError> {
    let mut response = flurl
        .append_path_segment(API_SEGMENT)
        .append_path_segment(ROW_CONTROLLER)
        .with_partition_key_as_query_param(partition_key)
        .with_row_key_as_query_param(row_key)
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .delete()
        .await?;

    if response.get_status_code() == 404 {
        return Ok(None);
    }

    check_error(&mut response).await?;

    if response.get_status_code() == 200 {
        let entity = TEntity::deserialize_entity(response.get_body_as_slice().await?).unwrap();
        return Ok(Some(entity));
    }

    return Ok(None);
}

pub async fn delete_partitions(
    flurl: FlUrl,
    table_name: &str,
    partition_keys: &[&str],
) -> Result<(), DataWriterError> {
    let mut response = flurl
        .append_path_segment(ROWS_CONTROLLER)
        .with_table_name_as_query_param(table_name)
        .with_partition_keys_as_query_param(partition_keys)
        .delete()
        .await?;

    if response.get_status_code() == 404 {
        return Ok(());
    }

    check_error(&mut response).await?;

    return Ok(());
}

pub async fn get_all<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send>(
    flurl: FlUrl,
) -> Result<Option<Vec<TEntity>>, DataWriterError> {
    let mut response = flurl
        .append_path_segment(ROW_CONTROLLER)
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .get()
        .await?;

    if response.get_status_code() == 404 {
        return Ok(None);
    }

    check_error(&mut response).await?;

    if is_ok_result(&response) {
        let entities = deserialize_entities(response.get_body_as_slice().await?)?;
        return Ok(Some(entities));
    }

    return Ok(None);
}

pub async fn clean_table_and_bulk_insert<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
>(
    flurl: FlUrl,
    entities: &[TEntity],
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    let mut response = flurl
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("CleanAndBulkInsert")
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .append_data_sync_period(sync_period)
        .post(serialize_entities_to_body(entities))
        .await?;

    check_error(&mut response).await?;

    return Ok(());
}

pub async fn clean_partition_and_bulk_insert<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
>(
    flurl: FlUrl,
    partition_key: &str,
    entities: &[TEntity],
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    let mut response = flurl
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("CleanAndBulkInsert")
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .append_data_sync_period(sync_period)
        .with_partition_key_as_query_param(partition_key)
        .post(serialize_entities_to_body(entities))
        .await?;

    check_error(&mut response).await?;

    return Ok(());
}

fn is_ok_result(response: &FlUrlResponse) -> bool {
    response.get_status_code() >= 200 && response.get_status_code() < 300
}

fn serialize_entities_to_body<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer>(
    entities: &[TEntity],
) -> FlUrlBody {
    if entities.len() == 0 {
        FlUrlBody::Json(vec![b'[', b']']);
    }

    let mut json_array_writer = JsonArrayWriter::new();

    for entity in entities {
        let payload = entity.serialize_entity();
        let payload: RawJsonObject = payload.into();
        json_array_writer.write(payload);
    }

    FlUrlBody::Json(json_array_writer.build().into_bytes())
}

async fn check_error(response: &mut FlUrlResponse) -> Result<(), DataWriterError> {
    let result = match response.get_status_code() {
        400 => Err(deserialize_error(response).await?),

        409 => Err(DataWriterError::TableNotFound("".to_string())),
        _ => Ok(()),
    };

    if let Err(err) = &result {
        my_logger::LOGGER.write_error(
            format!("FlUrlRequest to {}", response.url.to_string()),
            format!("{:?}", err),
            None.into(),
        );
    }

    result
}

async fn deserialize_error(
    response: &mut FlUrlResponse,
) -> Result<DataWriterError, DataWriterError> {
    let body = response.get_body_as_slice().await?;

    let body_as_str = std::str::from_utf8(body)?;

    let result = match serde_json::from_str::<OperationFailHttpContract>(body_as_str) {
        Ok(fail_contract) => match fail_contract.reason.as_str() {
            "TableAlreadyExists" => DataWriterError::TableAlreadyExists(fail_contract.message),
            "TableNotFound" => DataWriterError::TableNotFound(fail_contract.message),
            "RecordAlreadyExists" => DataWriterError::RecordAlreadyExists(fail_contract.message),
            "RequiredEntityFieldIsMissing" => {
                DataWriterError::RequiredEntityFieldIsMissing(fail_contract.message)
            }
            "JsonParseFail" => DataWriterError::ServerCouldNotParseJson(fail_contract.message),
            _ => DataWriterError::Error(format!("Not supported error. {:?}", fail_contract)),
        },
        Err(err) => {
            return Err(DataWriterError::Error(format!(
                "Failed to deserialize error: {:?}",
                err
            )))
        }
    };

    Ok(result)
}

fn deserialize_entities<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer>(
    src: &[u8],
) -> Result<Vec<TEntity>, DataWriterError> {
    let mut result = Vec::new();

    let json_array_iterator = JsonArrayIterator::new(src);

    if let Err(err) = &json_array_iterator {
        panic!(
            "Can not deserialize entities for table: {}. Err: {:?}",
            TEntity::TABLE_NAME,
            err
        );
    }

    let json_array_iterator = json_array_iterator.unwrap();

    while let Some(item) = json_array_iterator.get_next() {
        let itm = item.unwrap();

        match TEntity::deserialize_entity(itm.as_bytes()) {
            Ok(entity) => {
                result.push(entity);
            }
            Err(err) => {
                println!(
                    "Table: '{}', Entity: {:?}",
                    TEntity::TABLE_NAME,
                    std::str::from_utf8(itm.as_bytes())
                );
                panic!("Can not deserialize entity: {}", err);
            }
        }
    }
    Ok(result)

    /*
    let mut result = Vec::new();



    for itm in JsonArrayIterator::new(src) {
        let itm = itm.unwrap();

        result.push(TEntity::deserialize_entity(itm).unwrap());
    }
    Ok(result)
     */
}

async fn create_table_errors_handler(
    response: &mut FlUrlResponse,
    process_name: &'static str,
    url: &str,
) -> Result<(), DataWriterError> {
    if is_ok_result(response) {
        return Ok(());
    }

    let result = deserialize_error(response).await?;

    my_logger::LOGGER.write_error(
        process_name,
        format!("{:?}", result),
        LogEventCtx::new().add("URL", url),
    );

    Err(result)
}

#[cfg(test)]
mod tests {
    use my_no_sql_abstractions::{MyNoSqlEntity, MyNoSqlEntitySerializer, Timestamp};
    use serde::Serialize;
    use serde_derive::Deserialize;

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct TestEntity {
        partition_key: String,
        row_key: String,
    }

    impl MyNoSqlEntity for TestEntity {
        const TABLE_NAME: &'static str = "test";
        const LAZY_DESERIALIZATION: bool = false;

        fn get_partition_key(&self) -> &str {
            &self.partition_key
        }

        fn get_row_key(&self) -> &str {
            &self.row_key
        }

        fn get_time_stamp(&self) -> Timestamp {
            Timestamp::default()
        }
    }

    impl MyNoSqlEntitySerializer for TestEntity {
        fn serialize_entity(&self) -> Vec<u8> {
            my_no_sql_core::entity_serializer::serialize(self)
        }

        fn deserialize_entity(src: &[u8]) -> Result<Self, String> {
            my_no_sql_core::entity_serializer::deserialize(src)
        }
    }

    #[test]
    fn test() {
        let entities = vec![
            TestEntity {
                partition_key: "1".to_string(),
                row_key: "1".to_string(),
            },
            TestEntity {
                partition_key: "1".to_string(),
                row_key: "2".to_string(),
            },
            TestEntity {
                partition_key: "2".to_string(),
                row_key: "1".to_string(),
            },
            TestEntity {
                partition_key: "2".to_string(),
                row_key: "2".to_string(),
            },
        ];

        let as_json = super::serialize_entities_to_body(&entities);

        println!("{}", std::str::from_utf8(as_json.as_slice()).unwrap());
    }
}
