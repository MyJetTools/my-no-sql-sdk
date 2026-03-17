## 1. Reader & Writer overview

After "Where entities live and how they flow" add a full section with API.

### Writer (my-no-sql-data-writer)

HTTP writer — sends requests to MyNoSql server.

**Crate:** `my-no-sql-data-writer` (or via `my-no-sql-sdk` with feature `data-writer`)

#### MyNoSqlWriterSettings trait

```rust
#[async_trait::async_trait]
pub trait MyNoSqlWriterSettings {
    async fn get_url(&self) -> String;       // MyNoSql server URL (HTTP)
    fn get_app_name(&self) -> &'static str;
    fn get_app_version(&self) -> &'static str;
}
```

#### Creating a writer

```rust
use my_no_sql_sdk::{
    abstractions::DataSynchronizationPeriod,
    data_writer::{CreateTableParams, MyNoSqlDataWriter},
};

let writer = MyNoSqlDataWriter::<InstrumentEntity>::new(
    settings_reader.clone(),   // Arc<dyn MyNoSqlWriterSettings + Send + Sync>
    Some(CreateTableParams {
        persist: true,                        // persist to disk
        max_partitions_amount: None,          // None = no limit
        max_rows_per_partition_amount: None,
    }),
    DataSynchronizationPeriod::Immediately,   // how fast reader sees changes
);
```

#### with_retries — REQUIRED

**NEVER** call methods on writer directly. Always via `.with_retries(N)`:

```rust
// ✅ CORRECT — via with_retries
let writer_with_retries = writer.with_retries(3);
writer_with_retries.insert_or_replace_entity(&entity).await.unwrap();

// ❌ WRONG — directly
writer.insert_or_replace_entity(&entity).await.unwrap();
```

AppContext pattern:
```rust
pub struct AppContext {
    instruments: MyNoSqlDataWriter<InstrumentEntity>,
}

impl AppContext {
    pub fn get_instruments(&self) -> MyNoSqlDataWriterWithRetries<InstrumentEntity> {
        self.instruments.with_retries(3)
    }
}
```

#### Writer API (MyNoSqlDataWriterWithRetries)

```rust
let w = app_ctx.get_instruments();

// Insert or replace
w.insert_or_replace_entity(&entity).await.unwrap();

// Bulk insert or replace
w.bulk_insert_or_replace(&entities).await.unwrap();

// Get one entity → Result<Option<T>>
let entity = w.get_entity("partition_key", "row_key", None).await.unwrap();

// Get all in partition → Result<Option<Vec<T>>>
let items = w.get_by_partition_key("pk", None).await.unwrap().unwrap_or_default();

// Delete row
w.delete_row("partition_key", "row_key").await.unwrap();
```

**Important:** Writer returns **owned T** (not Arc), wrapped in `Result<Option<T>, DataWriterError>`.

#### DataSynchronizationPeriod

| Value | Description |
|---|---|
| `Immediately` | Reader sees changes ASAP |
| `Sec1` / `Sec5` / `Sec15` / `Sec30` | Delay N seconds |
| `Min1` | 1 minute |
| `Asap` | Best-effort |

#### CreateTableParams

```rust
CreateTableParams {
    persist: true,   // true = survives server restart
    max_partitions_amount: None,
    max_rows_per_partition_amount: None,
}
```

`Some(params)` — auto-create table on first write. `None` — table must already exist.

---

### Reader (my-no-sql-tcp-reader)

TCP reader — subscribes to a table, keeps a local copy. Reads are local, no network requests.

**Crate:** `my-no-sql-tcp-reader` (or via `my-no-sql-sdk` with feature `data-reader`)

#### Reader API (MyNoSqlDataReaderTcp)

```rust
// Get all in partition → Option<Vec<(String, Arc<T>)>>
// Tuple: (row_key, entity)
let items = reader.get_by_partition_key("pk").await;

match items {
    Some(entities) => {
        for (row_key, entity) in entities {
            // entity: Arc<T>
        }
    }
    None => { /* partition not found */ }
}

// Get one entity → Option<Arc<T>>
let entity = reader.get_entity("partition_key", "row_key").await;
```

**CRITICAL:** `get_by_partition_key` returns `Option<Vec<(String, Arc<T>)>>` — **NOT** `Vec<Arc<T>>`. Always destructure the tuple.

---

## 2. Shared entity crate

> Entities MUST live in a separate shared crate. Duplicating entity structs across multiple projects is an anti-pattern.
> Reader (service) and writer (admin) import the same crate.

```
my-no-sql-entities/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── instrument.rs
    └── ...
```

```toml
# my-no-sql-entities/Cargo.toml
[dependencies]
my-no-sql-sdk = { ..., features = ["macros"] }
serde = { version = "*", features = ["derive"] }
```

```toml
# Consuming service Cargo.toml
my-no-sql-entities = { path = "../my-no-sql-entities" }
```

---

## 3. Common Mistakes / Anti-patterns

| Mistake | Fix |
|---|---|
| Calling writer directly without retries | Always `writer.with_retries(3).method()` |
| Expecting `Vec<Arc<T>>` from reader `get_by_partition_key` | Returns `Option<Vec<(String, Arc<T>)>>` |
| Expecting `Option<Vec<Arc<T>>>` from writer `get_by_partition_key` | Writer returns `Result<Option<Vec<T>>>` — owned T, not Arc |
| Duplicating entity struct across multiple projects | Shared crate |
| Setting `time_stamp: Timestamp::now()` | Always `time_stamp: Default::default()` |

---

## 4. Reader Change Callbacks

When a service maintains an in-memory cache based on NoSql data and must react to changes — use `MyNoSqlDataReaderCallBacks`.

### Trait

```rust
#[async_trait::async_trait]
pub trait MyNoSqlDataReaderCallBacks<TMyNoSqlEntity> {
    async fn inserted_or_replaced(&self, partition_key: &str, entities: Vec<LazyMyNoSqlEntity<TMyNoSqlEntity>>);
    async fn deleted(&self, partition_key: &str, entities: Vec<LazyMyNoSqlEntity<TMyNoSqlEntity>>);
}
```

### Pattern: full cache reload

On any event (insert/replace/delete) — fully re-read all data from the reader and replace the cache. Do not attempt incremental updates.

```rust
use std::sync::Arc;
use my_no_sql_entities::MyEntityNoSqlEntity;
use my_no_sql_sdk::reader::{LazyMyNoSqlEntity, MyNoSqlDataReaderCallBacks};

pub struct MyEntityNoSqlCallback {
    app: Arc<AppContext>,
}

impl MyEntityNoSqlCallback {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

#[async_trait::async_trait]
impl MyNoSqlDataReaderCallBacks<MyEntityNoSqlEntity> for MyEntityNoSqlCallback {
    async fn inserted_or_replaced(
        &self,
        _partition_key: &str,
        _entities: Vec<LazyMyNoSqlEntity<MyEntityNoSqlEntity>>,
    ) {
        tokio::spawn(reload_my_entities(self.app.clone()));
    }

    async fn deleted(
        &self,
        _partition_key: &str,
        _entities: Vec<LazyMyNoSqlEntity<MyEntityNoSqlEntity>>,
    ) {
        tokio::spawn(reload_my_entities(self.app.clone()));
    }
}

// Reload script — reads all from reader, replaces cache
pub async fn reload_my_entities(app: Arc<AppContext>) {
    let items = app.my_entity_reader
        .get_by_partition_key(MyEntityNoSqlEntity::PARTITION_KEY)
        .await;

    let mut cache = app.cache.lock().await;
    match items {
        Some(entities) => cache.reload_all(entities.values().map(|e| e.as_ref().into())),
        None => cache.reload_all(std::iter::empty()),
    }
}
```

### Registration — in main.rs after init, before start_application

```rust
use my_no_sql_sdk::reader::MyNoSqlDataReader;

app.my_entity_reader
    .assign_callback(Arc::new(MyEntityNoSqlCallback::new(app.clone())))
    .await;
```

### Rules (in case of full reload to local cache)

| Rule | Why |
|---|---|
| **ALWAYS** `tokio::spawn` in callback | Callback must not block the NoSql reader |
| Reload script takes `Arc<AppContext>` (owned) | Required for `tokio::spawn` — move semantics |
| **ALWAYS** full reload, not incremental | Simpler, more reliable, no edge cases with event ordering |
| Callback lives in `scripts/` | Not a flow — no HTTP/gRPC context |
| Cache must have a `reload_all` method | Clears and re-populates from an iterator |
