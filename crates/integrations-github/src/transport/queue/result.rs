use super::*;

impl Connection {
    /// Reads only the retained async UUID; never dispatches or retries a merge.
    /// GitHub nevertheless requires Contents-write for this GET. Mint only that
    /// permission and Metadata-read, independently of the queue observation token.
    pub async fn queue_result(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        request_uuid: &str,
    ) -> Result<ObservedQueue> {
        b.validate()?;
        let placeholder = QueueAsyncResult {
            request_uuid: request_uuid.into(),
            http_status: 404,
            outcome: QueueAsyncOutcome::Unavailable,
        };
        placeholder.validate(t).map_err(|_| Error::Queue)?;
        if b.repository_id != t.pull.repository_id {
            return Err(Error::Queue);
        }
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let started = now()? as i64 * 1000;
        let capability = wire::Capability::QueueResult;
        let (installation, token) =
            tokio::time::timeout(PROBE_TIMEOUT, self.mint_for(b, capability))
                .await
                .map_err(|_| Error::Deadline)??;
        let result = tokio::time::timeout(PROBE_TIMEOUT, async {
            // Archive/disabled state prevents new writes, not historical recovery.
            self.observe_for(b, &installation, &token, capability)
                .await?;
            let path = format!(
                "repos/{}/{}/pulls/{}/merge-async/{request_uuid}",
                b.owner, b.name, t.pull.pull_number
            );
            let (status, bytes) = self
                .request_status(
                    Stage::Queue,
                    Method::GET,
                    &path,
                    token.token.header()?,
                    None,
                    &[StatusCode::OK, StatusCode::NOT_FOUND],
                )
                .await?;
            decode_result(status.as_u16(), &bytes, t, request_uuid)
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        let revoked = self.revoke(&token).await.is_ok();
        let observation = QueueObservation {
            version: 1,
            started_at_unix_ms: started,
            finished_at_unix_ms: now()? as i64 * 1000,
            snapshot: None,
            receipt: None,
            enqueue_receipt: None,
            async_result: Some(result?),
            mutation_not_sent: false,
            token_revoked: revoked,
        };
        observation.validate(t).map_err(|_| Error::Queue)?;
        Ok(ObservedQueue {
            binding: b.clone(),
            observation,
            token_expires_at: token.expires_at.clone(),
        })
    }
}

fn decode_result(
    status: u16,
    bytes: &[u8],
    t: &QueueTarget,
    request_uuid: &str,
) -> Result<QueueAsyncResult> {
    let outcome = if status == 404 {
        QueueAsyncOutcome::Unavailable
    } else {
        let raw: RawReceipt = serde_json::from_slice(bytes).map_err(|_| Error::Queue)?;
        let d = raw.details;
        if raw.status == "pending" {
            if d.uuid.as_deref() != Some(request_uuid) || d.sha.is_some() {
                return Err(Error::Queue);
            }
            QueueAsyncOutcome::Pending {
                expected_head_revision: format!(
                    "git:sha1:{}",
                    d.expected_head_sha.ok_or(Error::Queue)?
                ),
                merge_action: d.merge_action.ok_or(Error::Queue)?,
                bypass_rules: d.bypass_rules.ok_or(Error::Queue)?,
            }
        } else {
            if d.uuid.is_some()
                || d.expected_head_sha.is_some()
                || d.merge_action.is_some()
                || d.bypass_rules.is_some()
            {
                return Err(Error::Queue);
            }
            match (raw.status.as_str(), d.sha) {
                ("enqueued", None) => QueueAsyncOutcome::Enqueued,
                ("failed", None) => QueueAsyncOutcome::Failed,
                ("merged", Some(sha)) => QueueAsyncOutcome::Merged {
                    final_revision: format!("git:sha1:{sha}"),
                },
                _ => return Err(Error::Queue),
            }
        }
    };
    let result = QueueAsyncResult {
        request_uuid: request_uuid.into(),
        http_status: status,
        outcome,
    };
    result.validate(t).map_err(|_| Error::Queue)?;
    Ok(result)
}
