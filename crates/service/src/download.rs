//! Validate source responses before publishing any file to the caller's disk.
use crate::{
    Error, Result,
    client::{self, Credential, RawReply},
};
use hyper::StatusCode;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use treeharbor_core::{Object, ObjectId, RevisionRef};
use treeharbor_metadata::workspace_reads::SourceFence;

pub struct SourceReply {
    pub status: StatusCode,
    pub metadata: Value,
    pub content: Option<Vec<u8>>,
}
fn text<'a>(reply: &'a RawReply, name: &str) -> Result<&'a str> {
    if reply.headers.get_all(name).iter().count() != 1 {
        return Err(Error::Http);
    }
    reply
        .headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .ok_or(Error::Http)
}
fn context_matches(value: &Value, fence: SourceFence) -> bool {
    value["workspace"] == fence.workspace.to_string()
        && value["generation"] == fence.generation
        && value["authority"] == fence.authority.as_str()
        && value["epoch"] == fence.epoch
}
pub async fn read(
    socket: &Path,
    credential: &Credential,
    fence: SourceFence,
    operation: Value,
    timeout: Duration,
) -> Result<SourceReply> {
    let file = operation["kind"] == "file";
    let reply=client::source_read(socket,credential,json!({"workspace":fence.workspace.to_string(),"generation":fence.generation,"authority":fence.authority.as_str(),"epoch":fence.epoch,"operation":operation}),file,timeout).await?;
    decode_reply(reply, fence, file).await
}
async fn decode_reply(reply: RawReply, fence: SourceFence, file: bool) -> Result<SourceReply> {
    if !reply.status.is_success() || !file {
        if text(&reply, "content-type")? != "application/json" {
            return Err(Error::Http);
        }
        let metadata: Value = serde_json::from_slice(&reply.bytes).map_err(|_| Error::Http)?;
        if reply.status.is_success()
            && (metadata["version"] != 1 || !context_matches(&metadata["result"], fence))
        {
            return Err(Error::Http);
        }
        return Ok(SourceReply {
            status: reply.status,
            metadata,
            content: None,
        });
    }
    if text(&reply, "content-type")? != "application/octet-stream"
        || text(&reply, "x-treeharbor-version")? != "1"
    {
        return Err(Error::Http);
    }
    let generation: i64 = text(&reply, "x-treeharbor-generation")?
        .parse()
        .map_err(|_| Error::Http)?;
    let epoch: i64 = text(&reply, "x-treeharbor-epoch")?
        .parse()
        .map_err(|_| Error::Http)?;
    let revision: RevisionRef = text(&reply, "x-treeharbor-revision")?
        .parse()
        .map_err(|_| Error::Http)?;
    if !matches!(revision, RevisionRef::Native(_)) {
        return Err(Error::Http);
    }
    let snapshot: ObjectId = text(&reply, "x-treeharbor-snapshot")?
        .parse()
        .map_err(|_| Error::Http)?;
    let profile: ObjectId = text(&reply, "x-treeharbor-profile")?
        .parse()
        .map_err(|_| Error::Http)?;
    let id: ObjectId = text(&reply, "x-treeharbor-object")?
        .parse()
        .map_err(|_| Error::Http)?;
    let mode = text(&reply, "x-treeharbor-mode")?;
    if !matches!(mode, "100644" | "100755" | "120000") {
        return Err(Error::Http);
    }
    let context = json!({"workspace":text(&reply,"x-treeharbor-workspace")?,"generation":generation,"authority":text(&reply,"x-treeharbor-authority")?,"epoch":epoch,"base":revision.to_string(),"snapshot":snapshot.to_string(),"profile_digest":profile.to_string(),"object":id.to_string(),"mode":mode});
    if !context_matches(&context, fence) {
        return Err(Error::Http);
    }
    let frame = reply.bytes;
    let content = tokio::task::spawn_blocking(move || {
        let Object::Blob(bytes) = Object::decode_verified(id, &frame).map_err(|_| Error::Http)?
        else {
            return Err(Error::Http);
        };
        Ok(bytes)
    })
    .await
    .map_err(|_| Error::Http)??;
    Ok(SourceReply {
        status: reply.status,
        metadata: json!({"version":1,"result":context}),
        content: Some(content),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn response(fence: SourceFence) -> RawReply {
        let object = Object::Blob(b"fn main() {}\n".to_vec());
        let mut headers = hyper::HeaderMap::new();
        for (key, value) in [
            ("content-type", "application/octet-stream".into()),
            ("x-treeharbor-version", "1".into()),
            ("x-treeharbor-workspace", fence.workspace.to_string()),
            ("x-treeharbor-generation", fence.generation.to_string()),
            ("x-treeharbor-authority", fence.authority.as_str().into()),
            ("x-treeharbor-epoch", fence.epoch.to_string()),
            (
                "x-treeharbor-revision",
                "native:sha256:edd78b7b781955d45661c7d81c46ac4939ec0cb6fa9a80dc1fd28b0e53b5bf3f"
                    .into(),
            ),
            (
                "x-treeharbor-snapshot",
                "sha256:8e64bc29cab70eaae9816b3f7d26b3e08efccf2376da28118ef474feb4078bd3".into(),
            ),
            (
                "x-treeharbor-profile",
                treeharbor_core::SourceProfile::full().id().to_string(),
            ),
            (
                "x-treeharbor-object",
                "sha256:1bc57ba770f57f276f1098c147f4e105ca0f2348903d940c2a82f458b6b03cd5".into(),
            ),
            ("x-treeharbor-mode", "100644".into()),
        ] {
            headers.insert(key, hyper::header::HeaderValue::from_str(&value).unwrap());
        }
        RawReply {
            file: None,
            status: StatusCode::OK,
            headers,
            bytes: object.encode().unwrap().into(),
        }
    }
    #[tokio::test]
    async fn file_response_requires_bound_context_and_verified_blob_before_exposing_bytes() {
        let fence = SourceFence {
            workspace: uuid::Uuid::new_v4(),
            generation: 1,
            authority: treeharbor_metadata::Authority::Standalone,
            epoch: 1,
        };
        let valid = decode_reply(response(fence), fence, true).await.unwrap();
        assert_eq!(valid.content.unwrap(), b"fn main() {}\n");
        assert!(
            decode_reply(
                response(fence),
                SourceFence {
                    generation: 2,
                    ..fence
                },
                true
            )
            .await
            .is_err()
        );
        let mut reply = response(fence);
        reply.bytes = b"corrupt".as_slice().into();
        assert!(decode_reply(reply, fence, true).await.is_err());
        let mut reply = response(fence);
        reply.headers.remove("x-treeharbor-snapshot");
        assert!(decode_reply(reply, fence, true).await.is_err());
        let mut reply = response(fence);
        reply.headers.append(
            "x-treeharbor-mode",
            hyper::header::HeaderValue::from_static("120000"),
        );
        assert!(decode_reply(reply, fence, true).await.is_err());
    }
}
