# TreeHarbor queue acceptance fixture

This repository contains only synthetic code for testing TreeHarbor's GitHub
integration. It is not the TreeHarbor product or a customer repository. No secrets
or App private keys belong here. The original code is under Elastic License 2.0;
copyright Artishchev Andrey Borisovich.

Run `cargo test --locked --offline` with Rust 1.99.1. The three tests verify a
synthetic worker configuration, not operating-system resource enforcement.

Two independent changes start from this baseline:

- Agent A increases memory per worker from 256 MB to 512 MB.
- Agent B increases worker count from 2 to 4.

Each change passes all tests by itself. They edit different sections of the same
file and merge without a text conflict. Their combination requests 2048 MB against
a 1024 MB budget: the combined test must fail. An explicit repair increases the
synthetic budget to 2048 MB and needs a new check on its new revision. Another
scenario changes the same memory value to 384 MB, producing a text conflict with A.

The workflow checks the PR **source head** for pull requests and the actual group
head for `merge_group`. These are different subjects. Its 30-second group delay
helps capture a transient queue entry; it is not a performance measurement or a
guarantee that observation will succeed. No host-write token or secret is supplied
to candidate code. Required-check configuration must pin the `Queue fixture` check
and its observed GitHub Actions App ID.

A green fixture run does not prove TreeHarbor's integration. Acceptance additionally
requires the real TreeHarbor import, workspace, change, publication, queue and
evidence paths, retained signed events, exact group checks and final Git mapping.
