use std::sync::Arc;

use my_json::json_writer::JsonValueWriter;
#[cfg(feature = "master-node")]
use rust_extensions::date_time::AtomicDateTimeAsMicroseconds;
#[cfg(feature = "master-node")]
use rust_extensions::date_time::DateTimeAsMicroseconds;
use rust_extensions::sorted_vec::EntityWithStrKey;

use crate::db::PartitionKeyParameter;
use crate::db_json_entity::DbJsonEntity;

use super::RowKeyParameter;

pub struct DbRow {
    partition_key: crate::db_json_entity::KeyValueContentPosition,
    row_key: crate::db_json_entity::KeyValueContentPosition,
    raw: Vec<u8>,
    #[cfg(feature = "master-node")]
    expires_value: AtomicDateTimeAsMicroseconds,
    #[cfg(feature = "master-node")]
    expires: Option<crate::db_json_entity::JsonKeyValuePosition>,
    #[cfg(feature = "master-node")]
    pub time_stamp: crate::db_json_entity::KeyValueContentPosition,
    #[cfg(feature = "master-node")]
    last_read_access: AtomicDateTimeAsMicroseconds,
}

impl DbRow {
    pub fn new(db_json_entity: DbJsonEntity, raw: Vec<u8>) -> Self {
        #[cfg(feature = "debug_db_row")]
        println!(
            "Created DbRow: PK:{}. RK:{}. Expires{:?}",
            db_json_entity.get_partition_key(raw.as_slice()),
            db_json_entity.get_row_key(raw.as_slice()),
            db_json_entity.expires
        );

        #[cfg(feature = "master-node")]
        let time_stamp = db_json_entity.time_stamp.unwrap();
        #[cfg(feature = "master-node")]
        let time_stamp_value =
            DateTimeAsMicroseconds::from_str(time_stamp.value.get_str_value(&raw)).unwrap();

        Self {
            raw,
            partition_key: db_json_entity.partition_key.value,
            row_key: db_json_entity.row_key.value,
            #[cfg(feature = "master-node")]
            time_stamp: time_stamp.value,
            #[cfg(feature = "master-node")]
            expires_value: if let Some(expires_value) = db_json_entity.expires_value {
                AtomicDateTimeAsMicroseconds::new(expires_value.unix_microseconds)
            } else {
                AtomicDateTimeAsMicroseconds::new(0)
            },
            #[cfg(feature = "master-node")]
            expires: db_json_entity.expires,
            #[cfg(feature = "master-node")]
            last_read_access: AtomicDateTimeAsMicroseconds::new(time_stamp_value.unix_microseconds),
        }
    }

    pub fn get_partition_key(&self) -> &str {
        self.partition_key.get_str_value(&self.raw)
    }

    pub fn get_row_key(&self) -> &str {
        self.row_key.get_str_value(&self.raw)
    }

    pub fn get_time_stamp(&self) -> &str {
        self.row_key.get_str_value(&self.raw)
    }
    pub fn get_src_as_slice(&self) -> &[u8] {
        self.raw.as_slice()
    }

    #[cfg(feature = "master-node")]
    pub fn update_last_read_access(
        &self,
        value: rust_extensions::date_time::DateTimeAsMicroseconds,
    ) {
        self.last_read_access.update(value);
    }
    #[cfg(feature = "master-node")]
    pub fn get_last_read_access(&self) -> rust_extensions::date_time::DateTimeAsMicroseconds {
        self.last_read_access.as_date_time()
    }

    #[cfg(feature = "master-node")]
    pub fn update_expires(
        &self,
        expires: Option<DateTimeAsMicroseconds>,
    ) -> Option<DateTimeAsMicroseconds> {
        let old_value = self.get_expires();

        if let Some(expires) = expires {
            self.expires_value.update(expires);
        } else {
            self.expires_value.update(DateTimeAsMicroseconds::new(0));
        }

        old_value
    }
    #[cfg(feature = "master-node")]
    pub fn get_expires(&self) -> Option<DateTimeAsMicroseconds> {
        let result = self.expires_value.as_date_time();

        if result.unix_microseconds == 0 {
            None
        } else {
            Some(result)
        }
    }
    #[cfg(feature = "master-node")]
    pub fn write_json(&self, out: &mut String) {
        let expires_value = self.get_expires();

        if expires_value.is_none() {
            if let Some(expires) = &self.expires {
                if let Some(before_separator) =
                    find_json_separator_before(&self.raw, expires.key.start - 1)
                {
                    unsafe {
                        out.push_str(std::str::from_utf8_unchecked(&self.raw[..before_separator]));
                        out.push_str(std::str::from_utf8_unchecked(
                            &self.raw[expires.value.end..],
                        ));
                    }
                    return;
                }

                if let Some(after_separator) =
                    find_json_separator_after(&self.raw, expires.value.end)
                {
                    unsafe {
                        out.push_str(std::str::from_utf8_unchecked(
                            &self.raw[..expires.key.start],
                        ));
                        out.push_str(std::str::from_utf8_unchecked(&self.raw[after_separator..]));
                    }
                    return;
                }

                unsafe {
                    out.push_str(std::str::from_utf8_unchecked(
                        &self.raw[..expires.key.start],
                    ));
                    out.push_str(std::str::from_utf8_unchecked(
                        &self.raw[expires.value.end..],
                    ));
                }
            } else {
                unsafe {
                    out.push_str(std::str::from_utf8_unchecked(&self.raw));
                }
            }

            return;
        }

        let expires_value = expires_value.unwrap();

        unsafe {
            if let Some(expires) = &self.expires {
                out.push_str(std::str::from_utf8_unchecked(
                    &self.raw[..expires.key.start],
                ));
                inject_expires(out, expires_value);
                out.push_str(std::str::from_utf8_unchecked(
                    &self.raw[expires.value.end..],
                ));
            } else {
                let end_of_json = crate::db_json_entity::get_the_end_of_the_json(&self.raw);
                out.push_str(std::str::from_utf8_unchecked(&self.raw[..end_of_json]));
                out.push(',');
                inject_expires(out, expires_value);
                out.push_str(std::str::from_utf8_unchecked(&self.raw[end_of_json..]));
            }
        }
    }

    #[cfg(not(feature = "master-node"))]
    pub fn write_json(&self, out: &mut String) {
        let str = unsafe { std::str::from_utf8_unchecked(&self.raw) };
        out.push_str(str);
    }

    pub fn to_vec(&self) -> Vec<u8> {
        let mut result = String::new();
        self.write_json(&mut result);
        result.into_bytes()
    }
}

impl EntityWithStrKey for DbRow {
    fn get_key(&self) -> &str {
        self.get_row_key()
    }
}

impl PartitionKeyParameter for Arc<DbRow> {
    fn as_str(&self) -> &str {
        self.get_partition_key()
    }

    fn into_partition_key(self) -> crate::db::PartitionKey {
        self.get_partition_key().into()
    }

    fn to_partition_key(&self) -> crate::db::PartitionKey {
        self.get_partition_key().into()
    }
}

impl RowKeyParameter for Arc<DbRow> {
    fn as_str(&self) -> &str {
        self.get_row_key()
    }
}

#[cfg(feature = "master-node")]
fn inject_expires(out: &mut String, expires_value: DateTimeAsMicroseconds) {
    out.push('"');
    out.push_str(crate::db_json_entity::consts::EXPIRES);
    out.push_str("\":\"");
    out.push_str(&expires_value.to_rfc3339()[..19]);
    out.push('"');
}
#[cfg(feature = "master-node")]
fn find_json_separator_before(src: &[u8], pos: usize) -> Option<usize> {
    let mut i = pos;
    while i > 0 {
        let b = src[i];

        if b <= 32 {
            i -= 1;
            continue;
        }

        if b == b',' {
            return Some(i);
        }

        break;
    }

    None
}
#[cfg(feature = "master-node")]
fn find_json_separator_after(src: &[u8], pos: usize) -> Option<usize> {
    let mut i = pos;
    while i < src.len() {
        let b = src[i];

        if b <= 32 {
            i += 1;
            continue;
        }

        if b == b',' {
            return Some(i + 1);
        }

        break;
    }

    None
}

impl JsonValueWriter for &'_ DbRow {
    const IS_ARRAY: bool = false;

    fn write(&self, dest: &mut String) {
        self.write_json(dest)
    }
}

#[cfg(feature = "master-node")]
impl crate::ExpirationIndex<Arc<DbRow>> for Arc<DbRow> {
    fn get_id_as_str(&self) -> &str {
        self.get_row_key()
    }

    fn to_owned(&self) -> Arc<DbRow> {
        self.clone()
    }

    fn get_expiration_moment(&self) -> Option<rust_extensions::date_time::DateTimeAsMicroseconds> {
        self.get_expires()
    }
}

#[cfg(feature = "debug_db_row")]
impl Drop for DbRow {
    fn drop(&mut self) {
        println!(
            "Dropped DbRow: PK:{}. RK:{}",
            self.get_partition_key(),
            self.get_row_key(),
        );
    }
}
