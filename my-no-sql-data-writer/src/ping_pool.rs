use std::{collections::HashMap, sync::Arc, time::Duration};

use flurl::body::FlUrlBody;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::{FlUrlFactory, MyNoSqlWriterSettings, WriterSession};

pub struct PingDataItem {
    pub name: &'static str,
    pub version: &'static str,

    pub table_settings: Vec<(
        String,
        Arc<dyn MyNoSqlWriterSettings + Send + Sync + 'static>,
        Arc<WriterSession>,
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

    pub fn register(
        &self,

        settings: Arc<dyn MyNoSqlWriterSettings + Send + Sync + 'static>,
        table: &str,
        session: Arc<WriterSession>,
    ) {
        let mut data = self.data.lock();
        if !data.started {
            tokio::spawn(async move { ping_loop().await });
            data.started = true;
        }

        let index = data.items.iter().position(|x| {
            x.name == settings.get_app_name() && x.version == settings.get_app_version()
        });

        if let Some(index) = index {
            let item = &mut data.items[index];
            item.table_settings.push((table.to_string(), settings, session));
        } else {
            let item = PingDataItem {
                name: settings.get_app_name(),
                version: settings.get_app_version(),

                table_settings: vec![(table.to_string(), settings, session)],
            };

            data.items.push(item);
        }
    }
}

struct PingSnapshotItem {
    name: &'static str,
    version: &'static str,
    table_settings: Vec<(
        String,
        Arc<dyn MyNoSqlWriterSettings + Send + Sync + 'static>,
        Arc<WriterSession>,
    )>,
}

async fn ping_loop() {
    let delay = Duration::from_secs(30);
    loop {
        tokio::time::sleep(delay).await;

        let snapshot: Vec<PingSnapshotItem> = {
            let access = crate::PING_POOL.data.lock();
            access
                .items
                .iter()
                .map(|itm| PingSnapshotItem {
                    name: itm.name,
                    version: itm.version,
                    table_settings: itm.table_settings.clone(),
                })
                .collect()
        };

        for itm in snapshot {
            // All writers of the same app instance that target the same server
            // share a single session, so we ping once per url and keep the list
            // of session holders to update them all with whatever id the server
            // returns.
            let mut url_to_ping: HashMap<
                String,
                (
                    Arc<dyn MyNoSqlWriterSettings + Send + Sync + 'static>,
                    Vec<String>,
                    Vec<Arc<WriterSession>>,
                ),
            > = HashMap::new();

            for (table, settings, session) in itm.table_settings.iter() {
                let url = settings.get_url().await;
                let entry = url_to_ping
                    .entry(url)
                    .or_insert_with(|| (settings.clone(), Vec::new(), Vec::new()));
                entry.1.push(table.to_string());
                entry.2.push(session.clone());
            }

            for (_, (settings, tables, sessions)) in url_to_ping {
                // The session id is issued once (on the first ping, when we have
                // none) and then kept for the whole lifetime of the process. We
                // keep replaying the same id even after a server restart/GC: the
                // server re-adopts that exact id instead of us re-keying. Only
                // the very first ping goes out without a `session` header.
                let existing = sessions.iter().find_map(|itm| itm.get());

                let header_session = Arc::new(WriterSession::new());
                if let Some(id) = &existing {
                    header_session.set(id.as_ref().clone());
                }

                let factory = FlUrlFactory::new(settings, None, "", header_session);

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

                let mut fl_url_response = match fl_url_response {
                    Ok(response) => response,
                    Err(err) => {
                        println!("{}:{} ping error: {:?}", itm.name, itm.version, err);
                        continue;
                    }
                };

                // Resolve the id this group should hold: keep the one we already
                // have, otherwise adopt the one the server just issued on this
                // first ping. Old servers return no session field -> nothing to
                // store, and we keep pinging without a header.
                let resolved = match existing {
                    Some(id) => Some(id.as_ref().clone()),
                    None => read_session_from_response(&mut fl_url_response).await,
                };

                if let Some(id) = resolved {
                    // Populate holders that don't have the id yet (the first
                    // ping, or writers registered after the id was issued).
                    // Never overwrite an id we already hold.
                    for session in &sessions {
                        if session.get().is_none() {
                            session.set(id.clone());
                        }
                    }
                }
            }
        }
    }
}

async fn read_session_from_response(
    response: &mut flurl::FlUrlResponse,
) -> Option<String> {
    let body = response.get_body_as_slice().await.ok()?;

    if body.is_empty() {
        return None;
    }

    let parsed: PingResponseModel = serde_json::from_slice(body).ok()?;

    parsed.session.filter(|session| !session.is_empty())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingModel {
    pub name: String,
    pub version: String,
    pub tables: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PingResponseModel {
    pub session: Option<String>,
}
