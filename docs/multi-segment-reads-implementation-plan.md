# Multi-Segment Reads Implementation Plan

## Overview

Extend the engine from opening one immutable segment to opening a consistent,
immutable set of segments described by a binary manifest. Exact-term lookups
will search every visible segment and lazily return postings identified by a
segment ID and segment-local document ID.

This plan includes only the append-only manifest publication required by the
current `build` command. It intentionally excludes advanced write lifecycle
features such as updates, deletes, repair, and compaction.

## Current State Analysis

The current engine has the correct low-level primitives:

- `SegmentReader` memory-maps one immutable segment, verifies its checksum, and
  reads enough header layout information for safe access
  (`src/storage/segment_reader.rs`).
- Segment format version 1 stores dense segment-local `u32` document IDs and
  does not contain global identity (`src/storage/segment_codec.rs`).
- `BinaryFileCodec` centralizes the common magic/version prefix, CRC32 footer,
  checked decoding cursor, and temporary-file publication sequence
  (`src/storage/binary_file.rs`).
- `QueryEngine` performs exact dictionary lookup and returns a lazy
  `PostingsDecoder` (`src/search/query_engine.rs`).
- `LookupExecutor` owns segment opening, memory-map reuse, result
  materialization, and completed lookup statistics
  (`src/search/lookup_executor.rs`).
- Segment publication uses the shared temporary-file writer followed by rename
  (`src/storage/binary_file.rs`).
- The CLI uses one fixed `index` directory. `build <corpus>` writes another
  numbered segment, while `lookup` and `lookup-stats` query all numbered
  segments (`src/main.rs`).
- The current startup path synthesizes a manifest by scanning every segment.
  Persisted manifests are not yet published or used to define visibility.

The missing layer is an immutable index snapshot that owns several
checksum-verified segment readers and defines the physical identity of
documents across them.

## Decisions

### Index scope

An index is a directory containing immutable segment files and immutable
manifest generations:

```text
my-index/
  segments/
    segment-00000000000000000001.idx
    segment-00000000000000000002.idx
  manifests/
    manifest-00000000000000000001.bin
    manifest-00000000000000000002.bin
```

Temporary files use names that cannot be mistaken for published files and are
ignored by readers.

### Manifest discovery

There is no mutable `CURRENT` pointer in this phase. `open_latest`:

1. lists files matching the exact published-manifest naming convention;
2. selects the highest generation number;
3. opens and validates that manifest;
4. fails if that manifest is corrupt or references invalid segments.

It must not silently fall back to an older generation. Silent fallback would
turn corruption into stale reads.

### Document identity

Follow the Lucene/Tantivy physical identity model:

```rust
pub struct DocumentAddress {
    pub segment_id: SegmentId,
    pub local_document_id: u32,
}
```

Segment-local document IDs remain dense and compact. A physical address is
valid only while its manifest snapshot and referenced segment remain valid.
Compaction may later replace physical addresses.

A separate stable logical document key will be introduced with updates and
deletes. Physical posting IDs must not become an externally stable identity.

### Result ordering

Manifest segment records are stored in strictly increasing `SegmentId` order.
Within each segment, postings remain ordered by local document ID.

Multi-segment results are therefore ordered lexicographically by:

```text
(segment_id, local_document_id)
```

This is deterministic without inventing a permanent global ordinal.

### Lazy execution

Dictionary lookup is performed eagerly across all visible segments because the
engine must discover which segments contain the term. Posting decoding remains
lazy:

```text
query term
    -> binary-search every segment dictionary
    -> retain decoders only for matching segments
    -> return MultiSegmentPostings iterator
    -> decode one segment at a time as the caller advances
```

The low-level multi-segment query API must not allocate one combined postings
vector. The CLI executor may continue materializing results because it already
does so and needs completed decode statistics.

### Segment format

Segment format version 1 remains unchanged. Multi-segment metadata belongs in
the manifest, not in every segment.

## Desired End State

The completed phase keeps the current commands:

```text
build <corpus>
lookup <term>
lookup-stats <term>
```

Each successful build publishes one immutable segment followed by a new
manifest generation. Lookups pin the latest successfully published generation
and reuse every open memory map until another build succeeds.

A lookup returns:

```rust
pub struct AddressedPosting {
    pub address: DocumentAddress,
    pub term_frequency: u32,
}
```

The same term may occur in zero, one, or many segments. Results are complete,
deterministic, lazy at the query-engine boundary, and equivalent to a simple
in-memory oracle.

## What We Are Not Doing

- No WAL, replay, repair, or orphan cleanup
- No mutable memtable
- No separate assembly, import, or index-directory command family
- No automatic adoption of pre-manifest segment directories
- No concurrent or multi-process writers
- No updates or deletes
- No tombstones
- No segment merging or compaction
- No stable external document key
- No concurrent manifest refresh
- No automatic orphan-file deletion
- No query language, Boolean operators, ranking, or scoring
- No parallel segment opening or querying
- No change to segment format version 1
- No optimization intended to hide the cost of many segments

The phase should expose the segment-count cost clearly so future compaction work
has a measured justification.

## Binary Manifest Format

### File layout

```text
+----------------------------------------+
| Header                                 |
|   magic                         8 bytes |
|   format version                   u16 |
|   manifest generation              u64 |
|   segment count                    u32 |
+----------------------------------------+
| Segment records                         |
|   segment ID                        u64 |
|   file-name length              varint |
|   UTF-8 file-name bytes                |
|   document count                    u32 |
|   term count                        u32 |
|   segment length                    u64 |
|   segment checksum                  u32 |
|   ...                                  |
+----------------------------------------+
| Manifest CRC32 checksum          4 bytes|
+----------------------------------------+
```

Use:

```text
magic = "INVMAN\0\0"
manifest format version = 1
little-endian fixed-width integers
the existing canonical unsigned varint codec
```

The manifest checksum covers every byte before the checksum footer.

### Segment record semantics

```rust
pub struct SegmentMetadata {
    pub id: SegmentId,
    pub file_name: String,
    pub document_count: u32,
    pub term_count: u32,
    pub length_bytes: u64,
    pub checksum: u32,
}
```

The checksum is the checksum stored in the segment footer. This binds the
manifest record to the expected immutable segment contents more strongly than
file name and size alone.

### Manifest validation

Before writing, `Manifest::new` and the codec reject:

- zero, duplicate, or non-increasing segment IDs;
- empty, duplicate, absolute, or non-UTF-8 file names;
- file names containing separators, `.` or `..` path components;
- aggregate count and byte overflows.

While reading, the codec validates only what is required to establish byte
integrity and decode safely:

- exact published filename and nonzero generation;
- invalid magic or unsupported version;
- checksum mismatch;
- truncated or trailing data;
- header generation mismatch;
- segment-count conversion or allocation overflow;
- canonical varints and UTF-8 file names;
- nonzero segment IDs required by the `SegmentId` type;
- aggregate arithmetic overflow.

The reader does not repeat ordering, uniqueness, or path-safety validation that
the writer performed before calculating the checksum.

The later index-directory reader additionally rejects:

- file names outside the exact segment naming convention;
- duplicate referenced paths;
- a referenced file that is absent;
- segment size mismatch;
- segment footer-checksum mismatch;
- document-count or term-count mismatch with the opened segment.

Empty manifests are valid and represent an empty index.

### Publication

Manifest generation `N` is published as:

```text
build complete manifest bytes
    -> create unique temporary file
    -> write all bytes
    -> flush
    -> sync_all
    -> rename to manifest-N.bin
```

The final path must not already exist. Published manifest generations are never
replaced.

Directory synchronization can be researched with the later durability/WAL
phase. This phase must document that file synchronization is performed but does
not yet claim full power-loss durability for directory-entry publication on
every platform.

## Implementation Approach

## Phase 1: Physical Identity and Manifest Domain Types

### Goal

Define the vocabulary and invariants without changing query behavior.

### New files

#### `src/model/document_address.rs`

Add:

```rust
pub struct SegmentId(u64);

pub struct DocumentAddress {
    pub segment_id: SegmentId,
    pub local_document_id: u32,
}

pub struct AddressedPosting {
    pub address: DocumentAddress,
    pub term_frequency: u32,
}
```

Derive only traits actually required by comparison, testing, and display.
`SegmentId` must reject zero through a checked constructor rather than exposing
unchecked construction broadly.

#### `src/storage/manifest.rs`

Add the in-memory `Manifest` and `SegmentMetadata` types.

`Manifest::new` validates:

- segment IDs are strictly increasing;
- file names are unique and safe;
- aggregate document and byte counts use checked arithmetic.

Expose derived totals through methods instead of storing redundant values in
the manifest file:

```text
segment_count
total_document_count
total_segment_bytes
total_term_entries
```

`total_term_entries` is the sum of per-segment dictionary sizes, not a global
unique-term count.

### Tests

- valid empty and populated manifests;
- duplicate and unordered segment IDs;
- duplicate paths;
- unsafe paths;
- checked total overflow;
- deterministic address ordering.

### Success criteria

- Physical addresses cannot be confused with stable logical document IDs.
- Invalid manifest state cannot be constructed through the normal API.
- Existing segment and single-segment query tests remain unchanged.

---

## Phase 2: In-Memory Multi-Segment Query Engine

### Goal

Prove cross-segment query semantics before adding persistence.

### New file: `src/search/multi_segment_query_engine.rs`

Add:

```rust
pub struct MultiSegmentQueryEngine {
    segments: Vec<ManagedSegment>,
}

struct ManagedSegment {
    metadata: SegmentMetadata,
    query_engine: QueryEngine,
}
```

Construction receives a validated `Manifest` plus already resolved segment
paths for this phase. Every `QueryEngine` remains responsible for one segment.

Add:

```rust
pub struct MultiSegmentTermQueryResult<'a> {
    pub postings: MultiSegmentPostings<'a>,
    pub dictionary_stats: MultiSegmentDictionaryStats,
}
```

`MultiSegmentPostings` owns an iterator over matching segment decoders. It
decodes all postings from the current segment before advancing to the next
segment.

Iteration errors include the `SegmentId`:

```rust
pub struct SegmentDecodeError {
    pub segment_id: SegmentId,
    pub source: DecodeError,
}
```

Do not flatten segment failures into an unstructured string.

### Query behavior

For every visible segment:

1. call `query_term_with_stats`;
2. accumulate dictionary comparisons;
3. record whether the term exists;
4. retain the decoder and encoded-byte count when present.

Return a valid empty iterator when no segment contains the term.

### Existing changes

#### `src/search/query_engine.rs`

Keep the existing single-segment API. Add only small metadata accessors needed
by the multi-segment layer if they are not already available.

#### `src/storage/postings.rs`

Reuse the existing lazy decoder. Do not teach it about segments or manifests.

### Tests

- term present in every segment;
- term present in only the first, middle, or final segment;
- missing term;
- empty manifest;
- empty segment among populated segments;
- repeated local document IDs across segments produce distinct addresses;
- deterministic result ordering;
- term frequencies preserved;
- laziness: consuming one result does not consume later segment decoders;
- decode errors report the responsible segment ID;
- aggregate dictionary comparisons equal the sum of individual searches.

### Success criteria

- Cross-segment results match a simple in-memory oracle.
- The query API does not materialize all postings.
- Segment format version 1 is unchanged.

**Checkpoint:** Pause after this phase and inspect the API before persisting the
manifest. This is the cheapest point to correct identity or iterator mistakes.

---

## Phase 3: Manifest Codec

### Goal

Encode, validate, and reopen deterministic binary manifest generations.

### New files

- `src/storage/binary_file.rs`
- `src/storage/manifest_codec.rs`

Implement:

```rust
pub fn encode(path: impl AsRef<Path>, manifest: &Manifest) -> io::Result<()>;
pub fn decode(path: impl AsRef<Path>) -> io::Result<Manifest>;
```

Implement `BinaryFileCodec` for both segment and manifest formats. The generic
layer owns magic/version framing, checksum verification, checked primitive
decoding, and immutable publication. Each codec retains its own format-specific
validation and record layout.

Do not expose partially decoded manifests. Validation completes before a
`Manifest` is returned.

### Tests

- exact header and record layout;
- deterministic encoding;
- empty manifest;
- round trip;
- existing destination is not replaced;
- shared temporary-file cleanup;
- every truncated prefix is rejected without panic;
- checksum mismatch;
- unsupported version;
- noncanonical varints;
- duplicate IDs, duplicate paths, and unsafe file names rejected before write;
- trailing bytes;
- integer overflow and impossible segment count.

### Success criteria

- Re-encoding the same manifest produces identical bytes.
- Every malformed input returns a defined error without panic.
- Published manifest files are immutable.

---

## Phase 4: Persisted Index Snapshots and Manifest Publication

### Goal

Make a published manifest generation the only source of segment visibility.

### Runtime ownership

`IndexStorage` discovers and decodes the latest manifest. `IndexSnapshot` owns
the resulting runtime state:

```rust
pub struct ManagedSegment {
    metadata: SegmentMetadata,
    query_engine: QueryEngine,
}

pub struct IndexSnapshot {
    generation: Option<u64>,
    segments: Vec<ManagedSegment>,
}
```

The decoded `Manifest` is temporary. Its segment metadata is transferred into
the snapshot, and the manifest object is then dropped. A snapshot with no
generation and no segments represents an index that has not published its
first manifest.

The owning `MultiSegmentQueryEngine` type introduced in Phase 2 is folded into
`IndexSnapshot`; its query behavior and `ManagedSegment` values now live there.

### Latest-generation discovery

- inspect only `manifests`;
- accept only exact `manifest-{20 decimal digits}.bin` names;
- ignore temporary and unrelated files;
- detect numeric overflow;
- choose the greatest generation;
- fail if the selected manifest is invalid;
- never fall back to an older generation after selecting the latest.

Do not recursively scan directories.

### Source-of-truth rule

- if no manifest exists, `IndexSnapshot::new` returns an empty snapshot;
- if a manifest exists, only its segment records are visible;
- segment files not referenced by that manifest are ignored;
- never infer visibility by scanning the segment directory.

Before the first manifest-backed build, a pre-manifest development index must
be cleared and rebuilt. The build path reports this explicitly rather than
adopting those files.

### Segment opening

For each manifest record:

1. resolve the file under `segments`;
2. verify it remains under that directory;
3. check file length;
4. compare the stored footer checksum with manifest metadata;
5. open it through `SegmentReader`, verifying checksum and safe layout;
6. compare document and term counts;
7. retain the reader in manifest order.

Fail the complete snapshot open if any segment fails. Partial index visibility
is not allowed.

### Tests

- fresh empty directory opens an empty snapshot with no generation;
- latest generation selected;
- temporary and unrelated manifest files ignored;
- malformed latest manifest fails without fallback;
- unreferenced segment files remain invisible;
- missing or substituted segment rejected;
- size, checksum, document-count, and term-count mismatch rejected;
- segment traversal attempt rejected;
- empty published manifest opens an empty snapshot;
- an already-open snapshot remains pinned after a newer generation appears.

### Success criteria

- Every published `IndexSnapshot` represents exactly one manifest generation;
  the only exception is the explicit empty, uninitialized snapshot.
- The latest manifest is the complete source of visible index state.
- Opening never silently produces a partial, inferred, or stale snapshot.

### Manifest-backed build and CLI integration

Make the existing `build`, `lookup`, and `lookup-stats` commands use persisted
manifest generations.

#### Build publication protocol

For `build <corpus>`:

1. open the latest snapshot, or confirm that a new index has no segment files;
2. reserve the next segment ID and manifest generation;
3. build and publish the new immutable segment;
4. reopen the segment and collect its validated metadata;
5. construct a manifest containing the previous visible segments plus the new
   segment;
6. publish the new manifest generation last;
7. reopen that exact generation as a new `IndexSnapshot`;
8. replace the CLI's cached snapshot only after every preceding step succeeds;
9. report build success only after the manifest is published.

Generation 1 publishes the first segment. Every later successful build
publishes generation 2, 3, and so on.

If segment publication succeeds but manifest publication fails, the segment is
an invisible orphan. ID gaps are valid. No rollback, repair, or automatic
cleanup is added.

#### Existing commands only

```text
build <corpus>
lookup <term>
lookup-stats <term>
```

Do not add `assemble-index`, `lookup-index`, directory arguments, or parallel
single-segment command variants.

At startup, open the latest snapshot once. Before the first successful build,
lookups operate on an empty in-memory query engine. After a successful build,
subsequent lookups reuse the newly pinned snapshot and its memory maps.

#### CLI display

Continue displaying physical addresses explicitly:

```text
segment 2, document 41: frequency 3
```

Do not print a synthetic global document ID that could be mistaken for a stable
identifier.

#### Tests

- first build publishes segment 1 and manifest generation 1;
- later builds preserve earlier segments and publish increasing generations;
- shared and disjoint terms query correctly after each build;
- manifest publication occurs after segment publication;
- an unpublished segment remains invisible;
- a failed build does not replace the cached snapshot;
- process restart opens the same latest generation;
- repeated lookups reuse the same snapshot and memory maps;
- startup rejects legacy unmanifested segment directories.

#### Success criteria

- Every successful `build` atomically advances visible index state by one
  manifest generation.
- Existing lookup commands query only the latest successfully published state.
- No additional command family is introduced.

---

## Phase 5: Multi-Segment Observability

### Goal

Measure the physical work introduced by segment fan-out.

### Structured lookup boundary

Rewrite the existing lookup observability types instead of introducing a
parallel multi-segment hierarchy:

```rust
pub struct LookupStats {
    pub snapshot: SnapshotStats,
    pub query: TermLookupStats,
    pub timings: LookupTimings,
}
```

`LookupExecutor` is stateless. It accepts an already-open `IndexSnapshot`,
performs the lazy query, materializes the results for the CLI, and returns one
`LookupResult` containing both postings and statistics.

### Snapshot statistics

- manifest generation;
- visible segment count;
- total segment bytes;
- total documents;
- total per-segment term entries.

Snapshot construction is not part of lookup latency. The CLI opens or refreshes
the snapshot separately and reuses it across lookups, so lookup statistics must
not contain synthetic open/reuse flags or zero-valued open timings.

### Query work

- segments considered;
- segments containing the term;
- aggregate dictionary comparisons;
- matched documents;
- encoded postings bytes;

The matched-document count is also the number of successfully decoded postings;
do not store the same count twice.

### Timings

- dictionary lookup across all segments;
- postings decoding and materialization;
- total lookup.

Keep timing assembly inside `LookupExecutor`, not in `main.rs`. The current
sequential implementation should make timing boundaries explicit. Do not add
parallel execution in this phase.

### Tests

- aggregate counts equal per-segment sums;
- missing terms still report all segments considered;
- empty indexes report zero query work;
- decode failures do not produce success-shaped reports.

### Success criteria

- A lookup explains both latency and the amount of segment fan-out that caused
  it without duplicating instrumentation in the CLI.

---

## Phase 6: Correctness, Failure, and Scaling Validation

### Correctness oracle

Build a simple test oracle:

```text
BTreeMap<Term, Vec<AddressedPosting>>
```

Generate several independent corpora, add one segment per corpus through
repeated `build` operations, and compare every known and missing term against
the oracle.

Include:

- duplicate local IDs across segments;
- duplicate terms across segments;
- empty corpora/segments;
- varying term frequency;
- many tiny segments;
- terms appearing only at dictionary boundaries.

### Property-style randomized tests

Using deterministic seeds and existing test infrastructure:

- generate 1-32 segments;
- generate document and vocabulary distributions;
- compare complete query results;
- assert ordering and uniqueness of physical addresses;
- reopen encoded manifests before querying.

Do not add a property-testing dependency unless the manual deterministic
generator becomes a clear maintenance burden.

### Failure tests

Inject interrupted publication or malformed state around:

- segment published but manifest not published;
- manifest temporary file left behind;
- latest manifest truncated;
- referenced segment absent;
- segment replaced with another valid segment;
- unrelated directory entries;
- invalid and overflowing manifest names.

An unpublished segment must remain invisible. No repair or cleanup behavior is
required.

### Scaling benchmark

Create equivalent total document volumes split across:

```text
1, 2, 4, 8, 16, 32, 64, 128 segments
```

Measure:

- snapshot open and checksum-verification time;
- warm missing-term lookup;
- warm rare-, medium-, and common-term lookup;
- dictionary comparisons;
- segments containing the term;
- postings bytes and postings decoded;
- memory-map count and total bytes.

Keep total indexed documents and query workload constant while changing only
segment count.

Save raw and summarized results under:

```text
benchmarks/results/multi-segment-<date>.json
```

### Success criteria

- All oracle and corruption cases pass.
- Restarting consistently selects the same latest published generation.
- The segment-count latency curve is recorded without prematurely optimizing
  it.

## Error Model

Introduce typed errors at the remaining subsystem boundaries:

```text
IndexOpenError
IndexUpdateError
MultiSegmentQueryError
SegmentDecodeError
```

Errors must retain:

- manifest generation when known;
- segment ID and path when relevant;
- the underlying I/O or format error;
- the operation that failed.

The CLI may format these errors as text. Core APIs should not erase structure
into broad `String` errors.

## Performance Considerations

The initial design intentionally has:

```text
snapshot open cost = sum of segment checksum and layout-reading costs
term lookup cost   = sum of dictionary-search costs across segments
```

This is expected. Do not introduce:

- parallel opening;
- term Bloom filters;
- shared dictionaries;
- cached global term maps;
- speculative segment pruning;
- compaction;
- asynchronous refresh.

Those mechanisms would obscure the baseline this phase is intended to produce.

The implementation should still avoid accidental overhead:

- keep one memory map per visible segment;
- reuse snapshots across lookups;
- keep posting decoding lazy;
- do not copy term strings during lookup;
- do not materialize combined postings inside the query engine;
- use checked arithmetic for aggregate counts;
- preallocate only from validated bounded counts.

## Migration and Compatibility

- Existing `.idx` files remain valid without rewriting.
- The existing `build`, `lookup`, and `lookup-stats` command names remain.
- A pre-manifest index containing segment files is rejected and must be cleared
  and rebuilt.
- Existing segment files are never adopted automatically because their
  publication status cannot be proven.
- No stable external document IDs are promised.
- Every successful build after the first publishes the next manifest generation.
- Future compaction may replace segment IDs and physical document addresses in a
  newer manifest while older pinned generations remain readable.

## Documentation Changes

Update:

- `README.md` with the index-directory layout and persisted-manifest behavior;
- `docs/immediate-next-steps.md` when the phase is complete;
- the segment-format documentation to clarify that IDs are segment-local;
- observability documentation with multi-segment metric definitions.

## Final Completion Criteria

The multi-segment read phase is complete only when:

1. Repeated `build` commands publish increasing manifest generations.
2. Restarting opens the latest published manifest generation.
3. Exact-term queries return complete, ordered `AddressedPosting` values.
4. Posting decoding remains lazy at the query-engine boundary.
5. Repeated lookups reuse the pinned snapshot and all memory maps.
6. Missing, corrupt, substituted, or inconsistent files fail explicitly.
7. Unpublished segment files are invisible.
8. Existing command names and segment format remain compatible.
9. Results match a deterministic in-memory oracle.
10. Lookup cost is measured from 1 through 128 segments.

## Implementation Sequence Summary

```text
domain types
    -> in-memory multi-segment query
    -> manifest codec
    -> persisted index snapshot
    -> manifest-backed build and CLI
    -> observability
    -> correctness and scaling baseline
```

Pause after manifest decoding and again after the first manifest-backed build.
These are the points where visibility mistakes would otherwise propagate into
every later index generation.

## References

- Current short-term roadmap: `docs/immediate-next-steps.md`
- Long-term roadmap: `docs/search-engine-research-roadmap.md`
- Shared binary-file framing and publication: `src/storage/binary_file.rs`
- Segment format encoding and layout decoding: `src/storage/segment_codec.rs`
- Segment checksum verification and lazy record parsing:
  `src/storage/segment_reader.rs`
- Single-segment lookup: `src/search/query_engine.rs`
- Lookup orchestration and statistics: `src/search/lookup_executor.rs`
- Apache Lucene `LeafReaderContext.docBase`:
  <https://lucene.apache.org/core/9_1_0/core/org/apache/lucene/index/LeafReaderContext.html>
- Tantivy `DocAddress`:
  <https://docs.rs/tantivy/latest/tantivy/struct.DocAddress.html>
- Elasticsearch `_id`:
  <https://www.elastic.co/docs/reference/elasticsearch/mapping-reference/mapping-id-field>
