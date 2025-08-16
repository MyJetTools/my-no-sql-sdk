#[cfg(feature = "master-node")]
use rust_extensions::date_time::DateTimeAsMicroseconds;
use rust_extensions::sorted_vec::SortedVecOfArcWithStrKey;
use std::sync::Arc;

use crate::db::DbRow;

pub struct DbRowsContainer {
    data: SortedVecOfArcWithStrKey<DbRow>,

    #[cfg(feature = "master-node")]
    rows_with_expiration_index: crate::ExpirationIndexContainer<Arc<DbRow>>,
}

impl DbRowsContainer {
    pub fn new() -> Self {
        Self {
            data: SortedVecOfArcWithStrKey::new(),
            #[cfg(feature = "master-node")]
            rows_with_expiration_index: crate::ExpirationIndexContainer::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[cfg(feature = "master-node")]
    pub fn rows_with_expiration_index_len(&self) -> usize {
        self.rows_with_expiration_index.len()
    }
    #[cfg(feature = "master-node")]
    pub fn get_rows_to_expire(&self, now: DateTimeAsMicroseconds) -> Vec<Arc<DbRow>> {
        self.rows_with_expiration_index
            .get_items_to_expire(now, |itm| itm.clone())
    }

    #[cfg(feature = "master-node")]
    pub fn get_rows_to_gc_by_max_amount(&self, max_rows_amount: usize) -> Option<Vec<Arc<DbRow>>> {
        if self.data.len() <= max_rows_amount {
            return None;
        }

        let mut by_last_read_access = Vec::new();

        for db_row in self.data.iter() {
            match by_last_read_access.binary_search_by(|itm: &Arc<DbRow>| {
                itm.get_last_read_access()
                    .unix_microseconds
                    .cmp(&db_row.get_last_read_access().unix_microseconds)
            }) {
                Ok(index) => {
                    by_last_read_access.insert(index, db_row.clone());
                }
                Err(index) => {
                    by_last_read_access.insert(index, db_row.clone());
                }
            }

            //by_last_read_access.insert(last_read_access, db_row.clone());
        }

        while by_last_read_access.len() > max_rows_amount {
            by_last_read_access.pop();
        }

        Some(by_last_read_access)
    }

    pub fn insert(&mut self, db_row: Arc<DbRow>) -> Option<Arc<DbRow>> {
        #[cfg(feature = "master-node")]
        let added = self.rows_with_expiration_index.add(&db_row);

        let (_, removed_db_row) = self.data.insert_or_replace(db_row);

        #[cfg(feature = "master-node")]
        if let Some(added) = added {
            if added {
                if let Some(removed_db_row) = &removed_db_row {
                    self.rows_with_expiration_index.remove(removed_db_row);
                }
            }
        }

        removed_db_row
    }

    pub fn remove(&mut self, row_key: &str) -> Option<Arc<DbRow>> {
        let result = self.data.remove(row_key);

        #[cfg(feature = "master-node")]
        if let Some(removed_db_row) = &result {
            self.rows_with_expiration_index.remove(removed_db_row);
        }

        result
    }

    pub fn get(&self, row_key: &str) -> Option<&Arc<DbRow>> {
        self.data.get(row_key)
    }

    pub fn has_db_row(&self, row_key: &str) -> bool {
        return self.data.contains(row_key);
    }

    pub fn get_all<'s>(&'s self) -> std::slice::Iter<'s, Arc<DbRow>> {
        self.data.iter()
    }

    pub fn get_highest_row_and_below(&self, row_key: &String) -> &[Arc<DbRow>] {
        self.data.get_from_bottom_to_key(row_key)
    }

    #[cfg(feature = "master-node")]
    pub fn update_expiration_time(
        &mut self,
        row_key: &str,
        expiration_time: Option<DateTimeAsMicroseconds>,
    ) -> Option<Arc<DbRow>> {
        if let Some(db_row) = self.get(row_key).cloned() {
            let old_expires = db_row.update_expires(expiration_time);

            if are_expires_the_same(old_expires, expiration_time) {
                return None;
            }

            self.rows_with_expiration_index.update(old_expires, &db_row);

            return Some(db_row);
        }

        None
    }
}

#[cfg(feature = "master-node")]
fn are_expires_the_same(
    old_expires: Option<rust_extensions::date_time::DateTimeAsMicroseconds>,
    new_expires: Option<rust_extensions::date_time::DateTimeAsMicroseconds>,
) -> bool {
    if let Some(old_expires) = old_expires {
        if let Some(new_expires) = new_expires {
            return new_expires.unix_microseconds == old_expires.unix_microseconds;
        }

        return false;
    }

    if new_expires.is_some() {
        return false;
    }

    true
}

#[cfg(feature = "master-node")]
#[cfg(test)]
mod expiration_tests {

    use rust_extensions::date_time::DateTimeAsMicroseconds;

    use crate::db_json_entity::{DbJsonEntity, JsonTimeStamp};

    use super::*;

    #[test]
    fn test_that_index_appears() {
        let test_json = r#"{
            "PartitionKey": "test",
            "RowKey": "test",
            "Expires": "2019-01-01T00:00:00"
        }"#;

        let time_stamp = JsonTimeStamp::now();
        let db_row =
            DbJsonEntity::parse_into_db_row(test_json.as_bytes().into(), &time_stamp).unwrap();

        let mut db_rows = DbRowsContainer::new();

        db_rows.insert(Arc::new(db_row));

        db_rows.rows_with_expiration_index.assert_len(1);
    }

    #[test]
    fn test_that_index_does_not_appear_since_we_do_not_have_expiration() {
        let test_json = r#"{
            "PartitionKey": "test",
            "RowKey": "test"
        }"#;

        let time_stamp = JsonTimeStamp::now();
        let db_row =
            DbJsonEntity::parse_into_db_row(test_json.as_bytes().into(), &time_stamp).unwrap();

        let mut db_rows = DbRowsContainer::new();

        db_rows.insert(Arc::new(db_row));

        db_rows.rows_with_expiration_index.assert_len(0);
    }

    #[test]
    fn test_that_index_is_removed() {
        let test_json = r#"{
            "PartitionKey": "test",
            "RowKey": "test",
            "Expires": "2019-01-01T00:00:00"
        }"#;
        let time_stamp = JsonTimeStamp::now();
        let db_row =
            DbJsonEntity::parse_into_db_row(test_json.as_bytes().into(), &time_stamp).unwrap();

        let mut db_rows = DbRowsContainer::new();

        db_rows.insert(Arc::new(db_row));

        db_rows.remove("test");

        db_rows.rows_with_expiration_index.assert_len(0);
    }

    #[test]
    fn test_update_expiration_time_from_no_to() {
        let test_json = r#"{
            "PartitionKey": "test",
            "RowKey": "test"
        }"#;

        let time_stamp = JsonTimeStamp::now();

        let db_row =
            DbJsonEntity::parse_into_db_row(test_json.as_bytes().into(), &time_stamp).unwrap();

        let mut db_rows = DbRowsContainer::new();

        db_rows.insert(Arc::new(db_row));

        db_rows.rows_with_expiration_index.assert_len(0);

        let new_expiration_time = DateTimeAsMicroseconds::new(2);

        db_rows.update_expiration_time("test", Some(new_expiration_time));

        assert_eq!(
            true,
            db_rows
                .rows_with_expiration_index
                .has_data_with_expiration_moment(DateTimeAsMicroseconds::new(2))
        );
        db_rows.rows_with_expiration_index.assert_len(1);
    }

    #[test]
    fn test_update_expiration_time_to_new_expiration_time() {
        let test_json = r#"{
            "PartitionKey": "test",
            "RowKey": "test",
            "Expires": "2019-01-01T00:00:00"
        }"#;

        let time_stamp = JsonTimeStamp::now();
        let db_row =
            DbJsonEntity::parse_into_db_row(test_json.as_bytes().into(), &time_stamp).unwrap();

        let mut db_rows = DbRowsContainer::new();

        db_rows.insert(Arc::new(db_row));

        let current_expiration = DateTimeAsMicroseconds::from_str("2019-01-01T00:00:00").unwrap();

        assert_eq!(
            true,
            db_rows
                .rows_with_expiration_index
                .has_data_with_expiration_moment(current_expiration)
        );
        db_rows.rows_with_expiration_index.assert_len(1);

        db_rows.update_expiration_time("test", Some(DateTimeAsMicroseconds::new(2)));

        assert_eq!(
            true,
            db_rows
                .rows_with_expiration_index
                .has_data_with_expiration_moment(DateTimeAsMicroseconds::new(2))
        );

        db_rows.rows_with_expiration_index.assert_len(1);
    }

    #[test]
    fn test_update_expiration_time_from_some_to_no() {
        let mut db_rows = DbRowsContainer::new();

        let test_json = r#"{
            "PartitionKey": "test",
            "RowKey": "test",
            "Expires": "2019-01-01T00:00:00"
        }"#;

        let now = JsonTimeStamp::now();
        let db_row = DbJsonEntity::parse_into_db_row(test_json.as_bytes().into(), &now).unwrap();

        db_rows.insert(Arc::new(db_row));

        let current_expiration = DateTimeAsMicroseconds::from_str("2019-01-01T00:00:00").unwrap();

        assert_eq!(
            true,
            db_rows
                .rows_with_expiration_index
                .has_data_with_expiration_moment(current_expiration)
        );
        assert_eq!(1, db_rows.rows_with_expiration_index.len());

        db_rows.update_expiration_time("test", None);
        assert_eq!(0, db_rows.rows_with_expiration_index.len());
    }

    #[test]
    fn test_we_do_not_have_db_rows_to_expire() {
        let mut db_rows = DbRowsContainer::new();

        let test_json = r#"{
            "PartitionKey": "test",
            "RowKey": "test",
            "Expires": "2019-01-01T00:00:00"
        }"#;

        let now = JsonTimeStamp::now();

        let db_row = DbJsonEntity::parse_into_db_row(test_json.as_bytes().into(), &now).unwrap();

        db_rows.insert(Arc::new(db_row));

        let mut now = DateTimeAsMicroseconds::from_str("2019-01-01T00:00:00").unwrap();
        now.unix_microseconds -= 1;

        let rows_to_expire = db_rows
            .rows_with_expiration_index
            .get_items_to_expire(now, |itm| itm.clone());

        assert_eq!(0, rows_to_expire.len());
    }

    #[test]
    fn test_we_do_have_db_rows_to_expire() {
        let mut db_rows = DbRowsContainer::new();

        let test_json = r#"{
            "PartitionKey": "test",
            "RowKey": "test",
            "Expires": "2019-01-01T00:00:00"
        }"#;
        let now = JsonTimeStamp::now();

        let db_row = DbJsonEntity::parse_into_db_row(test_json.as_bytes().into(), &now).unwrap();

        db_rows.insert(Arc::new(db_row));

        let now = DateTimeAsMicroseconds::from_str("2019-01-01T00:00:00").unwrap();

        let rows_to_expire = db_rows
            .rows_with_expiration_index
            .get_items_to_expire(now, |itm| itm.clone());

        assert_eq!(true, rows_to_expire.len() > 0);
    }

    #[test]
    fn check_gc_max_rows_amount() {
        let mut db_rows = DbRowsContainer::new();

        let mut now = DateTimeAsMicroseconds::now();

        let json = r#"{
            "PartitionKey": "test",
            "RowKey": "test1"
        }"#;

        let db_row = DbJsonEntity::parse_into_db_row(
            json.as_bytes().into(),
            &JsonTimeStamp::from_date_time(now),
        )
        .unwrap();

        db_rows.insert(Arc::new(db_row));

        // Next Item

        now.add_seconds(1);

        let raw_json = r#"{
            "PartitionKey": "test",
            "RowKey": "test2"
        }"#;

        let db_row = DbJsonEntity::parse_into_db_row(
            raw_json.as_bytes().into(),
            &JsonTimeStamp::from_date_time(now),
        )
        .unwrap();

        db_rows.insert(Arc::new(db_row));

        // Next Item

        now.add_seconds(1);

        let json_db_row = r#"{
            "PartitionKey": "test",
            "RowKey": "test3"
        }"#;

        let db_row = DbJsonEntity::parse_into_db_row(
            json_db_row.as_bytes().into(),
            &JsonTimeStamp::from_date_time(now),
        )
        .unwrap();

        db_rows.insert(Arc::new(db_row));

        // Next Item

        now.add_seconds(1);

        let raw_json = r#"{
            "PartitionKey": "test",
            "RowKey": "test4"
        }"#;

        let db_row = DbJsonEntity::parse_into_db_row(
            raw_json.as_bytes().into(),
            &JsonTimeStamp::from_date_time(now),
        )
        .unwrap();

        db_rows.insert(Arc::new(db_row));

        let db_rows_to_gc = db_rows.get_rows_to_gc_by_max_amount(3).unwrap();

        assert_eq!("test1", db_rows_to_gc.get(0).unwrap().get_row_key());
    }

    #[test]
    fn check_we_update_row_with_the_same_expiration_date() {
        let mut db_rows = DbRowsContainer::new();

        let row = r#"{"Count":1,"PartitionKey":"in-progress-count1","RowKey":"my-id","Expires":"2025-03-12T10:55:46.0507979Z"}"#;
        let now = JsonTimeStamp::now();
        let db_json_entity = DbJsonEntity::parse(row.as_bytes(), &now).unwrap();
        let db_row = Arc::new(db_json_entity.into_db_row().unwrap());
        db_rows.insert(db_row);
        db_rows.rows_with_expiration_index.assert_len(1);

        let row = r#"{"Count":1,"PartitionKey":"in-progress-count1","RowKey":"my-id","Expires":"2025-03-12T10:55:46.0507979Z"}"#;
        let now = JsonTimeStamp::now();
        let db_json_entity = DbJsonEntity::parse(row.as_bytes(), &now).unwrap();
        let db_row = Arc::new(db_json_entity.into_db_row().unwrap());
        db_rows.insert(db_row);
        db_rows.rows_with_expiration_index.assert_len(1);

        db_rows.remove("my-id");
    }

    #[test]
    fn check_we_update_same_row_with_new_expiration_date() {
        let mut db_rows = DbRowsContainer::new();

        let row = r#"{"Count":1,"PartitionKey":"in-progress-count1","RowKey":"my-id","Expires":"2025-03-12T10:55:48.0507979Z"}"#;
        let now = JsonTimeStamp::now();
        let db_json_entity = DbJsonEntity::parse(row.as_bytes(), &now).unwrap();
        let db_row = Arc::new(db_json_entity.into_db_row().unwrap());
        db_rows.insert(db_row);
        db_rows.rows_with_expiration_index.assert_len(1);

        let row = r#"{"Count":1,"PartitionKey":"in-progress-count1","RowKey":"my-id","Expires":"2025-03-12T10:55:50.0507979Z"}"#;
        let now = JsonTimeStamp::now();
        let db_json_entity = DbJsonEntity::parse(row.as_bytes(), &now).unwrap();
        let db_row = Arc::new(db_json_entity.into_db_row().unwrap());
        db_rows.insert(db_row);
        db_rows.rows_with_expiration_index.assert_len(1);

        db_rows.remove("my-id");

        db_rows.rows_with_expiration_index.assert_len(0);
    }
}
