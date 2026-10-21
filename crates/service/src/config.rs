use crate::{Error, Result, files};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_postgres::{Config as PgConfig, config::Host};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub database_url: String,
    #[serde(default = "default_schema")]
    pub schema: String,
    pub socket: PathBuf,
    #[serde(default = "default_pool")]
    pub pool_size: usize,
    #[serde(default = "default_connections")]
    pub max_connections: usize,
    #[serde(default = "default_timeout")]
    pub request_timeout_ms: u64,
}
fn default_schema() -> String {
    "public".into()
}
fn default_pool() -> usize {
    8
}
fn default_connections() -> usize {
    64
}
fn default_timeout() -> u64 {
    15_000
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = serde_json::from_slice(&files::read_private(path, 16 * 1024)?)
            .map_err(|_| Error::Config)?;
        config.database_config()?;
        Ok(config)
    }
    pub fn database_config(&self) -> Result<PgConfig> {
        if !(1..=64).contains(&self.pool_size)
            || !(1..=1024).contains(&self.max_connections)
            || !(100..=60_000).contains(&self.request_timeout_ms)
            || !self.socket.is_absolute()
            || self.socket.file_name().is_none()
            || self.schema.is_empty()
            || self.schema.starts_with("pg_")
            || self.schema == "information_schema"
            || self.schema.len() > 63
            || !self
                .schema
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
            || !self.schema.as_bytes()[0].is_ascii_lowercase()
        {
            return Err(Error::Config);
        }
        let mut pg: PgConfig = self.database_url.parse().map_err(|_| Error::Config)?;
        if pg.get_hosts().len() != 1 || !pg.get_hostaddrs().is_empty() || pg.get_options().is_some()
        {
            return Err(Error::Config);
        }
        match &pg.get_hosts()[0] {
            Host::Unix(path) if path.is_absolute() => {}
            _ => return Err(Error::Config),
        }
        pg.connect_timeout(Duration::from_secs(5))
            .application_name("treeharbor");
        Ok(pg)
    }
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms)
    }
}
