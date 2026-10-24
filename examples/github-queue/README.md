# Real GitHub queue acceptance kit

`seed/` is a dependency-free synthetic Rust repository. `scenarios.json` defines
exact edits for two independent proposals, their explicit repair, and a separate
text conflict. These are scripted agent workloads, not LLM performance results.

From the TreeHarbor repository, run:

```sh
python3 scripts/test-github-queue-fixture.py
```

The local test uses real Git and Cargo in a disposable directory. It checks each
proposal separately, proves that the combined Git merge is textually clean but
fails the budget test, checks the repaired revision, and verifies the separate
text conflict. It neither connects to GitHub nor proves host integration.

## Host preparation

Use an explicitly authorized, otherwise empty **public organization repository**
or an eligible private organization repository on Enterprise Cloud. Personal
repositories do not meet GitHub's documented merge-queue requirements. Copy only
`seed/`, add the unmodified TreeHarbor root `LICENSE`, and publish the initial main.
Do not copy product history, internal planning files, local configuration or keys.
Keep the fixture limited to the reviewed synthetic workloads. Do not accept or
enqueue external contributions without the project's contribution-rights process.

Register a private App owned by the operator's organization. The manifest template
is a reviewable permission list, not a ready registration command. Choose a unique
App name and replace the callback host with the operator's HTTPS endpoint. For
the manifest handshake, add a controlled `redirect_url`, validate an unpredictable
`state`, and exchange the returned code without logging it or credentials. Follow
GitHub's [manifest flow](https://docs.github.com/en/apps/sharing-github-apps/registering-a-github-app-from-a-manifest).

Install the App on **only this fixture repository**. Contents write supports branch
publication and the async queue-result endpoint; Pull requests write supports PRs;
Checks read supports exact check observation; Merge queues write supports queue
operations. Metadata read is implicit. TreeHarbor narrows installation tokens per
operation. This template does not grant Administration, workflow-write, Checks
write, organization permissions or user OAuth access. Actual minimal-permission
acceptance remains to be verified. Configure repository rules using a separate
owner identity, not the App or an agent credential.

Configure main to require PRs, the observed GitHub Actions App's `Queue fixture`
check, a merge queue, and no force pushes/deletions/bypass. Start with MERGE,
ALLGREEN, build concurrency 2 and a maximum merge batch of 1. Record the actual
host configuration. Repeat the mapping acceptance for SQUASH and REBASE; passing
MERGE does not prove the other methods. Fixture rules must never replace or weaken
TreeHarbor's product-repository protection.

The dedicated [webhook receiver](../../docs/github-events.md) needs public TLS
ingress to `/github/events`, with its original request body/signature preserved.
Do not expose the command socket. Retain successful callback events and explicitly
recover missed deliveries; GitHub does not automatically retry failures.

## Evidence to collect

During an explicitly authorized live test, an operator can collect diagnostic
snapshots with their already-authenticated GitHub CLI:

```sh
python3 scripts/capture-github-queue.py --repository OWNER/REPOSITORY \
  --pull 1 --output /existing/private-parent/new-capture \
  --samples 12 --interval 5
```

This **test utility** runs only a read query against GitHub.com. It uses the
operator's CLI authentication; it does not exercise the product's App token scope.
It creates a new private evidence directory and never overwrites an earlier one.
It captures up to 20 queue entries, 30 checks per head and 8 immediate parents per
commit; any `hasNextPage` is retained and causes a nonzero exit. Null queues, missing
groups, historical checks and separate snapshots cannot prove membership or final
merge. Immediate parents are not a complete ancestry proof. Transient entries may
vanish between observations; retain signed events and reconcile separately. The
utility always emits `merge_verified: false` and cannot authorize publication.
Do not publish captures of private repositories without reviewing their contents.

Retain stable App/installation/repository identities, exact base/source commits,
PR IDs, queue configuration/entry observations, signed group payloads, exact group
check IDs/App IDs/head SHAs, commit parents/trees and final main ancestry. Capture
both the failed combined candidate and its separately checked replacement. Checks
on the old candidate must stay tied to that candidate. A branch-name convention,
webhook `merged` reason, enqueued response or passing PR check is insufficient.

Finally export the accepted source through TreeHarbor and verify it with ordinary
Git. Record missing or ambiguous provider evidence as unverified. This kit does not
yet automate the full live scenario or confirm complete membership/final mapping.
