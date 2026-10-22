use crate::{Error, Result, config::Config, database::Database, files};
use bytes::Bytes;
use deadpool::managed::Pool;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Request, Response, StatusCode, body::Incoming, header, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use rustix::fs::{FlockOperation, OFlags, flock};
use serde_json::json;
use std::{
    convert::Infallible,
    fs::{self, File, OpenOptions},
    future::Future,
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::Semaphore,
    task::JoinSet,
};
use treeharbor_metadata::{access::SecretToken, commands::MAX_REQUEST_BYTES};

type Body = http_body_util::combinators::BoxBody<Bytes, std::io::Error>;
fn full(bytes: Bytes) -> Body {
    Full::new(bytes).map_err(|never| match never {}).boxed()
}
pub struct Server {
    listener: UnixListener,
    guard: SocketGuard,
    config: Config,
    pool: Pool<Database>,
    backend: Option<crate::source::Backend>,
}
struct SocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
    _lock: File,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Ok(meta) = fs::symlink_metadata(&self.path)
            && meta.dev() == self.device
            && meta.ino() == self.inode
            && meta.file_type().is_socket()
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}
async fn bind(path: &Path) -> Result<(UnixListener, SocketGuard)> {
    let parent = files::private_directory(path.parent().ok_or(Error::UnsafeFile)?)?;
    let filename = path.file_name().ok_or(Error::UnsafeFile)?;
    let path = parent.join(filename);
    let lock_path = parent.join(format!("{}.lock", filename.to_str().ok_or(Error::Config)?));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
        .open(lock_path)?;
    files::private_file(&lock)?;
    flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|_| Error::AlreadyRunning)?;
    match fs::symlink_metadata(&path) {
        Ok(meta) => {
            if !meta.file_type().is_socket() || meta.uid() != rustix::process::geteuid().as_raw() {
                return Err(Error::UnsafeFile);
            }
            match tokio::time::timeout(Duration::from_secs(1), UnixStream::connect(&path)).await {
                Ok(Ok(_)) | Err(_) => return Err(Error::AlreadyRunning),
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    fs::remove_file(&path)?
                }
                Ok(Err(error)) => return Err(Error::Io(error)),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(Error::Io(error)),
    }
    let listener = UnixListener::bind(&path)?;
    let meta = fs::symlink_metadata(&path)?;
    let guard = SocketGuard {
        path: path.clone(),
        device: meta.dev(),
        inode: meta.ino(),
        _lock: lock,
    };
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok((listener, guard))
}
impl Server {
    pub async fn bind(config: Config) -> Result<Self> {
        let database = Database::new(config.clone())?;
        let pool = database.pool()?;
        // Fail before publishing a listening socket if schema or DB is unavailable.
        drop(pool.get().await.map_err(|_| Error::Database)?);
        let backend = match config.storage.clone() {
            Some(storage) => Some(crate::source::Backend::open(storage, &pool).await?),
            None => None,
        };
        let (listener, guard) = bind(&config.socket).await?;
        Ok(Self {
            listener,
            guard,
            config,
            pool,
            backend,
        })
    }
    pub fn socket(&self) -> &Path {
        &self.guard.path
    }
    pub async fn run(self, shutdown: impl Future<Output = ()>) -> Result<()> {
        let Self {
            listener,
            guard,
            config,
            pool,
            backend,
        } = self;
        let permits = Arc::new(Semaphore::new(config.max_connections));
        let mut tasks = JoinSet::new();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _=&mut shutdown=>break,
                Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
                incoming=listener.accept()=>{
                    let (stream,_)=incoming?;
                    let Ok(permit)=permits.clone().try_acquire_owned() else { drop(stream); continue; };
                    let pool=pool.clone(); let timeout=config.timeout(); let backend=backend.clone();
                    tasks.spawn(async move {
                        let _permit=permit;
                        let service=service_fn(move |request| {
                            let pool=pool.clone(); let backend=backend.clone();
                            async move {
                                let response=match tokio::time::timeout(timeout,handle(request,pool,backend)).await {
                                    Ok(response)=>response,
                                    Err(_)=>error(StatusCode::GATEWAY_TIMEOUT,"outcome_unknown"),
                                };
                                Ok::<_,Infallible>(response)
                            }
                        });
                        let mut http=hyper::server::conn::http1::Builder::new();
                        http.keep_alive(false).max_headers(32).max_header_size(8192).max_buf_size(16*1024)
                            .timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(5));
                        // Also bounds clients that stop reading a response.
                        let _=tokio::time::timeout(timeout+Duration::from_secs(6),http.serve_connection(TokioIo::new(stream),service)).await;
                    });
                }
            }
        }
        drop(listener);
        let grace = config.timeout() + Duration::from_secs(6);
        if tokio::time::timeout(grace, async { while tasks.join_next().await.is_some() {} })
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
}
fn response(status: StatusCode, value: serde_json::Value) -> Response<Body> {
    let mut response = Response::new(full(Bytes::from(value.to_string())));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        header::CONNECTION,
        header::HeaderValue::from_static("close"),
    );
    response
}
fn error(status: StatusCode, code: &'static str) -> Response<Body> {
    response(status, json!({"error":{"code":code}}))
}
async fn handle(
    request: Request<Incoming>,
    pool: Pool<Database>,
    backend: Option<crate::source::Backend>,
) -> Response<Body> {
    if request.headers().contains_key(header::ORIGIN)
        || request.headers().get_all(header::HOST).iter().count() != 1
        || request
            .headers()
            .get(header::HOST)
            .is_none_or(|v| v != "treeharbor.local")
    {
        return error(StatusCode::BAD_REQUEST, "invalid_host_or_origin");
    }
    if request.method() == hyper::Method::GET && request.uri() == "/healthz" {
        return response(StatusCode::OK, json!({"status":"alive"}));
    }
    let upload = crate::source::route(request.uri().path());
    let source_read = request.uri().path() == "/v1/source/read";
    let source_export = request.uri().path() == "/v1/source/export";
    let history_read = request.uri().path() == "/v1/history/read";
    let rebase_prepare = request.uri().path() == "/v1/changes/rebase";
    let native_prepare = request.uri().path() == "/v1/native/prepare";
    let change_submit = request.uri().path() == "/v1/changes/submit";
    if (request.uri().path() != "/v1/commands"
        && upload.is_none()
        && !source_read
        && !source_export
        && !history_read
        && !change_submit
        && !native_prepare
        && !rebase_prepare)
        || request.uri().query().is_some()
    {
        return error(StatusCode::NOT_FOUND, "not_found");
    }
    if request.method()
        != if upload.is_some() {
            hyper::Method::PUT
        } else {
            hyper::Method::POST
        }
    {
        return error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
    }
    if request
        .headers()
        .get_all(header::CONTENT_TYPE)
        .iter()
        .count()
        != 1
        || request.headers().get(header::CONTENT_TYPE).is_none_or(|v| {
            v != if upload.is_some() {
                "application/octet-stream"
            } else {
                "application/json"
            }
        })
    {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            if upload.is_some() {
                "binary_frame_required"
            } else {
                "json_required"
            },
        );
    }
    if request
        .headers()
        .get_all(header::AUTHORIZATION)
        .iter()
        .count()
        != 1
    {
        return error(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    let credential = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|v| SecretToken::parse(v).ok());
    let Some(credential) = credential else {
        return error(StatusCode::UNAUTHORIZED, "unauthenticated");
    };
    if let Some(route) = upload {
        let Some(backend) = backend else {
            return error(StatusCode::SERVICE_UNAVAILABLE, "storage_not_configured");
        };
        return match crate::source::put(request, &credential, route, backend, pool).await {
            Ok(value) => response(StatusCode::OK, value),
            Err(failure) => error(
                StatusCode::from_u16(failure.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                failure.code,
            ),
        };
    }
    let body = match tokio::time::timeout(
        Duration::from_secs(5),
        Limited::new(request.into_body(), MAX_REQUEST_BYTES).collect(),
    )
    .await
    {
        Ok(Ok(body)) => body.to_bytes(),
        Ok(Err(error_value)) if error_value.is::<http_body_util::LengthLimitError>() => {
            return error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
        }
        Ok(Err(_)) => return error(StatusCode::BAD_REQUEST, "invalid_body"),
        Err(_) => return error(StatusCode::REQUEST_TIMEOUT, "body_timeout"),
    };
    if rebase_prepare {
        let Some(backend) = backend else {
            return error(StatusCode::SERVICE_UNAVAILABLE, "storage_not_configured");
        };
        return match crate::rebases::prepare(&body, &credential, backend, pool).await {
            Ok(value) => response(StatusCode::OK, value),
            Err(f) => error(
                StatusCode::from_u16(f.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                f.code,
            ),
        };
    }
    if native_prepare {
        let Some(backend) = backend else {
            return error(StatusCode::SERVICE_UNAVAILABLE, "storage_not_configured");
        };
        return match crate::native::prepare(&body, &credential, backend, pool).await {
            Ok(value) => response(StatusCode::OK, value),
            Err(f) => error(
                StatusCode::from_u16(f.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                f.code,
            ),
        };
    }
    if change_submit {
        let Some(backend) = backend else {
            return error(StatusCode::SERVICE_UNAVAILABLE, "storage_not_configured");
        };
        return match crate::proposals::submit(&body, &credential, backend, pool).await {
            Ok(value) => response(StatusCode::OK, value),
            Err(failure) => error(
                StatusCode::from_u16(failure.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                failure.code,
            ),
        };
    }
    if source_export {
        let Some(backend) = backend else {
            return error(StatusCode::SERVICE_UNAVAILABLE, "storage_not_configured");
        };
        return match crate::source_export::export(&body, &credential, backend, pool).await {
            Ok(delivery) => {
                let mut result = Response::new(delivery.boxed());
                result.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static(crate::archive::CONTENT_TYPE),
                );
                result.headers_mut().insert(
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static("no-store"),
                );
                result.headers_mut().insert(
                    header::CONNECTION,
                    header::HeaderValue::from_static("close"),
                );
                result
            }
            Err(f) => error(
                StatusCode::from_u16(f.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                f.code,
            ),
        };
    }
    if source_read || history_read {
        let Some(backend) = backend else {
            return error(StatusCode::SERVICE_UNAVAILABLE, "storage_not_configured");
        };
        let delivery = if history_read {
            crate::history::read(&body, &credential, backend, pool).await
        } else {
            crate::source_read::read(&body, &credential, backend, pool).await
        };
        return match delivery {
            Ok(delivery) => {
                let mut response = Response::new(full(delivery.bytes));
                *response.headers_mut() = delivery.headers;
                response
            }
            Err(failure) => error(
                StatusCode::from_u16(failure.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                failure.code,
            ),
        };
    }
    let mut session = match pool.get().await {
        Ok(session) => session,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "database_unavailable"),
    };
    match session
        .metadata
        .execute_command_with_backend(
            &credential,
            &body,
            backend.as_ref().map(|b| b.config.backend.as_str()),
        )
        .await
    {
        Ok(value) => response(StatusCode::OK, value),
        Err(failure) => {
            let status = match failure.code() {
                "invalid_request" => StatusCode::BAD_REQUEST,
                "request_too_large" => StatusCode::PAYLOAD_TOO_LARGE,
                "unauthenticated" => StatusCode::UNAUTHORIZED,
                "forbidden" => StatusCode::FORBIDDEN,
                "not_found" => StatusCode::NOT_FOUND,
                "repository_fenced"
                | "execution_state_changed"
                | "idempotency_conflict"
                | "integration_state_changed"
                | "candidate_stale"
                | "admission_blocked"
                | "rebase_state_changed"
                | "version_conflict" => StatusCode::CONFLICT,
                "upload_state_changed" | "upload_membership" | "object_busy" | "lease_expired" => {
                    StatusCode::CONFLICT
                }
                "upload_limit" => StatusCode::TOO_MANY_REQUESTS,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            response(status, failure.response())
        }
    }
}
