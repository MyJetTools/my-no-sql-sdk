## MyNoSql Entity Design Patterns

### Purpose
- Provide consistent guidance for defining MyNoSql entities and enum-based models used by readers/writers.
- Align with MyNoSql server concepts of `PartitionKey`, `RowKey`, `TimeStamp`, and optional `Expires`.
- Serve as an instruction set for AI agents so generated code follows established patterns without guesswork.

### Core principles
- Entities always carry partition, row, and timestamp metadata; these identify rows per the server model.
- Use `my-no-sql-macros` to autogenerate required fields and trait impls instead of hand-writing boilerplate.
- Serde names `PartitionKey`, `RowKey`, `TimeStamp`, and `Expires` are reserved; avoid collisions or custom renames that reuse them.
- Table rows are identified by `(PartitionKey, RowKey)`; partitions group related rows; `TimeStamp` tracks server-side update time.
- Writers use HTTP CRUD; readers subscribe and keep local copies—design entities to be compact and stable.

### Table name validation (my-no-sql-server)
- Length: 3–63 characters.
- Allowed characters: lowercase latin letters `a`-`z`, digits `0`-`9`, and dash `-`.
- No consecutive dashes (e.g., `my--table-name` is invalid).
- Do not start or end with a dash; prefer starting with a letter (e.g., `-my-table-name` and `my-table-name-` are invalid).
- Keep names lowercase and stable; changing table names is a breaking change for both readers and writers.
- Reference: [Table Name Validation](https://github.com/MyJetTools/my-no-sql-server/wiki/Table-Name-Validation).

### Where entities live and how they flow
- Entities are stored in `my-no-sql-server`.
- Reader API: `MyNoSqlDataReader<TEntity>` subscribes to the table and keeps the latest snapshot locally in the application for fast reads.
- Writer API: `MyNoSqlDataWriter<TEntity>` is used by writers to push inserts/updates/deletes to the server.
- Design entities so they are stable over time; breaking schema changes can break both readers (subscriptions) and writers (HTTP).

### Pattern 1: Table = Model
- Use `#[my_no_sql_entity("table-name")]` on a struct that represents a single table’s rows.
- The macro injects:
  - `partition_key: String`, `row_key: String`, `time_stamp: Timestamp` with proper Serde casing.
  - `MyNoSqlEntity` and `MyNoSqlEntitySerializer` impls (serialize/deserialize ready for readers/writers).
- Optional `with_expires = true` adds `expires: Timestamp` with Serde name `Expires` for TTL semantics.
- Derive serde and clone/debug as needed; apply `#[serde(rename_all = "...")]` for payload fields to keep consistent casing.
- Example:
  ```rust
  #[my_no_sql_macros::my_no_sql_entity("bidask-snapshots")]
  #[derive(Serialize, Deserialize, Debug, Clone)]
  #[serde(rename_all = "PascalCase")]
  pub struct BidAskSnapshot {
      pub unix_timestamp_with_milis: u64,
      pub bid: f64,
      pub ask: f64,
      pub base: String,
      pub quote: String,
  }
  ```
- Example with TTL:
  ```rust
  #[my_no_sql_macros::my_no_sql_entity("sessions", with_expires = true)]
  #[derive(Serialize, Deserialize, Clone)]
  pub struct SessionEntity {
      pub user_id: String,
      pub token: String,
      pub device: String,
      // `expires` is auto-added by the macro because with_expires = true
  }
  ```

### Pattern 2: PartitionKey + RowKey = Model (enum)
- Use when each `(PartitionKey, RowKey)` pair maps to a distinct model type.
- Declare an enum with `#[enum_of_my_no_sql_entity(table_name:"<table>", generate_unwraps)]`.
- For each variant model:
  - Annotate with `#[enum_model(partition_key:"...", row_key:"...")]`.
  - Derive serde and other traits required by your service.
- The macros generate:
  - Correct partition/row keys per variant.
  - Serialization helpers plus `unwrap_caseX` accessors when `generate_unwraps` is set.
- Example:
  ```rust
  #[enum_of_my_no_sql_entity(table_name:"Test", generate_unwraps)]
  pub enum MyNoSqlEnumEntityTest {
      Case1(Struct1),
      Case2(Struct2),
  }

  #[enum_model(partition_key:"pk1", row_key:"rk1")]
  #[derive(Serialize, Deserialize, Clone)]
  pub struct Struct1 {
      pub field1: String,
      pub field2: i32,
  }

  #[enum_model(partition_key:"pk2", row_key:"rk2")]
  #[derive(Serialize, Deserialize, Clone)]
  pub struct Struct2 {
      pub field3: String,
      pub field4: i32,
  }
  ```
- More elaborate enum example (mixed casing and TTL in one variant):
  ```rust
  #[enum_of_my_no_sql_entity(table_name:"notifications", generate_unwraps)]
  pub enum NotificationEntity {
      #[enum_model(partition_key:"email", row_key:"welcome")]
      EmailWelcome(EmailWelcomeModel),
      #[enum_model(partition_key:"push", row_key:"security")]
      PushSecurity(PushSecurityModel),
  }

  #[derive(Serialize, Deserialize, Clone)]
  #[serde(rename_all = "PascalCase")]
  pub struct EmailWelcomeModel {
      pub subject: String,
      pub body: String,
  }

  #[derive(Serialize, Deserialize, Clone)]
  #[serde(rename_all = "camelCase")]
  pub struct PushSecurityModel {
      pub title: String,
      pub message: String,
      pub severity: String,
  }
  ```

### Field and serde rules
- Do not rename payload fields to reserved serde names (`PartitionKey`, `RowKey`, `TimeStamp`, `Expires`); the macro enforces uniqueness.
- When adding an `expires` field manually, use type `Timestamp`; otherwise enable `with_expires`.
- Keep `LAZY_DESERIALIZATION` as the default (`false`) unless the macro adds support for a specialized flow.
- Prefer `#[serde(rename_all = "...")]` instead of renaming individual fields when possible.
- Avoid floats for keys; keep keys ASCII-safe and stable (no trailing slashes or whitespace).

### Testing guidance
- Validate serialization/deserialization paths using the examples under `my-no-sql-tests/src/macros_tests`.
- For enums, assert `unwrap_caseX` helpers and key selection per variant.
- Add unit tests per model that:
  - Serialize then deserialize and compare fields.
  - For enums, confirm correct variant after round-trip and that unwrap helpers work.
  - Validate `expires` presence when `with_expires = true`.

### When to choose each pattern
- Use **Table = Model** when all rows in a table share the same schema.
- Use **PartitionKey + RowKey = Model (enum)** when a table hosts heterogeneous payloads selected by keys.

### Key design guidance
- PartitionKey:
  - Group data that is frequently read together.
  - Keep length modest; avoid unbounded cardinality when possible.
- RowKey:
  - Unique within a partition.
  - Prefer stable identifiers (ids, symbols, timestamps encoded consistently).
- TimeStamp:
  - Auto-managed; used by server for last-write tracking and sync ordering.
  - **Important**: When creating a new entity instance, do not set the current timestamp manually. The server will automatically set the timestamp when the entity is written. Always initialize `time_stamp` with `Default::default()`.
- Expires (TTL):
  - Use for session-like or cache-like data; choose UTC epoch milliseconds.

### Best practices: constant keys
- When PartitionKey and/or RowKey are fixed for a table, expose them as `const` on the entity and initialize defaults with those constants. This keeps writers/readers aligned and avoids typos.
- Example:
  ```rust
  use serde::*;

  service_sdk::macros::use_my_no_sql_entity!();

  #[my_no_sql_entity("atr-settings")]
  #[derive(Serialize, Deserialize, Debug, Clone)]
  pub struct AtrSettingsEntity {
      pub percent: f64,
      pub candles_count: i32,
  }

  impl AtrSettingsEntity {
      pub const PARTITION_KEY: &'static str = "*";
      pub const ROW_KEY: &'static str = "*";
  }

  impl Default for AtrSettingsEntity {
      fn default() -> Self {
          Self {
              partition_key: Self::PARTITION_KEY.to_string(),
              row_key: Self::ROW_KEY.to_string(),
              time_stamp: Default::default(),
              percent: 0.8,
              candles_count: 365,
          }
      }
  }
  ```

### Best practices: timestamp initialization
- **Never set the current timestamp manually** when creating a `MyNoSqlEntity` instance. The server automatically assigns the timestamp when the entity is written or updated.
- Always initialize `time_stamp` with `Default::default()` in constructors, `Default` implementations, or when manually creating entity instances.
- Example - Complete initialization patterns:
  ```rust
  use my_no_sql_macros::my_no_sql_entity;
  use serde::{Deserialize, Serialize};

  #[my_no_sql_entity("user-profiles")]
  #[derive(Serialize, Deserialize, Debug, Clone)]
  pub struct UserProfileEntity {
      pub user_name: String,
      pub email: String,
      pub age: u32,
  }

  impl UserProfileEntity {
      // ✅ Correct: Helper function for creating new instances
      pub fn new(user_name: String, email: String, age: u32) -> Self {
          Self {
              partition_key: user_name.clone(),
              row_key: "profile".to_string(),
              time_stamp: Default::default(), // ✅ Server will set timestamp on write
              user_name,
              email,
              age,
          }
      }

      // ✅ Correct: Creating with specific keys
      pub fn create(user_id: &str, email: String, age: u32) -> Self {
          Self {
              partition_key: user_id.to_string(),
              row_key: "profile".to_string(),
              time_stamp: Default::default(), // ✅ Always use Default::default()
              user_name: user_id.to_string(),
              email,
              age,
          }
      }
  }

  impl Default for UserProfileEntity {
      fn default() -> Self {
          Self {
              partition_key: "default".to_string(),
              row_key: "profile".to_string(),
              time_stamp: Default::default(), // ✅ Correct: server will set timestamp
              user_name: "".to_string(),
              email: "".to_string(),
              age: 0,
          }
      }
  }

  // Usage examples:
  // ✅ Correct: Using Default
  let entity1 = UserProfileEntity::default();

  // ✅ Correct: Using helper function
  let entity2 = UserProfileEntity::new("john".to_string(), "john@example.com".to_string(), 30);

  // ✅ Correct: Manual initialization
  let entity3 = UserProfileEntity {
      partition_key: "user-123".to_string(),
      row_key: "profile".to_string(),
      time_stamp: Default::default(), // ✅ Always use Default::default()
      user_name: "john".to_string(),
      email: "john@example.com".to_string(),
      age: 30,
  };

  // ❌ WRONG: Never do this
  // let entity_wrong = UserProfileEntity {
  //     partition_key: "user-123".to_string(),
  //     row_key: "profile".to_string(),
  //     time_stamp: Timestamp::now(), // ❌ Server will overwrite this anyway
  //     user_name: "john".to_string(),
  //     email: "john@example.com".to_string(),
  //     age: 30,
  // };
  ```

### Best practices: meaningful keys as helpers
- When keys encode business meaning, provide helper functions to generate and to read them back, to avoid duplicating string literals across the codebase.
- Example:
  ```rust
  use serde::*;
  use trading_robot_common::CandleType;

  service_sdk::macros::use_my_no_sql_entity!();

  // PartitionKey: instrument_id
  // RowKey: interval ("1m", "5m", "1h", "1d", "1M")
  #[my_no_sql_entity("atr-values")]
  #[derive(Serialize, Deserialize, Debug, Clone)]
  pub struct AtrValueMyNoSqlEntity {
      pub value: f64,
  }

  impl AtrValueMyNoSqlEntity {
      pub fn generate_partition_key(instrument_id: &str) -> &str {
          instrument_id
      }

      pub fn generate_row_key(candle_type: CandleType) -> &'static str {
          candle_type.as_str()
      }

      pub fn get_instrument(&self) -> &str {
          &self.partition_key
      }

      pub fn get_candle_type(&self) -> CandleType {
          CandleType::from_str(&self.row_key)
      }
  }
  ```

### Macro options cheat sheet
- `#[my_no_sql_entity("table", with_expires = true)]` → adds `expires`.
- `#[enum_of_my_no_sql_entity(table_name:"table", generate_unwraps)]` → generates unwrap helpers per variant.
- `#[enum_model(partition_key:"...", row_key:"...")]` → binds a variant model to fixed keys.
- The macros auto-implement:
  - `MyNoSqlEntity` with `TABLE_NAME`, `LAZY_DESERIALIZATION = false`, `get_partition_key`, `get_row_key`, `get_time_stamp`.
  - `MyNoSqlEntitySerializer` with standard binary serialize/deserialize.

### Minimal working template (table = model)
```rust
use my_no_sql_macros::my_no_sql_entity;
use serde::{Deserialize, Serialize};

#[my_no_sql_entity("table-name")]
#[derive(Serialize, Deserialize, Clone)]
pub struct MyEntity {
    pub value: String,
    pub count: i32,
}
```

### Minimal working template (enum model)
```rust
use my_no_sql_macros::{enum_model, enum_of_my_no_sql_entity};
use serde::{Deserialize, Serialize};

#[enum_of_my_no_sql_entity(table_name:"events", generate_unwraps)]
pub enum EventEntity {
    Create(CreateEvent),
    Delete(DeleteEvent),
}

#[enum_model(partition_key:"pk-create", row_key:"rk-create")]
#[derive(Serialize, Deserialize, Clone)]
pub struct CreateEvent {
    pub id: String,
    pub payload: String,
}

#[enum_model(partition_key:"pk-delete", row_key:"rk-delete")]
#[derive(Serialize, Deserialize, Clone)]
pub struct DeleteEvent {
    pub id: String,
    pub reason: String,
}
```

### AI generation checklist
- Choose the pattern (single model vs enum).
- Set `table_name` accurately and keep it stable.
- Apply `#[serde(rename_all = "...")]` for payload fields.
- Never introduce fields named `PartitionKey`, `RowKey`, `TimeStamp`, `Expires` manually unless following macro requirements.
- For TTL needs, prefer `with_expires = true` over a custom `expires` unless special handling is required.
- **Always initialize `time_stamp` with `Default::default()`**—never set the current timestamp manually; the server handles timestamp assignment.
- Derive `Serialize`, `Deserialize`, and `Clone`; add `Debug` when useful for logs/tests.
- Provide unit tests that round-trip entities and assert variant unwraps.

### Anti-patterns to avoid
- Manually defining partition/row/timestamp fields when using the macros (causes duplication or serde conflicts).
- **Setting the current timestamp manually** when creating entity instances (e.g., `time_stamp: Timestamp::now()`). The server sets timestamps automatically; use `Default::default()` instead.
- Reusing the same `(partition_key, row_key)` for multiple variants in enum models.
- Using dynamic keys in enum models (keys must be compile-time constants in attributes).
- Introducing serde renames that clash with reserved casing tokens.
