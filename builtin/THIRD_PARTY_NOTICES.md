# Third-Party Notices

## EdgeBench task metadata

The 51 source task records converted into the current native built-in catalog
come from EdgeBench by ByteDance Seed.

- Dataset: <https://huggingface.co/datasets/ByteDance-Seed/EdgeBench>
- Dataset revision: `47846a4c3669ad447e0ea984833b0d352460c5f9`
- Dataset license: Creative Commons Attribution 4.0 International
- Evaluation harness: <https://github.com/ByteDance-Seed/EdgeBench>
- Harness revision: `f59bcb0f024d4bc8baedeac271306050e4bb0d33`
- Harness license: Apache License 2.0

License texts are included as [CC-BY-4.0.txt](licenses/CC-BY-4.0.txt) and
[Apache-2.0.txt](licenses/Apache-2.0.txt).

### Adaptations

A3S converted the source records into the native global built-in layout,
including Task ACL fields, prompt placement, generic `score` measurement,
catalog entries, provenance records, and quarantined A3S Judge Agent Asset
source packages. Exact source paths and SHA-256 digests, generated-file digests,
and the full change description are recorded in
[provenance/edgebench.json](provenance/edgebench.json).

The source OCI images are referenced but not copied, extracted, modified, or
redistributed here. The dataset license does not by itself establish rights to
redistribute every hidden test, third-party project, binary, dataset, or game
file inside those images. The source and publisher names identify provenance
only; this conversion does not imply endorsement or official status.

These tasks use A3S endpoint scoring when eventually admitted. They do not
silently adopt the source harness policy of selecting the best repeated hidden
submission, and results must not be represented as official EdgeBench
leaderboard results without a separately locked comparable protocol.

### Citation requested by the source project

~~~bibtex
@misc{edgebench2026,
  title  = {EdgeBench: Unveiling Scaling Laws of Learning from Real-World Environments},
  author = {Deyao Zhu and Xin Zhou and Shengling Qin and Xuekai Zhu and Hangliang Ding and Shu Zhong and others},
  year   = {2026},
  url    = {https://arxiv.org/abs/2607.05155},
}
~~~
