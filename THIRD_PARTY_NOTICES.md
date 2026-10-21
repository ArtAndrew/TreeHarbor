# Third-party notices

TreeHarbor's original code is under Elastic License 2.0. Third-party dependencies
retain their own licenses. They are fetched from the versions in `Cargo.lock`;
this repository does not relicense them under ELv2.

The native merge increment uses these unmodified dependencies:

| Component | Upstream | Applicable license text |
|---|---|---|
| diffy 0.5.2, Brandon Williams | [diffy](https://github.com/bmwill/diffy) | MIT OR Apache-2.0; [MIT text](third-party/merge/diffy-MIT.txt) |
| hashbrown 0.17.1, Amanieu d'Antras | [hashbrown](https://github.com/rust-lang/hashbrown) | MIT OR Apache-2.0; [MIT text](third-party/merge/hashbrown-MIT.txt) |
| foldhash 0.2.0, Orson Peters | [foldhash](https://github.com/orlp/foldhash) | [Zlib text](third-party/merge/foldhash-Zlib.txt) |

This incremental inventory is not a complete distribution SBOM. The first binary
release must include a complete dependency/license inventory and corresponding
notices for every shipped component. No binary release is available yet.
