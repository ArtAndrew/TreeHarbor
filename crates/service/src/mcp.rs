//! MCP is a scoped client of the existing service, never a second policy engine.
mod catalog;
use crate::{
    Error, Result,
    client::{self, Credential},
};
use catalog::Route;
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt, model::*, service::RequestContext,
    transport::async_rw::AsyncRwTransport,
};
use serde_json::{Value, json};
use std::{borrow::Cow, path::PathBuf, time::Duration};
use tokio::sync::Semaphore;
use treeharbor_metadata::{Authority, workspace_reads::SourceFence};

const MAX_LINE: usize = 128 * 1024;
const MAX_RESULT: usize = 64 * 1024;
const MAX_INLINE: usize = 16 * 1024;
struct Adapter {
    socket: PathBuf,
    credential: Credential,
    timeout: Duration,
    calls: Semaphore,
}
pub async fn serve(socket: PathBuf, credential_file: PathBuf, timeout: Duration) -> Result<()> {
    let adapter = Adapter {
        socket,
        credential: Credential::load(&credential_file)?,
        timeout,
        calls: Semaphore::new(4),
    };
    let transport = AsyncRwTransport::new_server(tokio::io::stdin(), tokio::io::stdout())
        .with_max_line_length(MAX_LINE);
    // No stdout banners, tracing subscriber, environment credentials or shell execution.
    let running = adapter.serve(transport).await.map_err(|_| Error::Mcp)?;
    running.waiting().await.map_err(|_| Error::Mcp)?;
    Ok(())
}
fn bad() -> ErrorData {
    ErrorData::invalid_params("invalid TreeHarbor tool arguments", None)
}
fn result(status: Option<u16>, response: Value, error: bool) -> CallToolResponse {
    let value = json!({"adapter_version":1,"http_status":status,"response":response});
    if error {
        CallToolResult::structured_error(value).into()
    } else {
        CallToolResult::structured(value).into()
    }
}
fn failure(code: &str, unknown: bool) -> CallToolResponse {
    result(
        None,
        json!({"error":{"code":code},"outcome_unknown":unknown,"retry":"inspect original operation key; never create a new key to retry an uncertain mutation"}),
        true,
    )
}
fn hex_bytes(value: &str) -> std::result::Result<Vec<u8>, ErrorData> {
    if value.len() > MAX_INLINE * 2
        || !value.len().is_multiple_of(2)
        || !value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(bad());
    }
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).map_err(|_| bad()))
        .collect()
}
impl Adapter {
    async fn dispatch(&self, route: Route, mut args: Value) -> Result<client::Reply> {
        match route {
            Route::Command(method) => {
                args["method"] = json!(method);
                client::send(
                    &self.socket,
                    &self.credential.token,
                    self.credential.request(args).to_string().into_bytes(),
                    self.timeout,
                )
                .await
            }
            Route::Native(action) => {
                args["action"] = json!(action);
                client::send(
                    &self.socket,
                    &self.credential.token,
                    self.credential
                        .request(json!({"method":"native","request":args}))
                        .to_string()
                        .into_bytes(),
                    self.timeout,
                )
                .await
            }
            Route::Proposal => {
                args["version"] = json!(1);
                args["tenant"] = json!(self.credential.scope.tenant());
                args["repository"] = json!(self.credential.scope.repository().to_string());
                client::send_proposal(
                    &self.socket,
                    &self.credential.token,
                    args.to_string().into_bytes(),
                    self.timeout,
                )
                .await
            }
            Route::Blob => {
                let upload = args["upload"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .ok_or(Error::Config)?;
                let content = hex_bytes(args["content_hex"].as_str().ok_or(Error::Config)?)
                    .map_err(|_| Error::Size)?;
                let frame = treeharbor_core::Object::Blob(content)
                    .encode()
                    .map_err(|_| Error::Size)?;
                let id = treeharbor_core::ObjectId::hash_frame(&frame);
                client::send_change_blob(
                    &self.socket,
                    &self.credential,
                    upload,
                    id,
                    frame,
                    self.timeout,
                )
                .await
            }
            Route::Source => {
                let fence = SourceFence {
                    workspace: args["workspace"]
                        .as_str()
                        .and_then(|s| s.parse().ok())
                        .ok_or(Error::Config)?,
                    generation: args["generation"].as_i64().ok_or(Error::Config)?,
                    epoch: args["epoch"].as_i64().ok_or(Error::Config)?,
                    authority: match args["authority"].as_str() {
                        Some("standalone") => Authority::Standalone,
                        Some("connected") => Authority::Connected,
                        _ => return Err(Error::Config),
                    },
                };
                let reply = crate::download::read(
                    &self.socket,
                    &self.credential,
                    fence,
                    args["operation"].clone(),
                    self.timeout,
                )
                .await?;
                let mut body = reply.metadata;
                if let Some(content) = reply.content {
                    if content.len() > MAX_INLINE {
                        return Ok(client::Reply {
                            status: hyper::StatusCode::PAYLOAD_TOO_LARGE,
                            body: json!({"error":{"code":"inline_file_limit"},"metadata":body,"bytes":content.len(),"use":"client source-file or the binary source API"}),
                        });
                    }
                    body["content_hex"] = json!(crate::pack::hex(&content));
                    body["untrusted_source_content"] = json!(true);
                }
                Ok(client::Reply {
                    status: reply.status,
                    body,
                })
            }
            Route::History => {
                args["query"] = args
                    .as_object_mut()
                    .unwrap()
                    .remove("history_query")
                    .unwrap();
                client::history(&self.socket, &self.credential, args, self.timeout).await
            }
            Route::Diff => {
                args["query"] = args.as_object_mut().unwrap().remove("diff_query").unwrap();
                client::review(&self.socket, &self.credential, args, self.timeout).await
            }
        }
    }
}
impl ServerHandler for Adapter {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("treeharbor", env!("CARGO_PKG_VERSION")))
    }
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(vec![
            ProtocolVersion::V_2026_07_28,
            ProtocolVersion::V_2025_11_25,
        ])
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        catalog::definitions()
            .into_iter()
            .find(|d| d.tool.name == name)
            .map(|d| d.tool)
    }
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, ErrorData> {
        if request.is_some_and(|r| r.cursor.is_some()) {
            return Err(bad());
        }
        Ok(ListToolsResult::with_all_items(
            catalog::definitions().into_iter().map(|d| d.tool).collect(),
        ))
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, ErrorData> {
        let definition = catalog::definitions()
            .into_iter()
            .find(|d| d.tool.name == request.name)
            .ok_or_else(bad)?;
        let args = request.arguments.unwrap_or_default();
        let schema = &definition.tool.input_schema;
        let fields = schema["properties"].as_object().unwrap();
        // Reject unexpected top-level identities/routes even with an owner credential.
        // Full domain validation is performed by the same HTTP service as the CLI.
        if args.len() != fields.len()
            || args.keys().any(|k| !fields.contains_key(k))
            || request.request_state.is_some()
            || request.input_responses.is_some()
        {
            return Err(bad());
        }
        let args = Value::Object(args);
        if args.to_string().len() > treeharbor_metadata::commands::MAX_REQUEST_BYTES - 1024 {
            return Ok(failure("request_too_large", false));
        }
        if matches!(definition.route, Route::Blob) {
            hex_bytes(args["content_hex"].as_str().ok_or_else(bad)?)?;
        }
        let read = definition
            .tool
            .annotations
            .as_ref()
            .and_then(|a| a.read_only_hint)
            .unwrap_or(false);
        let Ok(_permit) = self.calls.try_acquire() else {
            return Ok(failure("adapter_busy", false));
        };
        let reply = match self.dispatch(definition.route, args).await {
            Ok(r) => r,
            Err(Error::Config) => return Err(bad()),
            Err(Error::Size) => return Ok(failure("request_or_response_too_large", !read)),
            Err(_) => return Ok(failure("service_unavailable_or_outcome_unknown", !read)),
        };
        if reply.body.to_string().len() > MAX_RESULT {
            return Ok(failure("result_too_large", !read));
        }
        Ok(result(
            Some(reply.status.as_u16()),
            reply.body,
            !reply.status.is_success(),
        ))
    }
}
