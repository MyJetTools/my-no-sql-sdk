# MyNoSqlServer Rust SDK

Rust client SDK and shared core for **MyNoSqlServer** — an in-memory NoSql server where data is written over HTTP and read over TCP (each client keeps a live local copy of the tables it subscribes to).

This is a cargo workspace of 9 crates. Application code normally depends on **one** of them — `my-no-sql-sdk` — and turns on the features it needs.

---

## Packages

| Crate | What it is | Use it when |
|---|---|---|
| **my-no-sql-sdk** | Facade — re-exports all the other crates behind cargo features | Always. This is the crate your service depends on |
| **my-no-sql-abstractions** | `MyNoSqlEntity`, `MyNoSqlEntitySerializer`, `DataSynchronizationPeriod`, `Timestamp` | Pulled in automatically; depend on it directly only in a crate that must stay dependency-light |
| **my-no-sql-macros** | Proc-macros: `my_no_sql_entity`, `enum_of_my_no_sql_entity`, `enum_model` | Defining entities. Enable via feature `macros` |
| **my-no-sql-data-writer** | HTTP writer — insert / replace / delete / read against the server REST API | The service writes data. Feature `data-writer` |
| **my-no-sql-tcp-reader** | TCP reader — subscribes to a table and keeps an in-memory copy; reads are local and synchronous | The service reads data. Feature `data-reader` |
| **my-no-sql-tcp-shared** | TCP protocol contracts, serializer, payload compression, sync-to-main-node handler | Rarely direct — it is what the reader and the server speak. Feature `tcp-contracts` |
| **my-no-sql-core** | The data model itself: `DbTable`, `DbPartition`, `DbRow`, JSON entity parsing, entity serializer | Always present (re-exported as `my_no_sql_sdk::core`) |
| **my-no-sql-server-core** | `DbInstance` / `DbTable` wrappers and table snapshots used by server-side nodes | You are building a MyNoSql master node or read node. Features `master-node` / `read-node` |
| **my-no-sql-tests** | Internal integration tests for the macros and serializers | Never — not part of the public API |

Dependency direction:

```
my-no-sql-sdk
 ├── my-no-sql-abstractions      (entity traits)
 ├── my-no-sql-core ─────────────┐
 ├── my-no-sql-macros            │
 ├── my-no-sql-data-writer       │ (HTTP, via flurl)
 ├── my-no-sql-tcp-reader ── my-no-sql-tcp-shared
 └── my-no-sql-server-core ──────┘
```

---

## Adding to a project

The repo is released as a single repo-wide git tag; every crate in it carries the same version. Pin the tag:

```toml
[dependencies]
my-no-sql-sdk = { tag = "0.5.1", git = "https://github.com/MyJetTools/my-no-sql-sdk.git", features = [
    "macros",
    "data-writer",
    "data-reader",
] }
serde = { version = "*", features = ["derive"] }
```

### Features of `my-no-sql-sdk`

| Feature | Enables | Re-exported as |
|---|---|---|
| `macros` | entity proc-macros | `my_no_sql_sdk::macros` |
| `data-writer` | HTTP writer | `my_no_sql_sdk::data_writer` |
| `data-reader` | TCP reader | `my_no_sql_sdk::reader` |
| `tcp-contracts` | raw TCP contracts | `my_no_sql_sdk::tcp_contracts` |
| `master-node` | server-side model + per-table row compression (zstd) | `my_no_sql_sdk::server` |
| `read-node` | server-side model for a read node | `my_no_sql_sdk::server` |
| `with-ssh` | writer can reach the server through an SSH tunnel | — |
| `debug_db_row` | extra `DbRow` diagnostics | — |

`my_no_sql_sdk::core` and `my_no_sql_sdk::abstractions` are always available.

---

## Defining entities

Entities belong in a **separate shared crate** that both the writing service and the reading service depend on — never copy-paste the struct.

```rust
use my_no_sql_sdk::macros::my_no_sql_entity;
use serde::{Deserialize, Serialize};

#[my_no_sql_entity("instruments")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstrumentEntity {
    pub name: String,
    pub digits: i32,
}
```

The macro adds `partition_key`, `row_key` and `time_stamp` fields and implements `MyNoSqlEntity` + `MyNoSqlEntitySerializer`:

```rust
let entity = InstrumentEntity {
    partition_key: "instruments".to_string(),
    row_key: "EURUSD".to_string(),
    time_stamp: Default::default(),   // always Default — never "now"
    name: "Euro vs Dollar".to_string(),
    digits: 5,
};
```

Auto-expiring entities add an `expires: Timestamp` field:

```rust
#[my_no_sql_entity(table_name: "sessions", with_expires: true)]
```

One table can also hold several shapes at once — `enum_of_my_no_sql_entity` on the enum, `enum_model` on each case,
which fixes that case's partition key (and optionally row key). Working example:
[my-no-sql-tests/src/macros_tests/enum_test.rs](my-no-sql-tests/src/macros_tests/enum_test.rs).

---

## Writing data

Implement the settings trait (usually on your `SettingsReader`):

```rust
#[async_trait::async_trait]
impl MyNoSqlWriterSettings for SettingsReader {
    async fn get_url(&self) -> String { self.get_my_no_sql_url().await }
    fn get_app_name(&self) -> &'static str { crate::APP_NAME }
    fn get_app_version(&self) -> &'static str { crate::APP_VERSION }
}
```

Build the writer:

```rust
use my_no_sql_sdk::abstractions::DataSynchronizationPeriod;
use my_no_sql_sdk::data_writer::MyNoSqlDataWriter;

let writer = MyNoSqlDataWriter::<InstrumentEntity>::create_with_builder(settings.clone())
    .set_sync_period(DataSynchronizationPeriod::Immediately)
    .persist_table(true)
    .build();
```

Builder options: `set_sync_period`, `persist_table`, `set_max_partitions_amount`,
`set_max_row_per_partitions_amount`, `do_not_auto_create_table`, `use_h1`.
By default the table is auto-created on the first request.

**Always go through `with_retries`** — the writer itself does not retry:

```rust
let w = writer.with_retries(3);

w.insert_or_replace_entity(&entity).await?;
w.bulk_insert_or_replace(&entities).await?;

let one = w.get_entity("instruments", "EURUSD", None).await?;      // Result<Option<T>>
let part = w.get_by_partition_key("instruments", None).await?;     // Result<Option<Vec<T>>>

w.delete_row("instruments", "EURUSD").await?;
w.delete_partitions(&["instruments"]).await?;

// Bulk delete: PartitionKey -> RowKeys
let mut rows_to_delete = std::collections::BTreeMap::new();
rows_to_delete.insert("instruments".to_string(), vec!["EURUSD".to_string()]);
w.bulk_delete(&rows_to_delete).await?;
```

The writer returns **owned** entities (`Result<Option<T>, DataWriterError>`), unlike the reader which hands out `Arc<T>`.

### HTTP/2

Since the h2 switch the writer talks **HTTP/2 by default** — one multiplexed connection per endpoint. Against a server that does not speak h2, ask for HTTP/1.1 explicitly:

```rust
let writer = MyNoSqlDataWriter::<InstrumentEntity>::create_with_builder(settings)
    .use_h1()
    .build();
```

The 30-second background ping loop follows the same choice.

---

## Reading data

One TCP connection serves any number of tables; every `get_reader::<T>()` subscribes to `T::TABLE_NAME`.

```rust
use my_no_sql_sdk::reader::MyNoSqlTcpConnection;

// settings: Arc<dyn MyNoSqlTcpConnectionSettings> — async fn get_host_port() -> String
let connection = MyNoSqlTcpConnection::new(crate::APP_NAME, settings.clone());

let instruments = connection.get_reader::<InstrumentEntity>();

connection.start().await;                          // create the readers first, then start
instruments.wait_until_first_data_arrives().await; // the only async read-side call
```

Reads are served from the local copy — synchronous and lock-free, no `.await`:

```rust
let one = instruments.get_entity("instruments", "EURUSD");         // Option<Arc<T>>
let map = instruments.get_by_partition_key("instruments");         // Option<BTreeMap<String, Arc<T>>>
let vec = instruments.get_by_partition_key_as_vec("instruments");  // Option<Vec<Arc<T>>>
let all = instruments.get_table_snapshot_as_vec();                 // Option<Vec<Arc<T>>>
```

To react to changes, assign a `MyNoSqlDataReaderCallBacks` implementation — see the callbacks section of
[MY_NO_SQL_ENTITY_DESIGN_PATTERNS.md](MY_NO_SQL_ENTITY_DESIGN_PATTERNS.md#5-reader-change-callbacks).

Feature `mocks` on `my-no-sql-tcp-reader` provides `MyNoSqlDataReaderMock` for unit tests.

---

## Server-side crates

`my-no-sql-core` and `my-no-sql-server-core` are the data model the server itself runs on: `DbInstance` (table registry),
`DbTable` / `DbPartition` / `DbRow`, and `db_snapshots` for persistence and replication. A service that only reads and
writes data never touches them directly — they are enabled by `master-node` / `read-node` when building a node.

With `master-node`, `my-no-sql-core` additionally compresses rows in memory per table (zstd); client readers do not pull that in.

---

## Building

```bash
cargo check --workspace --all-targets
cargo test --workspace
cargo check -p my-no-sql-data-writer --features with-ssh   # unix-only feature
```

## Releasing

All crates share one version, and the repo is tagged once (`0.4.2`, `0.5.1`, …) — downstream projects pin that tag.
Bump `version` in every `Cargo.toml` of the workspace, commit, then tag `main`.

## Further reading

- [MY_NO_SQL_ENTITY_DESIGN_PATTERNS.md](MY_NO_SQL_ENTITY_DESIGN_PATTERNS.md) — entity patterns, reader/writer API details, common mistakes
