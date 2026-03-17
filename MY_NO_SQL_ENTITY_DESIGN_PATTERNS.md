# Предлагаемые изменения к my-no-sql-entity-design-patterns.md

Этот документ описывает чистый my-no-sql-sdk — БЕЗ service-sdk.
Всё что связано с service-sdk (use_my_no_sql_entity!(), фичи service-sdk, ServiceContext) — отдельный документ.

## 1. Добавить раздел: Reader & Writer overview

После "Where entities live and how they flow" добавить полноценный раздел с API.

### Writer (my-no-sql-data-writer)

HTTP writer — отправляет запросы к MyNoSql server.

**Crate:** `my-no-sql-data-writer` (или через `my-no-sql-sdk` с feature `data-writer`)

#### MyNoSqlWriterSettings trait

```rust
#[async_trait::async_trait]
pub trait MyNoSqlWriterSettings {
    async fn get_url(&self) -> String;       // URL MyNoSql сервера (HTTP)
    fn get_app_name(&self) -> &'static str;
    fn get_app_version(&self) -> &'static str;
}
```

#### Создание writer

```rust
use my_no_sql_sdk::{
    abstractions::DataSynchronizationPeriod,
    data_writer::{CreateTableParams, MyNoSqlDataWriter},
};

let writer = MyNoSqlDataWriter::<InstrumentEntity>::new(
    settings_reader.clone(),   // Arc<dyn MyNoSqlWriterSettings + Send + Sync>
    Some(CreateTableParams {
        persist: true,                        // сохранять на диск
        max_partitions_amount: None,          // None = без лимита
        max_rows_per_partition_amount: None,
    }),
    DataSynchronizationPeriod::Immediately,   // как быстро reader увидит изменения
);
```

#### with_retries — ОБЯЗАТЕЛЬНО

**НИКОГДА** не вызывать методы на writer напрямую. Всегда через `.with_retries(N)`:

```rust
// ✅ CORRECT — через with_retries
let writer_with_retries = writer.with_retries(3);
writer_with_retries.insert_or_replace_entity(&entity).await.unwrap();

// ❌ WRONG — напрямую
writer.insert_or_replace_entity(&entity).await.unwrap();
```

Паттерн в AppContext:
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

**Важно:** Writer возвращает **owned T** (не Arc), обёрнутый в `Result<Option<T>, DataWriterError>`.

#### DataSynchronizationPeriod

| Value | Описание |
|---|---|
| `Immediately` | Reader увидит изменения ASAP |
| `Sec1` / `Sec5` / `Sec15` / `Sec30` | Задержка N секунд |
| `Min1` | 1 минута |
| `Asap` | Best-effort |

#### CreateTableParams

```rust
CreateTableParams {
    persist: true,   // true = переживает рестарт сервера
    max_partitions_amount: None,
    max_rows_per_partition_amount: None,
}
```

`Some(params)` — автоматически создать таблицу при первой записи. `None` — таблица должна существовать.

---

### Reader (my-no-sql-tcp-reader)

TCP reader — подписывается на таблицу, держит локальную копию. Чтения — локальные, без сетевых запросов.

**Crate:** `my-no-sql-tcp-reader` (или через `my-no-sql-sdk` с feature `data-reader`)

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
    None => { /* partition не найден */ }
}

// Get one entity → Option<Arc<T>>
let entity = reader.get_entity("partition_key", "row_key").await;
```

**CRITICAL:** `get_by_partition_key` возвращает `Option<Vec<(String, Arc<T>)>>` — **НЕ** `Vec<Arc<T>>`. Всегда деструктурировать tuple.

---

## 2. Добавить раздел: Shared entity crate

> Entities ДОЛЖНЫ жить в отдельном shared crate. Дублирование entity struct в нескольких проектах — антипаттерн.
> Reader (сервис) и writer (админка) импортируют один и тот же crate.

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

## 3. Добавить в "Common Mistakes" / "Anti-patterns"

| Ошибка | Исправление |
|---|---|
| Вызывать writer напрямую без retries | Всегда `writer.with_retries(3).method()` |
| Ожидать `Vec<Arc<T>>` от reader `get_by_partition_key` | Возвращает `Option<Vec<(String, Arc<T>)>>` |
| Ожидать `Option<Vec<Arc<T>>>` от writer `get_by_partition_key` | Writer возвращает `Result<Option<Vec<T>>>` — owned T, не Arc |
| Дублировать entity struct в нескольких проектах | Shared crate |
| Ставить `time_stamp: Timestamp::now()` | Всегда `time_stamp: Default::default()` |

---

## 4. Добавить раздел: Reader Change Callbacks

Когда сервис держит in-memory кэш на основе NoSql данных и должен реагировать на изменения — используй `MyNoSqlDataReaderCallBacks`.

### Trait

```rust
#[async_trait::async_trait]
pub trait MyNoSqlDataReaderCallBacks<TMyNoSqlEntity> {
    async fn inserted_or_replaced(&self, partition_key: &str, entities: Vec<LazyMyNoSqlEntity<TMyNoSqlEntity>>);
    async fn deleted(&self, partition_key: &str, entities: Vec<LazyMyNoSqlEntity<TMyNoSqlEntity>>);
}
```

### Паттерн: полная перезагрузка кэша

При любом event (insert/replace/delete) — полностью перечитать все данные из reader и заменить кэш. Не пытаться делать инкрементальные обновления.

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

// Скрипт перезагрузки — читает все из reader, заменяет кэш
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

### Регистрация — в main.rs после init, до start_application

```rust
use my_no_sql_sdk::reader::MyNoSqlDataReader;

app.my_entity_reader
    .assign_callback(Arc::new(MyEntityNoSqlCallback::new(app.clone())))
    .await;
```

### Правила

| Правило | Почему |
|---|---|
| **ALWAYS** `tokio::spawn` в callback | Callback не должен блокировать NoSql reader |
| Reload скрипт принимает `Arc<AppContext>` (owned) | Нужно для `tokio::spawn` — move semantics |
| **ALWAYS** полная перезагрузка, не инкрементальная | Проще, надёжнее, нет edge cases с порядком событий |
| Callback живёт в `scripts/` | Это не flow — нет HTTP/gRPC контекста |
| Кэш имеет метод `reload_all` | Очищает и заново заполняет из итератора |
