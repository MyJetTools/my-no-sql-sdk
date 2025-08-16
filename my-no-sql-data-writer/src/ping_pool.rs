use std::{collections::HashMap, sync::Arc, time::Duration};

use flurl::body::FlUrlBody;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{FlUrlFactory, MyNoSqlWriterSettings};

pub struct PingDataItem {
    pub name: &'static str,
    pub version: &'static str,

    pub table_settings: Vec<(
        String,
        Arc<dyn MyNoSqlWriterSettings + Send + Sync + 'static>,
    )>,
}

pub struct PingPoolInner {
    items: Vec<PingDataItem>,
    started: bool,
}

impl PingPoolInner {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            started: false,
        }
    }
}

pub struct PingPool {
    data: Mutex<PingPoolInner>,
}

impl PingPool {
    pub fn new() -> Self {
        Self {
            data: Mutex::new(PingPoolInner::new()),
        }
    }

    pub async fn register(
        &self,

        settings: Arc<dyn MyNoSqlWriterSettings + Send + Sync + 'static>,
        table: &str,
    ) {
        let mut data = self.data.lock().await;
        if !data.started {
            tokio::spawn(async move { ping_loop().await });
            data.started = true;
        }

        let index = data.items.iter().position(|x| {
            x.name == settings.get_app_name() && x.version == settings.get_app_version()
        });

        if let Some(index) = index {
            let item = &mut data.items[index];
            item.table_settings.push((table.to_string(), settings));
        } else {
            let item = PingDataItem {
                name: settings.get_app_name(),
                version: settings.get_app_version(),

                table_settings: vec![((table.to_string(), settings))],
            };

            data.items.push(item);
        }
    }
}

async fn ping_loop() {
    let delay = Duration::from_secs(30);
    loop {
        tokio::time::sleep(delay).await;

        let access = crate::PING_POOL.data.lock().await;

        for itm in access.items.iter() {
            let mut url_to_ping = HashMap::new();
            for (table, settings) in itm.table_settings.iter() {
                let url = settings.get_url().await;
                let entry = url_to_ping
                    .entry(url)
                    .or_insert_with(|| ((settings.clone(), Vec::new())));
                entry.1.push(table.to_string());
            }

            for (_, (settings, tables)) in url_to_ping {
                let factory = FlUrlFactory::new(settings, None, "");

                let ping_model = PingModel {
                    name: itm.name.to_string(),
                    version: itm.version.to_string(),
                    tables,
                };

                let fl_url = factory.get_fl_url().await;

                if let Err(err) = &fl_url {
                    println!("{}:{} ping error: {:?}", itm.name, itm.version, err);
                    continue;
                }

                let fl_url_response = fl_url
                    .unwrap()
                    .0
                    .with_retries(3)
                    .append_path_segment("api")
                    .append_path_segment("ping")
                    .post(FlUrlBody::as_json(&ping_model))
                    .await;

                if let Err(err) = &fl_url_response {
                    println!("{}:{} ping error: {:?}", itm.name, itm.version, err);
                    continue;
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingModel {
    pub name: String,
    pub version: String,
    pub tables: Vec<String>,
}
