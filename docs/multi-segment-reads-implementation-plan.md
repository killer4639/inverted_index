# Multi-Segment Reads Implementation Plan

## Overview

Extend the engine from opening one immutable segment to opening a consistent,
immutable set of segments described by a binary manifest. Exact-term lookups
will search every visible segment and lazily return postings identified by a
segment ID and segment-local document ID.

This phase establishes the read-side architecture needed by future incremental
ingestion, WAL recovery, updates, deletes, and compaction. It does not implement
those write-side features.

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
- The CLI currently accepts one segment path for `lookup` and `lookup-stats`
  (`src/main.rs`).

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

An explicit `open_generation` API allows tests and future snapshot operations
to open an older manifest.

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

The completed phase supports:

```text
lookup-index <index-directory> <term>
lookup-index-stats <index-directory> <term>
```

Opening an index directory pins one immutable manifest generation and opens all
segments referenced by it. Repeated lookups reuse every open memory map.

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

- No WAL or crash recovery for acknowledged writes
- No mutable memtable
- No incremental append command
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

## Phase 4: Index Directory Reader

### Goal

Open a complete immutable index snapshot from disk.

### New file: `src/storage/index_directory.rs`

Add:

```rust
pub struct IndexDirectory {
    root: PathBuf,
}

impl IndexDirectory {
    pub fn open_latest(&self) -> Result<IndexSnapshot, IndexOpenError>;
    pub fn open_generation(
        &self,
        generation: u64,
    ) -> Result<IndexSnapshot, IndexOpenError>;
}

pub struct IndexSnapshot {
    manifest: Manifest,
    query_engine: MultiSegmentQueryEngine,
}
```

`IndexSnapshot` owns all segment readers and therefore pins that generation.
No refresh occurs behind the caller's back.

### Latest-generation discovery

- inspect only `manifests`;
- accept only exact `manifest-{20 decimal digits}.bin` names;
- ignore temporary and unrelated files;
- detect numeric overflow;
- choose the greatest generation;
- fail if that manifest is invalid;
- return an explicit empty-index error when no manifest exists.

Do not recursively scan directories.

### Segment opening

For each manifest record:

1. resolve the file under `segments`;
2. verify it remains under that directory;
3. check file length;
4. check the stored footer checksum against manifest metadata;
5. open through `QueryEngine`, verifying the segment checksum and safe layout;
6. compare document and term counts;
7. retain the reader in manifest order.

Fail the entire snapshot open if any segment fails. Partial index visibility is
not allowed.

### Tests

- latest generation selected;
- explicit old generation remains readable;
- temporary manifests ignored;
- malformed published latest manifest fails without fallback;
- missing referenced segment;
- valid but substituted segment rejected by metadata mismatch;
- size, checksum, document-count, and term-count mismatch;
- segment traversal attempt rejected;
- empty manifest opens an empty index;
- reader remains usable after a newer manifest appears.

### Success criteria

- One `IndexSnapshot` always represents exactly one manifest generation.
- Opening never silently produces a partial or stale snapshot.

---

## Phase 5: Minimal Index Assembly and CLI Integration

### Goal

Make multi-segment reads usable without implementing incremental ingestion.

### Minimal assembly API

Add a narrowly scoped helper that creates generation 1 from existing immutable
segments:

```text
assemble-index <index-directory> <segment-path>...
```

Behavior:

1. require a new or empty index directory;
2. verify the checksum and safe layout of every input segment;
3. assign strictly increasing `SegmentId` values beginning at 1;
4. copy each segment to its generated immutable name under `segments`;
5. synchronize completed segment copies;
6. publish manifest generation 1 last;
7. never modify the source segment files.

This is fixture/bootstrap functionality, not incremental ingestion. Publishing
generation 2 belongs to the subsequent append phase.

### Lookup commands

Add:

```text
lookup-index <index-directory> <term>
lookup-index-stats <index-directory> <term>
```

Keep:

```text
build <corpus> <segment>
lookup <segment> <term>
lookup-stats <segment> <term>
```

This preserves the single-segment baseline and allows direct comparison.

### Executor boundary

Add `MultiSegmentLookupExecutor` rather than making the existing
`LookupExecutor` handle two unrelated path types.

The executor:

- reuses an open `IndexSnapshot` when the directory and generation are
  unchanged;
- consumes the lazy multi-segment iterator;
- materializes `AddressedPosting` only for CLI display;
- returns completed multi-segment statistics.

### CLI display

Display physical addresses explicitly:

```text
segment 2, document 41: frequency 3
```

Do not print a synthetic global document ID that could be mistaken for a stable
identifier.

### Tests

- assemble two independently built segments;
- query shared and disjoint terms;
- preserve source segments;
- refuse existing non-empty destination state;
- repeated lookups reuse the same snapshot and memory maps;
- single-segment commands retain current behavior.

### Success criteria

- A user can construct and query a multi-segment index using CLI commands.
- Existing single-segment workflows remain compatible.

---

## Phase 6: Multi-Segment Observability

### Goal

Measure the physical work introduced by segment fan-out.

### New file: `src/search/multi_segment_stats.rs`

Group statistics as:

```rust
pub struct MultiSegmentLookupStats {
    pub snapshot: SnapshotStats,
    pub query: MultiSegmentTermStats,
    pub timings: MultiSegmentLookupTimings,
}
```

### Snapshot statistics

- manifest generation;
- snapshot opened or reused;
- visible segment count;
- total segment bytes;
- total documents;
- total per-segment term entries.

### Query work

- segments considered;
- segments containing the term;
- aggregate dictionary comparisons;
- matched documents;
- encoded postings bytes;
- postings decoded;
- per-segment matches and bytes for diagnostic output.

Do not use segment IDs as future metrics labels. Per-segment details belong in
the per-operation report, not an unbounded global metric series.

### Timings

- manifest discovery and decoding;
- segment opening and checksum verification;
- dictionary lookup across all segments;
- postings decoding and materialization;
- total lookup.

The current sequential implementation should make timing boundaries explicit.
Do not add parallel execution in this phase.

### Tests

- aggregate counts equal per-segment sums;
- missing terms still report all segments considered;
- reused snapshots report zero open time;
- empty indexes report zero query work;
- failed opens do not produce success-shaped reports.

### Success criteria

- A lookup explains both latency and the amount of segment fan-out that caused
  it.

---

## Phase 7: Correctness, Failure, and Scaling Validation

### Correctness oracle

Build a simple test oracle:

```text
BTreeMap<Term, Vec<AddressedPosting>>
```

Generate several independent corpora, build one segment per corpus, assemble an
index, and compare every known and missing term against the oracle.

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

Inject failures or malformed state around:

- segment copied but manifest not published;
- manifest temporary file left behind;
- latest manifest truncated;
- referenced segment absent;
- segment replaced with another valid segment;
- duplicate generation filename;
- unrelated directory entries;
- invalid and overflowing manifest names.

An unpublished segment must remain invisible. Cleanup is intentionally deferred.

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
- Old manifest generations remain reproducible.
- The segment-count latency curve is recorded without prematurely optimizing
  it.

## Error Model

Introduce typed errors at new subsystem boundaries:

```text
ManifestEncodeError
ManifestDecodeError
IndexOpenError
MultiSegmentQueryError
SegmentDecodeError
IndexAssemblyError
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
- Existing single-segment commands remain supported.
- `assemble-index` copies existing segment bytes and records their existing
  footer checksums.
- No stable external document IDs are promised.
- Future append work publishes generation 2 and later using the same manifest
  format.
- Future compaction may replace segment IDs and physical document addresses in a
  newer manifest while older pinned generations remain readable.

## Documentation Changes

Update:

- `README.md` with index-directory layout and new commands;
- `docs/immediate-next-steps.md` when the phase is complete;
- the segment-format documentation to clarify that IDs are segment-local;
- observability documentation with multi-segment metric definitions.

## Final Completion Criteria

The multi-segment read phase is complete only when:

1. Multiple existing immutable segments can be assembled into one index
   directory.
2. The latest or an explicit older manifest generation can be opened.
3. Exact-term queries return complete, ordered `AddressedPosting` values.
4. Posting decoding remains lazy at the query-engine boundary.
5. Repeated lookups reuse the pinned snapshot and all memory maps.
6. Missing, corrupt, substituted, or inconsistent files fail explicitly.
7. Unpublished segment files are invisible.
8. Existing single-segment behavior and format remain compatible.
9. Results match a deterministic in-memory oracle.
10. Lookup cost is measured from 1 through 128 segments.

## Implementation Sequence Summary

```text
domain types
    -> in-memory multi-segment query
    -> manifest codec
    -> index-directory snapshot
    -> assembly and CLI
    -> observability
    -> correctness and scaling baseline
```

Pause after the in-memory query phase and again after manifest decoding. These
are the two points where mistakes would otherwise propagate into every future
write, recovery, and compaction feature.

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
