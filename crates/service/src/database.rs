use crate::{Error, Result, config::Config};
use deadpool::managed::{Manager, Metrics, Pool, RecycleError, RecycleResult};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_postgres::NoTls;
use treeharbor_metadata::Metadata;

pub struct Session {
    pub metadata: Metadata,
    backend_pid: i32,
    _driver: Driver,
}
impl Session {
    /// Diagnostics for this connection only; not a request-controlled query.
    pub fn backend_pid(&self) -> i32 {
        self.backend_pid
    }
}
struct Driver(JoinHandle<()>);
impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}
#[derive(Clone)]
pub struct Database {
    config: Config,
}
impl Database {
    pub fn new(config: Config) -> Result<Self> {
        config.database_config()?;
        Ok(Self { config })
    }
    /// Administration may connect before schema installation; serving cannot.
    pub async fn connect(&self, check_schema: bool) -> Result<Session> {
        let config = self.config.database_config()?;
        let (client, connection) = config.connect(NoTls).await.map_err(|_| Error::Database)?;
        let driver = Driver(tokio::spawn(async move {
            let _ = connection.await;
        }));
        let setup = async {
            client
                .query_one(
                    "SELECT set_config('search_path',$1,false)",
                    &[&self.config.schema],
                )
                .await
                .map_err(|_| Error::Database)?;
            client
                .batch_execute(
                    "SET statement_timeout='15s'; SET idle_in_transaction_session_timeout='10s'",
                )
                .await
                .map_err(|_| Error::Database)?;
            let backend_pid: i32 = client
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .map_err(|_| Error::Database)?
                .get(0);
            let metadata = Metadata::new(client).await.map_err(|_| Error::Database)?;
            if check_schema {
                metadata.check_schema().await.map_err(|_| Error::Database)?;
            }
            // A connection's process ID is stable until it closes.
            Ok((metadata, backend_pid))
        }
        .await;
        match setup {
            Ok((metadata, backend_pid)) => Ok(Session {
                metadata,
                backend_pid,
                _driver: driver,
            }),
            Err(error) => Err(error),
        }
    }
    pub fn pool(&self) -> Result<Pool<Self>> {
        Pool::builder(self.clone())
            .max_size(self.config.pool_size)
            .runtime(deadpool::Runtime::Tokio1)
            .wait_timeout(Some(Duration::from_secs(2)))
            .create_timeout(Some(Duration::from_secs(5)))
            .recycle_timeout(Some(Duration::from_secs(2)))
            .build()
            .map_err(|_| Error::Config)
    }
}
impl Manager for Database {
    type Type = Session;
    type Error = Error;
    async fn create(&self) -> Result<Session> {
        self.connect(true).await
    }
    async fn recycle(&self, session: &mut Session, _metrics: &Metrics) -> RecycleResult<Error> {
        session
            .metadata
            .ping()
            .await
            .map_err(|_| RecycleError::Backend(Error::Database))
    }
}
