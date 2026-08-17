use flurl::{body::HttpRequestBody, FlUrl, FlUrlResponse};
use my_json::{
    json_reader::JsonArrayIterator,
    json_writer::{JsonArrayWriter, RawJsonObject},
};
use my_logger::LogEventCtx;
use my_no_sql_abstractions::{
    DataSynchronizationPeriod, MyNoSqlEntity, MyNoSqlEntitySerializer, Timestamp,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{CreateTableParams, DataWriterError, OperationFailHttpContract, UpdateReadStatistics};

use super::delete_if::{
    deserialize_bulk_delete_if_result, serialize_rows_to_delete_if, BulkDeleteIfResult,
    RowToDeleteIf,
};
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

    let mut response = fl_url.post(HttpRequestBody::Empty).await?;

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

    let mut response = fl_url.post(HttpRequestBody::Empty).await?;

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
        .post(HttpRequestBody::Json(entity.serialize_entity()))
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
        .post(HttpRequestBody::Json(entity))
        .await?;

    if is_ok_result(&response) {
        return Ok(());
    }

    let body = response.receive_body().await?;
    let body = String::from_utf8(body)?;

    return Err(DataWriterError::Error(body));
}

/// PUT /api/Row/Replace — optimistic-concurrency replace. The entity must carry the
/// `TimeStamp` it was read with; the server compares it to the stored row's `TimeStamp`:
/// equal → replaced (200); different → 409 [`DataWriterError::RecordIsChanged`]; row
/// missing → 404 [`DataWriterError::RecordNotFound`]; no `TimeStamp` → 400. Use it as
/// read-version → mutate → write-with-that-version; on a conflict re-read and retry (see
/// `MyNoSqlDataWriter::update_entity`).
pub async fn replace_entity<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send>(
    flurl: FlUrl,
    entity: &TEntity,
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    let mut response = flurl
        .append_path_segment(ROW_CONTROLLER)
        .append_path_segment("Replace")
        .append_data_sync_period(sync_period)
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .put(HttpRequestBody::Json(entity.serialize_entity()))
        .await?;

    if response.get_status_code() == 404 {
        let body = response.receive_body().await?;
        let message = String::from_utf8(body)
            .unwrap_or_else(|_| "Record not found".to_string());
        return Err(DataWriterError::RecordNotFound(message));
    }

    // Handles 400 (deserialize_error) and 409 (RecordIsChanged).
    check_error(&mut response).await?;

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

/// POST /api/Bulk/InsertOrReplace with `useTimestamp=true` — bulk insert-or-replace that
/// KEEPS each entity's own `TimeStamp` instead of letting the server stamp its own clock.
///
/// Unlike [`bulk_insert_or_replace_if_new`], this is an **unconditional** replace (no
/// "strictly greater" check): every row is written, but the stored row carries the
/// client-supplied `TimeStamp`. Because of that the `TimeStamp` is **mandatory** — a
/// default/unset one serializes to `null`/omitted and the server answers **HTTP 400**.
/// An empty slice is a no-op.
pub async fn bulk_insert_or_update_with_own_timestamp<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
>(
    flurl: FlUrl,
    entities: &[TEntity],
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    if entities.is_empty() {
        return Ok(());
    }

    debug_assert!(
        entities.iter().all(|e| !e.get_time_stamp().is_default()),
        "bulk_insert_or_update_with_own_timestamp requires every entity to carry its own \
         (non-default) TimeStamp; a default Timestamp serializes to null/omitted and the \
         server rejects it with HTTP 400"
    );

    let response = flurl
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("InsertOrReplace")
        .append_data_sync_period(sync_period)
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .append_query_param("useTimestamp", Some("true"))
        .post(serialize_entities_to_body(entities))
        .await?;

    if is_ok_result(&response) {
        return Ok(());
    }

    let reason = response.receive_body().await?;
    let reason = String::from_utf8(reason)?;
    return Err(DataWriterError::Error(reason));
}

/// Deletes rows described as PartitionKey -> RowKeys
pub async fn bulk_delete<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send>(
    flurl: FlUrl,
    rows_to_delete: &BTreeMap<String, Vec<String>>,
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    if rows_to_delete.is_empty() {
        return Ok(());
    }

    let body = match serde_json::to_vec(rows_to_delete) {
        Ok(body) => body,
        Err(err) => {
            return Err(DataWriterError::Error(format!(
                "Failed to serialize rows to delete: {:?}",
                err
            )))
        }
    };

    let response = flurl
        .append_path_segment(API_SEGMENT)
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("Delete")
        .append_data_sync_period(sync_period)
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .post(HttpRequestBody::Json(body))
        .await?;

    if is_ok_result(&response) {
        return Ok(());
    }

    let reason = response.receive_body().await?;
    let reason = String::from_utf8(reason)?;
    return Err(DataWriterError::Error(reason));
}

/// POST /api/Bulk/DeleteIf — optimistic-concurrency delete of a whole batch. Each entity's
/// own `TimeStamp` is the version to match, so pass the entities exactly as they were read.
///
/// The answer is always a **partial success** (HTTP 200): rows which are still at the
/// version sent are deleted, every other one stays in the table and comes back in
/// [`BulkDeleteIfResult::skipped`] with the reason - `TimeStampMismatch` (rewritten
/// meanwhile) or `NotFound` (no such row). A conflict here is data, not an error - unlike
/// [`delete_row_if`], which answers 409 for the single row it addresses.
///
/// Every entity must carry a real (non-default) `TimeStamp`: an unreadable version could
/// never match a stored one, so the server refuses the **whole batch** with HTTP 400 instead
/// of reporting that row as skipped. An empty slice is a no-op (no request at all), like
/// [`bulk_delete`].
pub async fn bulk_delete_if<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send>(
    flurl: FlUrl,
    entities: &[&TEntity],
    sync_period: &DataSynchronizationPeriod,
) -> Result<BulkDeleteIfResult, DataWriterError> {
    if entities.is_empty() {
        return Ok(BulkDeleteIfResult::nothing_to_delete());
    }

    debug_assert!(
        entities.iter().all(|e| !e.get_time_stamp().is_default()),
        "bulk_delete_if compares against the version each row was read at; a default \
         Timestamp is not a version the server can read and it rejects the whole batch \
         with HTTP 400"
    );

    let body = serialize_rows_to_delete_if(
        entities
            .iter()
            .map(|e| (e.get_partition_key(), e.get_row_key(), e.get_time_stamp())),
    )?;

    send_bulk_delete_if::<TEntity>(flurl, body, sync_period).await
}

/// [`bulk_delete_if`] taking the keys and versions on their own instead of whole entities —
/// for when the versions come from somewhere else than the entities (a projection, a change
/// log, another service). Same contract in every other respect.
pub async fn bulk_delete_if_rows<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send>(
    flurl: FlUrl,
    rows: &[RowToDeleteIf],
    sync_period: &DataSynchronizationPeriod,
) -> Result<BulkDeleteIfResult, DataWriterError> {
    if rows.is_empty() {
        return Ok(BulkDeleteIfResult::nothing_to_delete());
    }

    debug_assert!(
        rows.iter().all(|row| !row.time_stamp.is_default()),
        "bulk_delete_if_rows compares against the version each row was read at; a default \
         Timestamp is not a version the server can read and it rejects the whole batch \
         with HTTP 400"
    );

    let body = serialize_rows_to_delete_if(rows.iter().map(|row| {
        (
            row.partition_key.as_str(),
            row.row_key.as_str(),
            row.time_stamp,
        )
    }))?;

    send_bulk_delete_if::<TEntity>(flurl, body, sync_period).await
}

async fn send_bulk_delete_if<TEntity: MyNoSqlEntity>(
    flurl: FlUrl,
    body: Vec<u8>,
    sync_period: &DataSynchronizationPeriod,
) -> Result<BulkDeleteIfResult, DataWriterError> {
    let mut response = flurl
        .append_path_segment(API_SEGMENT)
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("DeleteIf")
        .append_data_sync_period(sync_period)
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .post(HttpRequestBody::Json(body))
        .await?;

    // 400 - the table is not there, or a row of the batch carries no readable TimeStamp.
    check_error(&mut response).await?;

    if !is_ok_result(&response) {
        let reason = response.receive_body().await?;
        let reason = String::from_utf8(reason)?;
        return Err(DataWriterError::Error(reason));
    }

    deserialize_bulk_delete_if_result(response.get_body_as_slice().await?)
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

/// DELETE /api/Row/DeleteIf — optimistic-concurrency delete: the row goes away only while
/// the `TimeStamp` stored in the table is still `time_stamp`, and the deleted row comes back
/// in the body.
///
/// Same codes as [`replace_entity`], mapped the way [`delete_row`] maps them: 200 → the row
/// was deleted → `Ok(Some(entity))`; 404 → there is no such row → `Ok(None)`; 409 → the row
/// is there but at another version, somebody rewrote it between the read and this call →
/// [`DataWriterError::RecordIsChanged`]; 400 → the table is missing, or `time_stamp` is not
/// a readable `TimeStamp`.
///
/// Use it as read → decide → delete exactly the version that was read. On a conflict re-read
/// and decide again — the row may no longer be one you want to delete.
pub async fn delete_row_if<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send>(
    flurl: FlUrl,
    partition_key: &str,
    row_key: &str,
    time_stamp: Timestamp,
    sync_period: &DataSynchronizationPeriod,
) -> Result<Option<TEntity>, DataWriterError> {
    debug_assert!(
        !time_stamp.is_default(),
        "DeleteIf matches against the version the row was read at; a default Timestamp is \
         not a version the server can read and it answers HTTP 400"
    );

    let mut response = flurl
        .append_path_segment(API_SEGMENT)
        .append_path_segment(ROW_CONTROLLER)
        .append_path_segment("DeleteIf")
        .append_data_sync_period(sync_period)
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .with_partition_key_as_query_param(partition_key)
        .with_row_key_as_query_param(row_key)
        .append_query_param("timeStamp", Some(time_stamp.to_string()))
        .delete()
        .await?;

    if response.get_status_code() == 404 {
        return Ok(None);
    }

    // Handles 400 (deserialize_error) and 409 (RecordIsChanged).
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

/// [`clean_table_and_bulk_insert`] with `useTimestamp=true`: the whole table is cleaned and
/// re-inserted, but each row keeps its **own `TimeStamp`** instead of the server clock.
/// Every entity must carry a real (non-default) `TimeStamp`, otherwise the server rejects
/// the request with HTTP 400. (An empty slice still cleans the table — nothing to insert.)
pub async fn clean_table_and_bulk_insert_with_own_timestamp<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
>(
    flurl: FlUrl,
    entities: &[TEntity],
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    debug_assert!(
        entities.iter().all(|e| !e.get_time_stamp().is_default()),
        "clean_table_and_bulk_insert_with_own_timestamp requires every entity to carry its \
         own (non-default) TimeStamp; a default one → HTTP 400"
    );

    let mut response = flurl
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("CleanAndBulkInsert")
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .append_data_sync_period(sync_period)
        .append_query_param("useTimestamp", Some("true"))
        .post(serialize_entities_to_body(entities))
        .await?;

    check_error(&mut response).await?;

    return Ok(());
}

/// [`clean_partition_and_bulk_insert`] with `useTimestamp=true`: the partition is cleaned and
/// re-inserted, but each row keeps its **own `TimeStamp`**. Every entity must carry a real
/// (non-default) `TimeStamp`, otherwise the server rejects the request with HTTP 400.
pub async fn clean_partition_and_bulk_insert_with_own_timestamp<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
>(
    flurl: FlUrl,
    partition_key: &str,
    entities: &[TEntity],
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    debug_assert!(
        entities.iter().all(|e| !e.get_time_stamp().is_default()),
        "clean_partition_and_bulk_insert_with_own_timestamp requires every entity to carry its \
         own (non-default) TimeStamp; a default one → HTTP 400"
    );

    let mut response = flurl
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("CleanAndBulkInsert")
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .append_data_sync_period(sync_period)
        .with_partition_key_as_query_param(partition_key)
        .append_query_param("useTimestamp", Some("true"))
        .post(serialize_entities_to_body(entities))
        .await?;

    check_error(&mut response).await?;

    return Ok(());
}

/// POST /api/Row/InsertOrReplaceIfNew — inserts a missing row, or replaces the stored
/// one only when the incoming `TimeStamp` is strictly greater.
///
/// Unlike every other write, the server does NOT stamp its own time here: the client's
/// `TimeStamp` is the object's version and is mandatory. A default (unset) `Timestamp`
/// serializes to `null` / is omitted, and the server answers HTTP 400. The caller must
/// set `entity.time_stamp` to a real value (e.g. `DateTimeAsMicroseconds::now().into()`).
pub async fn insert_or_replace_entity_if_new<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
>(
    flurl: FlUrl,
    entity: &TEntity,
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    debug_assert!(
        !entity.get_time_stamp().is_default(),
        "InsertOrReplaceIfNew requires the entity to carry its own (non-default) TimeStamp; \
         a default Timestamp serializes to null/omitted and the server rejects it with HTTP 400"
    );

    let response = flurl
        .append_path_segment(API_SEGMENT)
        .append_path_segment(ROW_CONTROLLER)
        .append_path_segment("InsertOrReplaceIfNew")
        .append_data_sync_period(sync_period)
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .post(HttpRequestBody::Json(entity.serialize_entity()))
        .await?;

    if is_ok_result(&response) {
        return Ok(());
    }

    let reason = response.receive_body().await?;
    let reason = String::from_utf8(reason)?;
    return Err(DataWriterError::Error(reason));
}

/// POST /api/Bulk/InsertOrReplaceIfNew — same rule as [`insert_or_replace_entity_if_new`]
/// applied per row. Every entity must carry its own non-default `TimeStamp`. An empty
/// slice is a no-op (early `Ok(())`, like `bulk_insert_or_replace`).
pub async fn bulk_insert_or_replace_if_new<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
>(
    flurl: FlUrl,
    entities: &[TEntity],
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    if entities.is_empty() {
        return Ok(());
    }

    debug_assert!(
        entities.iter().all(|e| !e.get_time_stamp().is_default()),
        "InsertOrReplaceIfNew requires every entity to carry its own (non-default) TimeStamp; \
         a default Timestamp serializes to null/omitted and the server rejects it with HTTP 400"
    );

    let response = flurl
        .append_path_segment(API_SEGMENT)
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("InsertOrReplaceIfNew")
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

/// POST /api/Bulk/InsertOrReplaceIfNewByChunks — uploads one chunk of rows aside from the
/// table. Pass `process_id = None` to start a new process (the server issues an id and
/// returns it in `{ "processId": ".." }`); pass the previously issued id to append a
/// further chunk. Either way the (echoed) process id is returned. Nothing is applied
/// until the commit. Each row must carry its own non-default `TimeStamp`.
pub async fn insert_or_replace_if_new_by_chunks_upload<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
>(
    flurl: FlUrl,
    entities: &[TEntity],
    process_id: Option<&str>,
) -> Result<String, DataWriterError> {
    debug_assert!(
        entities.iter().all(|e| !e.get_time_stamp().is_default()),
        "InsertOrReplaceIfNew requires every entity to carry its own (non-default) TimeStamp; \
         a default Timestamp serializes to null/omitted and the server rejects the commit"
    );

    let mut request = flurl
        .append_path_segment(API_SEGMENT)
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("InsertOrReplaceIfNewByChunks")
        .with_table_name_as_query_param(TEntity::TABLE_NAME);

    if let Some(process_id) = process_id {
        request = request.append_query_param("processId", Some(process_id));
    }

    let mut response = request.post(serialize_entities_to_body(entities)).await?;

    if !is_ok_result(&response) {
        let reason = response.receive_body().await?;
        let reason = String::from_utf8(reason)?;
        return Err(DataWriterError::Error(reason));
    }

    let body = response.get_body_as_slice().await?;

    let contract: BulkProcessResponseContract =
        serde_json::from_slice(body).map_err(|err| {
            DataWriterError::Error(format!(
                "Failed to deserialize BulkProcessResponse: {:?}",
                err
            ))
        })?;

    Ok(contract.process_id)
}

/// POST /api/Bulk/InsertOrReplaceIfNewByChunksCommit — applies all accumulated chunks in
/// one operation (insert when missing, replace only when the row's `TimeStamp` is greater).
pub async fn insert_or_replace_if_new_by_chunks_commit(
    flurl: FlUrl,
    process_id: &str,
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    let response = flurl
        .append_path_segment(API_SEGMENT)
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("InsertOrReplaceIfNewByChunksCommit")
        .append_query_param("processId", Some(process_id))
        .append_data_sync_period(sync_period)
        .post(HttpRequestBody::Empty)
        .await?;

    if is_ok_result(&response) {
        return Ok(());
    }

    let reason = response.receive_body().await?;
    let reason = String::from_utf8(reason)?;
    return Err(DataWriterError::Error(reason));
}

/// POST /api/Bulk/InsertOrReplaceIfNewByChunksCancel — drops the accumulated chunks. The
/// table is not touched.
pub async fn insert_or_replace_if_new_by_chunks_cancel(
    flurl: FlUrl,
    process_id: &str,
) -> Result<(), DataWriterError> {
    let response = flurl
        .append_path_segment(API_SEGMENT)
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("InsertOrReplaceIfNewByChunksCancel")
        .append_query_param("processId", Some(process_id))
        .post(HttpRequestBody::Empty)
        .await?;

    if is_ok_result(&response) {
        return Ok(());
    }

    let reason = response.receive_body().await?;
    let reason = String::from_utf8(reason)?;
    return Err(DataWriterError::Error(reason));
}

/// POST /api/Bulk/CleanAndBulkInsertByChunks with `useTimestamp=true` — uploads one chunk of
/// a clean-and-bulk-insert process that keeps each row's own `TimeStamp`. `process_id = None`
/// starts a new process (the server issues the id and returns it); `partition_key` is honored
/// only on that first (start) chunk and scopes the clean to that partition (`None` = whole
/// table). Pass the issued id on every following chunk. Nothing is applied until the commit.
/// Each row must carry a non-default `TimeStamp`.
pub async fn clean_and_bulk_insert_by_chunks_with_own_timestamp_upload<
    TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer + Sync + Send,
>(
    flurl: FlUrl,
    entities: &[TEntity],
    partition_key: Option<&str>,
    process_id: Option<&str>,
) -> Result<String, DataWriterError> {
    debug_assert!(
        entities.iter().all(|e| !e.get_time_stamp().is_default()),
        "clean_and_bulk_insert_by_chunks_with_own_timestamp requires every entity to carry its \
         own (non-default) TimeStamp; a default one → the chunk is rejected with HTTP 400"
    );

    let mut request = flurl
        .append_path_segment(API_SEGMENT)
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("CleanAndBulkInsertByChunks")
        .with_table_name_as_query_param(TEntity::TABLE_NAME)
        .append_query_param("useTimestamp", Some("true"));

    // partitionKey is taken into account only when a new process is started.
    if let Some(partition_key) = partition_key {
        request = request.with_partition_key_as_query_param(partition_key);
    }

    if let Some(process_id) = process_id {
        request = request.append_query_param("processId", Some(process_id));
    }

    let mut response = request.post(serialize_entities_to_body(entities)).await?;

    if !is_ok_result(&response) {
        let reason = response.receive_body().await?;
        let reason = String::from_utf8(reason)?;
        return Err(DataWriterError::Error(reason));
    }

    let body = response.get_body_as_slice().await?;

    let contract: BulkProcessResponseContract = serde_json::from_slice(body).map_err(|err| {
        DataWriterError::Error(format!(
            "Failed to deserialize BulkProcessResponse: {:?}",
            err
        ))
    })?;

    Ok(contract.process_id)
}

/// POST /api/Bulk/CleanAndBulkInsertByChunksCommit — cleans the table (or the partition the
/// process was started with) and inserts every accumulated row atomically.
pub async fn clean_and_bulk_insert_by_chunks_commit(
    flurl: FlUrl,
    process_id: &str,
    sync_period: &DataSynchronizationPeriod,
) -> Result<(), DataWriterError> {
    let response = flurl
        .append_path_segment(API_SEGMENT)
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("CleanAndBulkInsertByChunksCommit")
        .append_query_param("processId", Some(process_id))
        .append_data_sync_period(sync_period)
        .post(HttpRequestBody::Empty)
        .await?;

    if is_ok_result(&response) {
        return Ok(());
    }

    let reason = response.receive_body().await?;
    let reason = String::from_utf8(reason)?;
    return Err(DataWriterError::Error(reason));
}

/// POST /api/Bulk/CleanAndBulkInsertByChunksCancel — drops the accumulated chunks. The table
/// is not touched.
pub async fn clean_and_bulk_insert_by_chunks_cancel(
    flurl: FlUrl,
    process_id: &str,
) -> Result<(), DataWriterError> {
    let response = flurl
        .append_path_segment(API_SEGMENT)
        .append_path_segment(BULK_CONTROLLER)
        .append_path_segment("CleanAndBulkInsertByChunksCancel")
        .append_query_param("processId", Some(process_id))
        .post(HttpRequestBody::Empty)
        .await?;

    if is_ok_result(&response) {
        return Ok(());
    }

    let reason = response.receive_body().await?;
    let reason = String::from_utf8(reason)?;
    return Err(DataWriterError::Error(reason));
}

#[derive(Deserialize)]
struct BulkProcessResponseContract {
    #[serde(rename = "processId")]
    process_id: String,
}

fn is_ok_result(response: &FlUrlResponse) -> bool {
    response.get_status_code() >= 200 && response.get_status_code() < 300
}

fn serialize_entities_to_body<TEntity: MyNoSqlEntity + MyNoSqlEntitySerializer>(
    entities: &[TEntity],
) -> HttpRequestBody {
    if entities.len() == 0 {
        HttpRequestBody::Json(vec![b'[', b']']);
    }

    let mut json_array_writer = JsonArrayWriter::new();

    for entity in entities {
        let payload = entity.serialize_entity();
        let payload: RawJsonObject = payload.into();
        json_array_writer = json_array_writer.write(payload);
    }

    HttpRequestBody::Json(json_array_writer.build().into_bytes())
}

async fn check_error(response: &mut FlUrlResponse) -> Result<(), DataWriterError> {
    let result = match response.get_status_code() {
        400 => Err(deserialize_error(response).await?),

        // 409 is an optimistic-concurrency conflict ("Record is changed"), not a missing
        // table. The body is a plain-text message, not an OperationFailHttpContract.
        409 => {
            let body = response.get_body_as_slice().await?;
            Err(DataWriterError::RecordIsChanged(
                String::from_utf8_lossy(body).to_string(),
            ))
        }
        _ => Ok(()),
    };

    if let Err(err) = &result {
        if !is_expected_outcome(err) {
            my_logger::LOGGER.write_error(
                format!("FlUrlRequest to {}", response.url.to_string()),
                format!("{:?}", err),
                None.into(),
            );
        }
    }

    result
}

/// Errors which are the API answering normally rather than something going wrong. They are
/// carried to the caller to act on, and are deliberately not written to the log - a routine
/// outcome must not look like a failure of the service which is using the writer.
///
/// [`DataWriterError::RecordIsChanged`] is exactly that: `DeleteIf` / `Replace` answer 409
/// whenever the row was rewritten between the read and the write, which is what the
/// optimistic-concurrency protocol is for. The caller re-reads and decides again, and
/// `update_entity` even retries it in a loop - logging it once per attempt turned an ordinary
/// conflict into a stream of errors in the console.
///
/// [`DataWriterError::RecordNotFound`] is the same kind of answer ("it is not there any
/// more"); every caller maps 404 before `check_error` today, so it is listed here to keep the
/// rule in one place rather than because a 404 reaches this function.
fn is_expected_outcome(err: &DataWriterError) -> bool {
    match err {
        DataWriterError::RecordIsChanged(_) | DataWriterError::RecordNotFound(_) => true,
        _ => false,
    }
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

/// The optimistic-concurrency read-modify-write loop shared by every `update_entity`
/// wrapper. Kept transport-agnostic (parameterized by `read` / `replace` closures) so the
/// retry logic can be unit-tested without a live server.
///
/// - `read` fetches the current entity (its `TimeStamp` is the version to send back);
/// - `update` mutates the caller's fields in place — it must NOT touch `time_stamp`;
/// - `replace` writes it and hands the entity back alongside the result.
///
/// On [`DataWriterError::RecordIsChanged`] the loop re-reads (fresh version) and re-applies
/// `update`, up to `max_attempts`; on exhaustion the last `RecordIsChanged` is returned.
/// A missing row (`read` → `None`) yields `Ok(None)`; any other error is propagated as-is.
pub(crate) async fn run_read_modify_write<TEntity, TFn, FRead, RFut, FReplace, PFut>(
    max_attempts: usize,
    mut update: TFn,
    mut read: FRead,
    mut replace: FReplace,
) -> Result<Option<TEntity>, DataWriterError>
where
    TFn: FnMut(&mut TEntity),
    FRead: FnMut() -> RFut,
    RFut: std::future::Future<Output = Result<Option<TEntity>, DataWriterError>>,
    FReplace: FnMut(TEntity) -> PFut,
    PFut: std::future::Future<Output = (TEntity, Result<(), DataWriterError>)>,
{
    let mut attempt: usize = 0;

    loop {
        let mut entity = match read().await? {
            Some(entity) => entity,
            None => return Ok(None),
        };

        update(&mut entity);

        let (entity, replace_result) = replace(entity).await;

        match replace_result {
            Ok(()) => return Ok(Some(entity)),
            Err(DataWriterError::RecordIsChanged(message)) => {
                attempt += 1;
                if attempt >= max_attempts {
                    return Err(DataWriterError::RecordIsChanged(message));
                }
                // Otherwise loop: re-read the fresh version and re-apply `update`.
            }
            Err(err) => return Err(err),
        }
    }
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

        let body = as_json.into_vec();

        println!("{}", std::str::from_utf8(&body).unwrap());
    }

    /// Mirrors what `#[my_no_sql_entity]` generates for the TimeStamp field, so we can
    /// verify how a real / default `Timestamp` actually serializes on the InsertOrReplaceIfNew
    /// path (where the server requires a parseable ISO TimeStamp).
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct TimeStampedTestEntity {
        partition_key: String,
        row_key: String,
        #[serde(rename = "TimeStamp")]
        #[serde(skip_serializing_if = "my_no_sql_abstractions::skip_timestamp_serializing")]
        time_stamp: Timestamp,
    }

    impl MyNoSqlEntity for TimeStampedTestEntity {
        const TABLE_NAME: &'static str = "test";
        const LAZY_DESERIALIZATION: bool = false;

        fn get_partition_key(&self) -> &str {
            &self.partition_key
        }

        fn get_row_key(&self) -> &str {
            &self.row_key
        }

        fn get_time_stamp(&self) -> Timestamp {
            self.time_stamp
        }
    }

    impl MyNoSqlEntitySerializer for TimeStampedTestEntity {
        fn serialize_entity(&self) -> Vec<u8> {
            my_no_sql_core::entity_serializer::serialize(self)
        }

        fn deserialize_entity(src: &[u8]) -> Result<Self, String> {
            my_no_sql_core::entity_serializer::deserialize(src)
        }
    }

    #[test]
    fn real_timestamp_serializes_as_parseable_iso() {
        use rust_extensions::date_time::DateTimeAsMicroseconds;

        let entity = TimeStampedTestEntity {
            partition_key: "pk".to_string(),
            row_key: "rk".to_string(),
            time_stamp: DateTimeAsMicroseconds::from_str("2025-01-01T12:00:00.123456")
                .unwrap()
                .into(),
        };

        let body = entity.serialize_entity();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let ts = json
            .get("TimeStamp")
            .expect("TimeStamp must be present for a real value")
            .as_str()
            .expect("TimeStamp must be a string");

        // The server parses it exactly like this; if it were null/empty it would 400.
        assert!(
            DateTimeAsMicroseconds::parse_iso_string(ts).is_some(),
            "serialized TimeStamp '{}' must be a parseable ISO date-time",
            ts
        );
    }

    #[test]
    fn default_timestamp_is_omitted() {
        let entity = TimeStampedTestEntity {
            partition_key: "pk".to_string(),
            row_key: "rk".to_string(),
            time_stamp: Timestamp::default(),
        };

        let body = entity.serialize_entity();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // A default Timestamp is skipped entirely — the server sees no TimeStamp and
        // rejects an InsertOrReplaceIfNew request with HTTP 400. This is exactly why the
        // wrapper methods debug_assert on a non-default TimeStamp.
        assert!(
            json.get("TimeStamp").is_none(),
            "a default Timestamp must not serialize to a value, got: {}",
            String::from_utf8_lossy(&body)
        );
    }

    #[tokio::test]
    async fn empty_bulk_insert_or_replace_if_new_is_ok() {
        // The empty-slice guard returns before any request is built, so no server is needed.
        let flurl = flurl::FlUrl::new("http://127.0.0.1:0");
        let result = super::bulk_insert_or_replace_if_new::<TimeStampedTestEntity>(
            flurl,
            &[],
            &my_no_sql_abstractions::DataSynchronizationPeriod::Immediately,
        )
        .await;

        assert!(result.is_ok(), "empty bulk must be a no-op Ok(()), got {:?}", result.err());
    }

    #[tokio::test]
    async fn empty_bulk_delete_if_is_ok() {
        // Same empty-input guard as bulk_delete: the answer is built without a request, so
        // no server is needed.
        let result = super::bulk_delete_if::<TimeStampedTestEntity>(
            flurl::FlUrl::new("http://127.0.0.1:0"),
            &[],
            &my_no_sql_abstractions::DataSynchronizationPeriod::Immediately,
        )
        .await
        .unwrap();

        assert_eq!(result.deleted, 0);
        assert!(result.is_all_deleted());

        let result = super::bulk_delete_if_rows::<TimeStampedTestEntity>(
            flurl::FlUrl::new("http://127.0.0.1:0"),
            &[],
            &my_no_sql_abstractions::DataSynchronizationPeriod::Immediately,
        )
        .await
        .unwrap();

        assert_eq!(result.deleted, 0);
        assert!(result.is_all_deleted());
    }

    #[test]
    fn chunking_splits_as_expected() {
        // The chunked one-call method relies on slice::chunks; pin the boundaries it produces.
        let entities: Vec<u32> = (0..10).collect();

        let lens: Vec<usize> = entities.chunks(3).map(|c| c.len()).collect();
        assert_eq!(lens, vec![3, 3, 3, 1]);

        let lens: Vec<usize> = entities.chunks(5).map(|c| c.len()).collect();
        assert_eq!(lens, vec![5, 5]);

        let lens: Vec<usize> = entities.chunks(100).map(|c| c.len()).collect();
        assert_eq!(lens, vec![10]);
    }

    use crate::DataWriterError;
    use std::cell::Cell;

    // A tiny entity for the read-modify-write loop tests. `version` stands in for the
    // stored TimeStamp; `value` is the field the closure mutates.
    #[derive(Debug, PartialEq)]
    struct LoopEntity {
        version: i64,
        value: i32,
    }

    #[tokio::test]
    async fn update_loop_retries_on_conflict_then_succeeds() {
        let read_calls = Cell::new(0i32);
        let replace_calls = Cell::new(0i32);

        // Server-side "stored version" bumps on every write attempt so the first two
        // replaces see a stale version (409) and the third one matches.
        let stored_version = Cell::new(10i64);

        let result: Result<Option<LoopEntity>, DataWriterError> = super::run_read_modify_write(
            5,
            |e: &mut LoopEntity| e.value += 1,
            || {
                read_calls.set(read_calls.get() + 1);
                let version = stored_version.get();
                async move {
                    Ok(Some(LoopEntity {
                        version,
                        value: 100,
                    }))
                }
            },
            |e: LoopEntity| {
                let n = replace_calls.get() + 1;
                replace_calls.set(n);
                // The row moves on under us for the first two attempts.
                stored_version.set(stored_version.get() + 1);
                async move {
                    if n < 3 {
                        (e, Err(DataWriterError::RecordIsChanged("changed".to_string())))
                    } else {
                        (e, Ok(()))
                    }
                }
            },
        )
        .await;

        // Each attempt starts from a fresh read (value 100) and applies the closure once.
        assert_eq!(result.unwrap(), Some(LoopEntity { version: 12, value: 101 }));
        assert_eq!(read_calls.get(), 3, "must re-read on every conflict");
        assert_eq!(replace_calls.get(), 3);
    }

    #[tokio::test]
    async fn update_loop_gives_up_after_max_attempts() {
        let replace_calls = Cell::new(0i32);

        let result: Result<Option<LoopEntity>, DataWriterError> = super::run_read_modify_write(
            3,
            |_e: &mut LoopEntity| {},
            || async { Ok(Some(LoopEntity { version: 1, value: 0 })) },
            |e: LoopEntity| {
                replace_calls.set(replace_calls.get() + 1);
                async move {
                    (e, Err(DataWriterError::RecordIsChanged("still conflicting".to_string())))
                }
            },
        )
        .await;

        match result {
            Err(DataWriterError::RecordIsChanged(msg)) => assert_eq!(msg, "still conflicting"),
            other => panic!("expected RecordIsChanged, got {:?}", other),
        }
        assert_eq!(replace_calls.get(), 3, "must stop exactly at max_attempts");
    }

    #[tokio::test]
    async fn update_loop_returns_none_when_row_missing() {
        let replace_calls = Cell::new(0i32);

        let result: Result<Option<LoopEntity>, DataWriterError> = super::run_read_modify_write(
            5,
            |_e: &mut LoopEntity| panic!("update must not be called when the row is missing"),
            || async { Ok(None) },
            |e: LoopEntity| {
                replace_calls.set(replace_calls.get() + 1);
                async move { (e, Ok(())) }
            },
        )
        .await;

        assert_eq!(result.unwrap(), None);
        assert_eq!(replace_calls.get(), 0, "must not attempt a replace");
    }

    #[tokio::test]
    async fn update_loop_propagates_other_errors_without_retry() {
        let replace_calls = Cell::new(0i32);

        let result: Result<Option<LoopEntity>, DataWriterError> = super::run_read_modify_write(
            5,
            |_e: &mut LoopEntity| {},
            || async { Ok(Some(LoopEntity { version: 1, value: 0 })) },
            |e: LoopEntity| {
                replace_calls.set(replace_calls.get() + 1);
                async move { (e, Err(DataWriterError::RecordNotFound("gone".to_string()))) }
            },
        )
        .await;

        assert!(matches!(result, Err(DataWriterError::RecordNotFound(_))));
        assert_eq!(replace_calls.get(), 1, "a non-conflict error must not be retried");
    }
}
