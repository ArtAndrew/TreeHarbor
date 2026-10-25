use tokio_postgres::{Client, NoTls};
use treeharbor_metadata::{Authority, Metadata, Scope};
use uuid::Uuid;
pub async fn connection(schema: &str) -> Client {
    let url = std::env::var("TH_TEST_DATABASE_URL")
        .expect("set TH_TEST_DATABASE_URL to a disposable local PostgreSQL database");
    let (client, driver) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    tokio::spawn(async move {
        let _ = driver.await;
    });
    client
        .query_one("SELECT set_config('search_path',$1,false)", &[&schema])
        .await
        .unwrap();
    client
}
pub async fn setup() -> (String, Scope, Metadata) {
    let schema = format!("th_test_{}", Uuid::new_v4().simple());
    let client = connection("public").await;
    // This identifier is generated locally; never interpolate user SQL input.
    client
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .await
        .unwrap();
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    db.migrate().await.unwrap();
    let scope = Scope::new("test-tenant", Uuid::new_v4()).unwrap();
    db.create_repository(&scope, Authority::Standalone)
        .await
        .unwrap();
    let client = connection(&schema).await;
    client
        .batch_execute("CREATE TABLE test_effects (value integer NOT NULL)")
        .await
        .unwrap();
    (schema, scope, db)
}

pub async fn cleanup(schema: &str) {
    connection("public")
        .await
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[allow(dead_code)]
pub async fn drop_unpopulated_v26(raw: &Client) {
    assert_eq!(raw.query_one("SELECT (SELECT count(*) FROM th_github_managed_checks)+(SELECT count(*) FROM th_github_managed_executions)+(SELECT count(*) FROM th_github_managed_check_revocations)", &[]).await.unwrap().get::<_,i64>(0),0);
    raw.batch_execute("DROP TABLE th_github_managed_executions,th_github_managed_check_revocations,th_github_managed_checks; DROP FUNCTION th_github_managed_execution_identity();").await.unwrap();
}
