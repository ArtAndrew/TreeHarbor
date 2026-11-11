//! Resume one union upload and atomically stage every verified branch root.
use crate::{
    Error, Result,
    client::{Credential, Reply},
    import::{Transfer, retain_journal},
    pack::archive_branches::StageSource,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{path::Path, time::Duration};
use treeharbor_metadata::{commands::MAX_REQUEST_BYTES, github::ArchiveStageSpec};
use uuid::Uuid;

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Intent {
    tenant: String,
    repository: Uuid,
    epoch: i64,
    binding: Uuid,
    ttl_seconds: i32,
    specification: ArchiveStageSpec,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    upload: Uuid,
    intent: Intent,
}

pub(crate) struct Request<'a> {
    pub socket: &'a Path,
    pub credential: &'a Credential,
    pub timeout: Duration,
    pub state: &'a Path,
    pub source: StageSource,
    pub binding: Uuid,
    pub epoch: i64,
    pub ttl_seconds: i32,
}

pub(crate) async fn send(request: Request<'_>) -> Result<Reply> {
    if request.binding.is_nil() || request.epoch <= 0 || !(1..=3600).contains(&request.ttl_seconds)
    {
        return Err(Error::Config);
    }
    let proposed = Journal {
        version: 1,
        upload: Uuid::new_v4(),
        intent: Intent {
            tenant: request.credential.scope.tenant().into(),
            repository: request.credential.scope.repository(),
            epoch: request.epoch,
            binding: request.binding,
            ttl_seconds: request.ttl_seconds,
            specification: request.source.specification,
        },
    };
    let journal: Journal = retain_journal(request.state, &proposed, MAX_REQUEST_BYTES)?;
    if journal.version != 1 || journal.upload.is_nil() || journal.intent != proposed.intent {
        return Err(Error::ImportState);
    }
    let intent = journal.intent;
    let upload = journal.upload;
    let transfer = Transfer {
        socket: request.socket,
        credential: request.credential,
        timeout: request.timeout,
        upload,
        begin: json!({"method":"source_begin","key":format!("archive-{upload}-begin"),
            "authority":"connected","epoch":intent.epoch,"upload":upload,"ttl_seconds":intent.ttl_seconds,
            "max_objects":intent.specification.object_count,"max_bytes":intent.specification.encoded_bytes}),
        finish: json!({"method":"github","request":{"action":"archive_stage","key":format!("archive-{upload}-stage"),
            "epoch":intent.epoch,"binding":intent.binding,"upload":upload,"specification":intent.specification}}),
    };
    let reply = transfer
        .send(request.source.ids, request.source.store)
        .await?;
    if reply.status.is_success() {
        let expected = json!({"version":1,"tenant":intent.tenant,"repository":intent.repository,
            "authority":"connected","epoch":intent.epoch,"binding":intent.binding,"upload":upload,
            "state":"staged","specification":intent.specification});
        if reply.body["version"] != 1 || reply.body["result"] != expected {
            return Err(Error::Http);
        }
    }
    Ok(reply)
}
