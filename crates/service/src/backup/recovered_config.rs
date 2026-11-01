//! Configuration for an explicitly fenced, quarantined local deployment.
use super::*;
use std::os::unix::ffi::OsStrExt;

pub(super) async fn check_quarantine(tx: &Transaction<'_>) -> Result<()> {
    // A journal/receipt is historical evidence, never a substitute for these
    // current predicates. Existing immutable observations and API tasks remain.
    let unsafe_state: bool = tx.query_one(r#"SELECT
        EXISTS(SELECT 1 FROM th_access_tokens WHERE revoked_at IS NULL)
        OR EXISTS(SELECT 1 FROM th_principals WHERE kind='node' AND enabled)
        OR EXISTS(SELECT 1 FROM th_workspaces WHERE delivery='files' AND state NOT IN ('draining','deleted'))
        OR EXISTS(SELECT 1 FROM th_workspace_preparations WHERE state IN ('reserved','prepared'))
        OR EXISTS(SELECT 1 FROM th_managed_runs WHERE NOT removed AND NOT stop_requested)
        OR EXISTS(SELECT 1 FROM th_native_queue WHERE state IN ('queued','preparing','checking'))
        OR EXISTS(SELECT 1 FROM th_native_candidates WHERE state IN ('preparing','checking'))
        OR EXISTS(SELECT 1 FROM th_github_bindings WHERE state='active')
        OR EXISTS(SELECT 1 FROM th_github_event_channels WHERE state='active')"#, &[])
        .await.map_err(|_| Error::Database)?.get(0);
    if unsafe_state {
        return Err(Error::Unsafe);
    }
    Ok(())
}

pub(super) struct Prepared {
    root: PathBuf,
    config: Value,
    database: String,
    schema: String,
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn connection_string(pg: &tokio_postgres::Config, user: &str) -> Result<String> {
    let Host::Unix(host) = &pg.get_hosts()[0] else {
        return Err(Error::Unsafe);
    };
    let host = host.to_str().ok_or(Error::Unsafe)?;
    let database = pg.get_dbname().ok_or(Error::Unsafe)?;
    let mut text = format!(
        "host={} port={} dbname={} user={}",
        quote(host),
        pg.get_ports().first().copied().unwrap_or(5432),
        quote(database),
        quote(user)
    );
    if let Some(password) = pg.get_password() {
        text.push_str(" password=");
        text.push_str(&quote(
            std::str::from_utf8(password).map_err(|_| Error::Unsafe)?,
        ));
    }
    Ok(text)
}

pub(super) fn prepare(
    stage: &Path,
    manifest: &Manifest,
    pg: &tokio_postgres::Config,
    user: &str,
    budget: &Budget,
) -> Result<Prepared> {
    let root = stage.join("active");
    let socket = root.join("service.sock");
    if socket.as_os_str().as_bytes().len() > 100 {
        return Err(Error::Unsafe);
    }
    let mut config: Value = serde_json::from_slice(
        &crate::files::read_private(&stage.join("backup/service-config.json"), 16 * 1024)
            .map_err(|_| Error::Unsafe)?,
    )
    .map_err(|_| Error::Integrity)?;
    config["database_url"] = json!(connection_string(pg, user)?);
    config["schema"] = json!(manifest.schema);
    config["socket"] = json!(socket);
    config["storage"] = json!({"root":root.join("objects"),"backend":manifest.backend});
    let parsed: Config = serde_json::from_value(config.clone()).map_err(|_| Error::Unsafe)?;
    parsed.database_config().map_err(|_| Error::Unsafe)?;
    if serde_json::to_vec(&config)
        .map_err(|_| Error::Integrity)?
        .len()
        > 16 * 1024
    {
        return Err(Error::Limit);
    }
    budget.tick()?;
    // One canonical exclusive destination. Never overwrite or silently resume a
    // partial configuration, and never use the backup itself as live storage.
    files::mkdir(&root)?;
    File::open(stage)?.sync_all()?;
    let expected: Inventory = manifest
        .inventory
        .iter()
        .filter(|(name, _)| name.as_str() == "objects" || name.starts_with("objects/"))
        .map(|(name, entry)| (name.clone(), entry.clone()))
        .collect();
    let mut inventory = Inventory::new();
    files::walk(
        &files::open(&stage.join("backup/objects"), true)?,
        Some(&root.join("objects")),
        "objects",
        &mut inventory,
        &mut budget.clone(),
        0,
    )?;
    if inventory != expected {
        return Err(Error::Integrity);
    }
    let mut actual = Inventory::new();
    files::walk(
        &files::open(&root.join("objects"), true)?,
        None,
        "objects",
        &mut actual,
        &mut budget.clone(),
        0,
    )?;
    if actual != expected {
        return Err(Error::Integrity);
    }
    files::mkdir(&root.join("private"))?;
    File::open(root.join("private"))?.sync_all()?;
    File::open(&root)?.sync_all()?;
    budget.tick()?;
    Ok(Prepared {
        root,
        config,
        database: pg.get_dbname().ok_or(Error::Unsafe)?.into(),
        schema: manifest.schema.clone(),
    })
}

impl Prepared {
    pub(super) fn publish(self, manifest_sha256: &str) -> Result<Value> {
        let config = self.root.join("service.json");
        crate::files::write_private(
            &config,
            &serde_json::to_vec_pretty(&self.config).map_err(|_| Error::Integrity)?,
        )
        .map_err(|_| Error::Integrity)?;
        let result = json!({"version":1,"status":"configured","database":self.database,"schema":self.schema,
            "manifest_sha256":manifest_sha256,"config":config,"socket":self.config["socket"],
            "storage":self.root.join("objects"),"private_state":self.root.join("private"),
            "operator_fencing_acknowledged":true,"external_fencing_verified":false,
            "credentials_issued":false,"service_started":false,"github_reconciled":false});
        crate::files::write_private(
            &self.root.join("configured.json"),
            &serde_json::to_vec(&result).map_err(|_| Error::Integrity)?,
        )
        .map_err(|_| Error::Integrity)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovered_connection_quotes_credentials_and_preserves_target() {
        let mut pg = tokio_postgres::Config::new();
        pg.host_path("/private/socket path")
            .port(6543)
            .dbname("restored")
            .password("pa'ss\\word");
        let parsed: tokio_postgres::Config = connection_string(&pg, "user'\\name")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(parsed.get_hosts(), pg.get_hosts());
        assert_eq!(parsed.get_ports(), pg.get_ports());
        assert_eq!(parsed.get_dbname(), Some("restored"));
        assert_eq!(parsed.get_user(), Some("user'\\name"));
        assert_eq!(parsed.get_password(), Some(b"pa'ss\\word".as_slice()));
    }
}
