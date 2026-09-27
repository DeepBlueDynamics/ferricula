# Ferricula v2 build snapshot — 2026-09-27

This branch publishes the current v2 implementation alongside the existing root project. It is an integration snapshot, not a declaration that all research goals are complete or a deployment of the running Steve instance.

Included: core durable memory, episode/causal records, cognition and ephemeral curation, provider-neutral decision gates, multilingual search, semantic embeddings, bounded generation/decision harness, server APIs/native MCP chat, and the temporary stdio Steve bridge.

Validation reported by the build agents on the source workspace: Docker integration build and isolated HTTP health smoke; core/episode/cognition/server locked tests inside the image; cognition 141 unit plus four synthetic integration tests; curation five tests; harness nine tests; gates five tests; search 67 tests; Windows semantic 19 tests plus explicit real GTR-T5-base 768-dimensional inference. These are source-workspace results, not a new clean-checkout CI run of this commit. Image reported: ferricula-server:integration-20260926, sha256:bccada28ad8ac2413f2cbb52facb15fd84e287e48b37d11985d59c2677c25d91.

Known unfinished work: native graph-walk MCP tool and its client acceptance; persistent native chat verification through the current Steve-connected client across server restart; server-side Chinese recall tokenization integration; final review of retrieval/cognition seams and curation usage receipts. Native chat exists in source, while the previously running Steve image lacks that endpoint. The bridge is a separate implementation and does not establish native persistence.

Default semantic ML linking remains incompatible with the tested older Linux/glibc container; Windows default-feature tests and actual inference passed after adjusting tokenizer features. The server image does not depend on the semantic ML crate.

Gate calibration artifacts did not authorize deployment. Synthetic monitoring and consolidation tests establish fixture invariants, not empirical calibration or broad cognitive capability. Research evaluations remain separate from release acceptance.

Private runtime memories, credentials, agent configuration, host-specific MCP registration and build artifacts are excluded. This publication does not recreate the live Steve container.
