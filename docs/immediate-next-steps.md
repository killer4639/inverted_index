# Immediate Next Steps

Last updated: **2026-08-30**

## Current decision

Establish a reproducible performance baseline before implementing updates.
Do not optimize during this phase.

Updates should not be added directly to the current single-segment design.
First introduce multiple immutable segments and an atomic manifest so writes do
not require rebuilding or mutating an existing segment.

## Implementation order

1. Baseline benchmarks
2. Multi-segment querying and atomic manifest
3. Mutable memtable and write-ahead log
4. Inserts, updates, deletes, and snapshot visibility
5. Background flush and segment compaction

## Baseline benchmark scope

Measure:

| Area | Measurements |
|---|---|
| Index construction | Documents/s, input MB/s, elapsed time, and peak memory |
| Segment size | Bytes/document, bytes/posting, and compression ratio |
| Term lookup | p50, p95, and p99 latency for missing, rare, medium, and common terms |
| Postings decoding | Postings/s and ns/posting for short, medium, and long lists |

Use:

- deterministic synthetic data with configurable vocabulary and Zipfian skew;
- adversarial data containing extremely common terms, mostly unique terms, and
  unusually large documents;
- one fixed real-world corpus.

Run release builds, repeat measurements, record dataset and machine metadata,
and distinguish warm-cache and cold-cache results where practical.

## Baseline exit criteria

The baseline phase is complete when:

- one command reproduces the benchmark suite;
- results are stable enough to detect approximately 5-10% regressions;
- every run records corpus and segment statistics;
- indexed results still match the in-memory correctness oracle;
- results are written in a machine-readable format for future comparisons.

Do not spend weeks perfecting the harness. Its purpose is to establish a trusted
reference point before architecture and performance changes.

## Next architectural milestone

After the baseline, implement:

```text
write batches
    -> immutable segment 1, segment 2, ...
    -> atomic manifest declaring visible segments
    -> queries merging results across visible segments
```

The detailed implementation plan is maintained in
[multi-segment-reads-implementation-plan.md](multi-segment-reads-implementation-plan.md).

Only after this milestone should the engine gain a memtable and WAL:

- insert: append to WAL and apply to memtable;
- delete: append a versioned tombstone;
- update: delete the previous version and insert a new version;
- flush: convert a frozen memtable into an immutable segment;
- compaction: merge segments and permanently apply tombstones.

The long-term direction is documented in
[search-engine-research-roadmap.md](search-engine-research-roadmap.md).
