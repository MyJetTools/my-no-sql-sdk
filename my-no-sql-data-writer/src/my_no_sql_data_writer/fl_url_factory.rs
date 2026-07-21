use std::sync::Arc;

use flurl::FlUrl;

use rust_extensions::UnsafeValue;

use super::{CreateTableParams, DataWriterError, MyNoSqlWriterSettings};

#[derive(Clone)]
pub struct FlUrlFactory {
    settings: Arc<dyn MyNoSqlWriterSettings + Send + Sync + 'static>,
    auto_create_table_params: Option<Arc<CreateTableParams>>,

    #[cfg(all(unix, feature = "with-ssh"))]
    pub ssh_security_credentials_resolver:
        Option<Arc<dyn flurl::my_ssh::ssh_settings::SshSecurityCredentialsResolver + Send + Sync>>,

    create_table_is_called: Arc<UnsafeValue<bool>>,
    table_name: &'static str,
    mode: flurl::FlUrlMode,
}

impl FlUrlFactory {
    pub fn new(
        settings: Arc<dyn MyNoSqlWriterSettings + Send + Sync + 'static>,
        auto_create_table_params: Option<Arc<CreateTableParams>>,
        table_name: &'static str,
    ) -> Self {
        Self {
            auto_create_table_params,

            create_table_is_called: UnsafeValue::new(false).into(),
            settings,
            table_name,
            // HTTP/2 multiplexes all our requests over a single connection per
            // endpoint. Servers which do not speak h2 can be handled with use_h1().
            mode: flurl::FlUrlMode::H2,

            #[cfg(all(unix, feature = "with-ssh"))]
            ssh_security_credentials_resolver: None,
        }
    }

    /// Falls back to HTTP/1.1 - for MyNoSqlServer instances which do not support HTTP/2.
    pub fn use_h1(&mut self) {
        self.mode = flurl::FlUrlMode::Http1Hyper;
    }

    pub fn get_settings(&self) -> &Arc<dyn MyNoSqlWriterSettings + Send + Sync + 'static> {
        &self.settings
    }

    async fn create_fl_url(&self, url: &str) -> FlUrl {
        // Replay this process's session id on every request so the server can
        // attribute all of our traffic (data requests and the Ping handshake
        // alike) to a single writer.
        let fl_url = flurl::FlUrl::new(url)
            .update_mode(self.mode)
            .with_header("session", super::get_writer_session_id());

        #[cfg(all(unix, feature = "with-ssh"))]
        if let Some(ssh_security_credentials_resolver) = &self.ssh_security_credentials_resolver {
            return fl_url
                .set_ssh_security_credentials_resolver(ssh_security_credentials_resolver.clone());
        }

        fl_url
    }

    pub async fn get_fl_url(&self) -> Result<(FlUrl, String), DataWriterError> {
        let url = self.settings.get_url().await;
        if !self.create_table_is_called.get_value() {
            if let Some(crate_table_params) = &self.auto_create_table_params {
                self.create_table_if_not_exists(url.as_str(), crate_table_params)
                    .await?;
            }

            self.create_table_is_called.set_value(true);
        }

        let result = self.create_fl_url(url.as_str()).await;

        Ok((result, url))
    }

    pub async fn create_table_if_not_exists(
        &self,
        url: &str,
        create_table_params: &CreateTableParams,
    ) -> Result<(), DataWriterError> {
        let fl_url = self.create_fl_url(url).await;
        super::execution::create_table_if_not_exists(
            fl_url,
            url,
            self.table_name,
            create_table_params,
            my_no_sql_abstractions::DataSynchronizationPeriod::Sec1,
        )
        .await
    }
}
