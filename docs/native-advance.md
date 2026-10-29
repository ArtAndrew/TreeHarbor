# Resume one native queue preparation (unreleased)

`native-advance` discovers the oldest pending entry for one native target and
prepares one merge candidate. It keeps its entry/candidate IDs and deterministic
request keys in an immutable private journal before the first server mutation.
It does not execute checks, review code, land a candidate or run a background loop.

Use a Standalone repository with an existing target and accepted queued changes.
The operator credential needs `read` and `integration.control`; agents must not
receive it. The journal path must be absolute and its parent private (`0700`).

```sh
treeharbor client --socket /private/service.sock \
  --credential-file /private/integrator.json native-advance \
  --target main --epoch 1 --state /private/prepare-one.json
```

Repeat the exact command after an interrupted connection or process. Do not delete
or edit the journal to retry an uncertain request. A changed target, repository or
epoch is rejected. Use the same principal (a renewed token is allowed); the server
rechecks permissions. Keep separate journal files for separate attempts and keep
them with the deployment's recovery state.

If the queue is empty, or another candidate already occupies its first entry, the
command returns that pending view without creating a journal or adopting the
other candidate. Otherwise it records one candidate identity, starts it, reads
current status, and computes it only while still `preparing`. Replays of start or
prepare can describe old state, so the command reads fresh candidate status before
returning. A competing claimant can make the stored attempt fail; inspect status
before deliberately starting a new attempt with a different journal.

`checking` means a candidate is ready for the existing check/review workflow,
not that its checks passed. Conflicted, failed, cancelled, stale and landed states
are returned without another preparation. Expired preparation fails explicitly;
this command neither renews its lease nor cancels a candidate waiting for review.
Use the existing authorized failure/cancellation operations after inspection.

The journal is not a backup of server data and does not fence an old installation
after database restore. Full deployment recovery and an automatic queue controller
remain separate unfinished work.
