//! Dedicated callback ingress. No agent commands, App private key or host writes.
use crate::{Error, Result, config::Config, database::Database, files};
use bytes::Bytes;
use deadpool::managed::Pool;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Request, Response, StatusCode, body::Incoming, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::Deserialize;
use serde_json::json;
use std::{
    convert::Infallible,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{sync::Semaphore, task::JoinSet};
use treeharbor_github_webhook::{MAX_BODY_BYTES, VerifiedDelivery, WebhookEvent, WebhookKey};
use treeharbor_metadata::Error as MetadataError;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    database: Config,
    app_id: u64,
    keys: Vec<KeyFile>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyFile {
    id: Uuid,
    secret_file: PathBuf,
}
fn response(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("connection", "close")
        .body(Full::new(Bytes::from(json!({"status":code}).to_string())))
        .expect("constant response headers")
}
fn one<'a>(req: &'a Request<Incoming>, name: &str) -> Option<&'a str> {
    let mut values = req.headers().get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        None
    } else {
        Some(value)
    }
}
async fn handle(
    req: Request<Incoming>,
    keys: Arc<Vec<WebhookKey>>,
    pool: Pool<Database>,
) -> Response<Full<Bytes>> {
    if req.method() == hyper::Method::GET
        && req
            .uri()
            .path_and_query()
            .is_some_and(|p| p.as_str() == "/health")
    {
        return response(StatusCode::OK, "alive");
    }
    if req.method() != hyper::Method::POST
        || !req
            .uri()
            .path_and_query()
            .is_some_and(|p| p.as_str() == "/github/events")
    {
        return response(StatusCode::NOT_FOUND, "route_not_found");
    }
    if req.headers().contains_key("content-encoding")
        || !one(&req, "content-type").is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return response(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required");
    }
    let (Some(event), Some(signature), Some(delivery)) = (
        one(&req, "x-github-event"),
        one(&req, "x-hub-signature-256"),
        one(&req, "x-github-delivery"),
    ) else {
        return response(StatusCode::BAD_REQUEST, "delivery_headers_required");
    };
    let Ok(id) = delivery.parse::<Uuid>() else {
        return response(StatusCode::BAD_REQUEST, "invalid_delivery_id");
    };
    if id.is_nil() || id.to_string() != delivery {
        return response(StatusCode::BAD_REQUEST, "invalid_delivery_id");
    }
    let event = event.to_owned();
    let signature = signature.to_owned();
    let body = match Limited::new(req.into_body(), MAX_BODY_BYTES)
        .collect()
        .await
    {
        Ok(v) => v.to_bytes(),
        Err(_) => return response(StatusCode::PAYLOAD_TOO_LARGE, "body_limit_or_invalid_body"),
    };
    let mut verified: Option<VerifiedDelivery> = None;
    for key in keys.iter() {
        match key.authenticate(&event, &signature, body.to_vec()) {
            Ok(v) => {
                verified = Some(v);
                break;
            }
            Err(treeharbor_github_webhook::Error::Signature) => {}
            Err(treeharbor_github_webhook::Error::Size) => {
                return response(StatusCode::PAYLOAD_TOO_LARGE, "body_limit");
            }
            Err(_) => {
                return response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "unsupported_or_invalid_event",
                );
            }
        }
    }
    let Some(verified) = verified else {
        return response(StatusCode::UNAUTHORIZED, "invalid_signature");
    };
    if matches!(verified.event(), WebhookEvent::Ping { .. }) {
        return response(StatusCode::OK, "ping_verified");
    }
    let mut db = match pool.get().await {
        Ok(v) => v,
        Err(_) => return response(StatusCode::SERVICE_UNAVAILABLE, "retry_original_delivery"),
    };
    match db.metadata.accept_github_event(id, &verified).await {
        Ok(_) => response(StatusCode::OK, "durably_received"),
        Err(MetadataError::HostEventLimit) => response(
            StatusCode::SERVICE_UNAVAILABLE,
            "retention_capacity_exhausted",
        ),
        Err(MetadataError::IdempotencyConflict) => {
            response(StatusCode::CONFLICT, "delivery_identity_conflict")
        }
        Err(
            MetadataError::HostEventState | MetadataError::HostBindingState | MetadataError::Fenced,
        ) => response(StatusCode::CONFLICT, "event_channel_unavailable"),
        Err(_) => response(StatusCode::SERVICE_UNAVAILABLE, "retry_original_delivery"),
    }
}
pub(crate) async fn serve(path: &Path) -> Result<()> {
    let c: Configuration = serde_json::from_slice(&files::read_private(path, 16 * 1024)?)
        .map_err(|_| Error::Config)?;
    if c.database.storage.is_some() || !(1..=2).contains(&c.keys.len()) {
        return Err(Error::Config);
    }
    let mut keys = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for k in c.keys {
        if !k.secret_file.is_absolute() || !ids.insert(k.id) {
            return Err(Error::Config);
        }
        keys.push(
            WebhookKey::new(c.app_id, k.id, files::read_private(&k.secret_file, 256)?)
                .map_err(|_| Error::Config)?,
        );
    }
    let keys = Arc::new(keys);
    let config = c.database;
    let pool = Database::new(config.clone())?.pool()?;
    drop(pool.get().await.map_err(|_| Error::Database)?);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let (listener, guard) = crate::server::bind(&config.socket).await?;
    println!(
        "{}",
        json!({"status":"listening","socket":config.socket,"service":"github_webhook"})
    );
    let permits = Arc::new(Semaphore::new(config.max_connections));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            _=terminate.recv()=>break,
            _=interrupt.recv()=>break,
            Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
            incoming=listener.accept()=>{
                let (stream,_)=incoming?;
                let Ok(permit)=permits.clone().try_acquire_owned() else {drop(stream);continue;};
                let pool=pool.clone(); let keys=keys.clone(); let timeout=config.timeout();
                tasks.spawn(async move {
                    let _permit=permit;
                    let service=service_fn(move |request| {
                        let pool=pool.clone();let keys=keys.clone();
                        async move {
                            let r=match tokio::time::timeout(timeout,handle(request,keys,pool)).await {
                                Ok(r)=>r,Err(_)=>response(StatusCode::GATEWAY_TIMEOUT,"retry_original_delivery"),
                            };
                            Ok::<_,Infallible>(r)
                        }
                    });
                    let mut http=hyper::server::conn::http1::Builder::new();
                    http.keep_alive(false).max_headers(32).max_header_size(8192).max_buf_size(16*1024)
                        .timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(5));
                    let _=tokio::time::timeout(timeout+Duration::from_secs(6),http.serve_connection(TokioIo::new(stream),service)).await;
                });
            }
        }
    }
    drop(listener);
    if tokio::time::timeout(config.timeout() + Duration::from_secs(6), async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    pool.close();
    drop(guard);
    Ok(())
}
