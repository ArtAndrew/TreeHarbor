# Contributing

Please start with an issue describing a reproducible problem, expected behavior
and workload. Do not include private source code, credentials or customer data.
Use the commands in the README to run format, lint and tests.

Changes to object encoding, durability, identity, authority or trust boundaries
need an ADR and tests that cover the failure boundary. Benchmarks must retain raw
data, environment details, errors and the strongest applicable baseline. Never
replace an external dependency with a mock and report the release gate as passed.

## Contribution rights during initial development

TreeHarbor is under ELv2. The project also intends to offer separate commercial
Provider/OEM licensing. Contributors keep their copyright; a sufficient grant
for that licensing must be established before their original code is merged.
A DCO sign-off by itself does not grant arbitrary relicensing rights.

**External code contributions are not being merged until the contribution
agreement and its verification process are published.** Issues, suggestions and
reproducible bug reports are welcome now. Do not sign an agreement by copying a
statement from this document: this is process guidance, not a CLA.

The initial contribution-rights check only accepts commits authored by the
project owner. It fails closed for unknown authors and co-authored commits.
An account match is an administrative check, not evidence of copyright ownership;
maintainers must also check provenance and preserve third-party licenses.

No external contribution should be merged using an administrator bypass. The
planned process must cover all authors, employers when applicable, versioned
terms, authority to grant rights and an auditable record before merge.
