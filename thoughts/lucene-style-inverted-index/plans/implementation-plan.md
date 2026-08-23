# Lucene-Style Inverted Index Implementation Plan

## Overview

Build the smallest useful part of a search engine in Rust: tokenize documents, create one
immutable postings segment, reopen it through a memory map, and retrieve postings by exact term.

Lucene's SimpleText postings writer and reader are reference material for understanding the
field-to-term-to-postings hierarchy, streaming serialization, reader/writer symmetry, and
immutable segment boundaries. This project is not a class-for-class Lucene port and does not aim
for SimpleText byte compatibility.

## Desired End State

The completed program can:

1. Treat each line of a text corpus as a document.
2. Assign sequential `u32` document IDs.
3. Validate each document and tokenize it without changing token bytes or case.
4. Count each term's frequency within each document.
5. Build one deterministic, immutable, versioned segment file.
6. Store sorted terms and delta-encoded postings.
7. Reopen the segment through a read-only memory map.
8. Find a term with binary search.
9. Lazily iterate its `(document ID, term frequency)` postings.
10. Report indexing throughput, segment size, and lookup latency.
11. Use repeatable experiments to identify and retain performance improvements.

The memory-mapped reader's results must match a simple in-memory implementation used as a
correctness oracle.

## Scope

### Included

- Whole-file validation against a deliberately narrow document grammar
- Deterministic, case-preserving tokenization
- Sequential document IDs
- Per-document term frequencies
- Two-phase indexing: accumulation followed by finalization
- Sorted term dictionary
- Variable-length unsigned integer encoding
- Delta-encoded document IDs
- One immutable, versioned segment file
- Segment checksum and structural validation
- Read-only memory-mapped access
- Binary-search term lookup
- Lazy postings iteration
- Command-line build, lookup, and inspect operations
- Unit, integration, corruption, and benchmark coverage

### Baseline Hard Stopping Boundary

- No deletes
- No mutable segments
- No segment merging
- No stored document contents
- No positions or phrase queries
- No token offsets or payloads
- No scoring, ranking, norms, or impacts
- No skip lists
- No finite-state transducer
- No Lucene API compatibility
- No exact Lucene SimpleText file compatibility

These exclusions define the correct baseline through Phase 9. The later performance research
track may experimentally revisit physical data structures such as block indexes, skip data, or
alternative dictionaries, but it does not expand the product into deletes, merging, stored
fields, phrase search, or ranking.

## Core Invariants

- One input line represents one document.
- The complete input file is validated before any document ID is assigned or posting is built.
- Every document contains at least one ASCII-alphanumeric word.
- Words contain only ASCII letters and digits.
- Words are separated with Rust's standard `split_whitespace()` behavior.
- Leading, trailing, repeated, and non-space whitespace is ignored.
- Punctuation, symbols, and non-ASCII characters inside tokens make the input file invalid.
- Document IDs are assigned as `0, 1, 2, ...`.
- Token bytes and case are preserved exactly; `Rust`, `rust`, and `RUST` are distinct terms.
- A lookup term must be one non-empty ASCII-alphanumeric word and is not normalized.
- Repeated terms in one document increase term frequency instead of creating duplicate postings.
- Terms are serialized in bytewise sorted order.
- Document IDs within a posting list are strictly increasing.
- Every term frequency is greater than zero.
- Unknown terms return an empty result rather than an error.
- A successfully created segment never changes.
- Every offset and length read from disk is validated before it is used.
- Malformed input produces a defined error rather than a panic or infinite loop.

## Input Validation Contract

Input validation is a separate operation that completes before indexing starts. A file is either
fully valid or not indexed at all.

For example:

| Document | Valid | Reason |
|---|---|---|
| `Rust is Fast` | Yes | ASCII-alphanumeric words |
| `R2D2 uses Rust2024` | Yes | Digits are allowed inside words |
| `Rust  is fast` | Yes | Repeated whitespace is ignored |
| ` Rust is fast ` | Yes | Leading and trailing whitespace is ignored |
| `Rust\tis fast` | Yes | `split_whitespace()` accepts tabs |
| `Rust is fast.` | No | Punctuation |
| `café` | No | Non-ASCII characters |
| empty line | No | Blank documents are invalid |

An empty file contains zero documents and is valid. A line ending separates documents and is not
part of a document. A final line ending is allowed.

If any document is invalid:

- Validation reports the document line and the reason.
- The entire file is rejected.
- No document ID is assigned.
- No postings or segment output are created.
- Indexing must not silently skip the invalid document.

## Implementation Approach

Correctness comes before storage optimization. We first build a deliberately simple in-memory
index that serves as an oracle. We then separate accumulation from finalization, introduce and
test the integer codec independently, write the immutable format, and finally build a
memory-mapped reader over the same format.

Each phase is a vertical, testable step. A phase is complete only when its success criteria pass.
We pause at each phase boundary before introducing the next concept.

## Phase 0: Freeze Semantics and Format Decisions

**Status: Complete**

### Goal

Turn assumptions into explicit contracts before implementation begins.

### Work

1. Define the document grammar, whole-file validation, and case-preserving tokenization policy.
2. Define document ID assignment and integer limits.
3. Define exact lookup behavior for empty and missing terms.
4. Define the logical posting model as `(document ID, term frequency)`.
5. Define deterministic term and posting ordering.
6. Record the segment layout and byte order.
7. Record corruption checks and error behavior.
8. Keep the baseline hard stopping boundary visible in the design.

### Rust and Systems Concepts

- Logical model versus physical representation
- Invariants versus implementation details
- Why immutability simplifies concurrent and memory-mapped readers
- Why a file format requires versioning from its first release

### Success Criteria

- A tiny example corpus can be evaluated manually.
- Its exact case-preserved tokens, frequencies, sorted terms, and posting lists are unambiguous.
- No format or behavioral decision needed by later phases remains unresolved.

### Final Decision Record

| Area | Final decision |
|---|---|
| Input unit | Each line is one document |
| File validation | Validate the entire file before assigning IDs or building postings |
| Valid document | One or more ASCII-alphanumeric words separated by whitespace |
| Whitespace | Use `split_whitespace()`; ignore leading, trailing, and repeated whitespace |
| Invalid document | Blank/whitespace-only line or a token containing punctuation, symbols, or non-ASCII characters |
| Invalid file behavior | Reject the whole file and create no segment |
| Empty file | Valid corpus containing zero documents |
| Token handling | Preserve bytes and case exactly; perform no normalization |
| Query validation | Require exactly one non-empty ASCII-alphanumeric word |
| Empty query | Return an invalid-query error |
| Case semantics | Case-sensitive; `Rust`, `rust`, and `RUST` are distinct |
| Document ID assignment | Sequential IDs beginning at zero, local to one segment |
| Document ID type | `u32` |
| Term-frequency type | `u32`; zero is not a valid stored frequency |
| Global counts | `u64` |
| File offsets and lengths | `u64` |
| In-memory slice indexes | `usize`, produced only through checked conversion |
| Missing valid term | Successful lookup with no term entry and an empty postings iterator |
| Logical posting | `(document ID, term frequency)` for a document containing the term |
| Term ordering | Ascending lexicographic order of exact term bytes |
| Posting ordering | Strictly increasing document ID within each term |
| Fixed-width byte order | Little-endian |
| Compact integers | Explicit unsigned variable-length integer format |
| Segment layout | Header, term-offset table, sorted term records, postings region, checksum footer |
| Corrupt segment | Return a specific format error; never panic or silently return empty results |
| Baseline product | One immutable, exact-term postings segment |

### Phase 0 Example

For this valid corpus:

```text
Rust is Fast
Rust Rust is Safe
Search is Fast
```

document IDs are `0`, `1`, and `2`, and the logical index is:

```text
Fast   -> [(0, 1), (2, 1)]
Rust   -> [(0, 1), (1, 2)]
Safe   -> [(1, 1)]
Search -> [(2, 1)]
is     -> [(0, 1), (1, 1), (2, 1)]
```

The term order shown above follows exact byte ordering: uppercase ASCII letters sort before
lowercase ASCII letters. Searching for `rust` returns an empty postings iterator because only
`Rust` exists. Searching for an empty string is an invalid query.

**Implementation checkpoint:** Approved. Phase 1 is ready to begin.

---

## Phase 1: Build the In-Memory Correctness Oracle

### Goal

Build the simplest index that can answer an exact term lookup, without disk storage or
compression.

### Work

1. Define domain types for document IDs, term frequencies, and postings.
2. Validate the complete input before indexing.
3. Implement deterministic, case-preserving tokenization with `split_whitespace()`.
4. Index one document at a time.
5. Count duplicate terms locally within each document.
6. Build ordered posting lists.
7. Expose exact, case-sensitive term lookup.
8. Test the implementation with a hand-computed corpus.

### Rust and Systems Concepts

- Structs and domain-specific newtypes
- Borrowed `&str` versus owned `String`
- Ownership of terms across indexing stages
- `HashMap` versus `BTreeMap`
- Iterator transformations
- `Option` and `Result`
- Deterministic tests

### Success Criteria

- Invalid documents reject the entire input before indexing.
- Repeated terms produce one posting with the correct frequency.
- Shared terms produce postings in increasing document order.
- Case is preserved, and invalid punctuation is rejected.
- Missing-term lookup returns no postings.
- All results match the manually computed corpus.

### Deferred Phase 1 Follow-Ups

These are intentionally postponed while the core in-memory index and query behavior are being
learned:

- Make the CLI own one persistent `InvertedIndex` across commands.
- Prevent partial index mutation if reading a new file fails midway.
- Make the boundary between validated input and indexing explicit.
- Return a defined error before the `u32` document ID overflows.

**Implementation checkpoint:** Pause for manual confirmation before Phase 2.

---

## Phase 2: Introduce Two-Phase Indexing

### Goal

Separate mutable document ingestion from immutable segment finalization.

### Accumulation Phase

For every document:

1. Tokenize the already-validated document without changing its words.
2. Count its terms locally.
3. Add one posting per distinct term.
4. Preserve increasing document IDs.

### Finalization Phase

1. Sort terms lexicographically.
2. Validate posting order and frequencies.
3. Compute document, term, and posting statistics.
4. Move accumulated data into an immutable intermediate segment representation.
5. Prevent additional documents from being added after finalization.

### Rust and Systems Concepts

- Mutable build state versus immutable final state
- Moving ownership during finalization
- Typestate-style design
- Local aggregation to avoid duplicate postings
- Sorting and memory complexity

### Success Criteria

- Finalized results exactly match the Phase 1 oracle.
- Terms are bytewise sorted.
- Posting document IDs are strictly increasing.
- Frequencies are non-zero.
- Finalization cannot silently run twice.
- Indexing cannot continue after finalization.

**Implementation checkpoint:** Pause for manual confirmation before Phase 3.

---

## Phase 3: Implement the Integer and Postings Codec

### Goal

Build and verify the binary primitives independently of file I/O.

### Encoding Rules

- Counts and lengths use unsigned variable-length integers.
- Document IDs use deltas from the previous document ID.
- Frequencies use unsigned variable-length integers.
- Each posting is encoded as `document delta` followed by `term frequency`.

For document IDs `[3, 10, 11, 25]`, the encoded deltas are `[3, 7, 1, 14]`.

### Work

1. Encode an unsigned integer into one or more bytes.
2. Decode an unsigned integer from a byte slice.
3. Detect truncated encodings.
4. Detect overflow and overlong encodings.
5. Encode complete posting lists.
6. Decode postings with a stateful iterator.
7. Reconstruct absolute document IDs with checked arithmetic.

### Required Boundary Cases

- `0`
- `1`
- `127`
- `128`
- `16_383`
- `16_384`
- `u32::MAX`
- A truncated continuation sequence
- An encoding that exceeds the supported integer width

### Rust and Systems Concepts

- Bit masks and shifts
- Checked integer arithmetic
- Byte slices
- Stateful iterators
- Delta encoding and ordered data
- Error-aware decoding

### Success Criteria

- Decoding an encoded posting list reproduces the original postings.
- Every required boundary value round-trips.
- Truncated and overflowing input returns an explicit error.
- The decoder cannot loop forever on malformed bytes.

**Implementation checkpoint:** Pause for manual confirmation before Phase 4.

---

## Phase 4: Write the Immutable Segment

### Goal

Serialize the finalized logical index into one deterministic, versioned segment file.

### Segment Layout

The segment contains these ordered regions:

1. **Header**
   - Magic bytes
   - Format version
   - Document count
   - Term count
   - Dictionary offset
   - Postings offset
2. **Term-offset table**
   - One fixed-width offset for each sorted term
3. **Term records**
   - Term byte length
   - UTF-8 term bytes
   - Document frequency
   - Relative postings offset
   - Postings byte length
4. **Postings region**
   - Repeated document delta and frequency pairs
5. **Footer**
   - Checksum over the preceding segment bytes

The offset table enables binary search while allowing variable-length term records.

### Work

1. Define named constants for magic bytes and format version.
2. Calculate or buffer each region without unchecked offset arithmetic.
3. Write integer fields with an explicit byte order.
4. Write term records in deterministic sorted order.
5. Write delta-encoded postings.
6. Write section offsets and lengths.
7. Compute and append the checksum.
8. Flush and close the completed file.

### Rust and Systems Concepts

- `Write` and buffered output
- Explicit byte order
- Relative versus absolute offsets
- Checked offset calculations
- Error propagation with `?`
- Deterministic serialization
- Preventing partial state from appearing valid

### Success Criteria

- Writing the same logical index twice produces identical bytes.
- Header counts and offsets agree with actual file contents.
- Every term record points to its own postings range.
- The checksum covers the intended bytes.
- The output can be inspected using only the format specification.

**Implementation checkpoint:** Pause for manual confirmation before Phase 5.

---

## Phase 5: Build a Safe Memory-Mapped Segment Reader

### Goal

Open a segment without reconstructing the complete index in heap collections.

### Work

1. Open the segment read-only.
2. Create a read-only memory map.
3. Validate magic bytes and format version.
4. Parse header counts and section boundaries.
5. Validate all offset and length arithmetic.
6. Verify the checksum.
7. Expose validated dictionary and postings slices.
8. Reject corruption before lookup begins.

### Safety Boundary

Creating a memory map requires a small, isolated unsafe operation. Its precondition is:

> A mapped segment is immutable and is not truncated or replaced while the reader holds the map.

All parsing and lookup logic above this boundary remains safe Rust.

### Rust and Systems Concepts

- Virtual memory and page faults
- Why mmap does not immediately copy the entire file into RAM
- Safe wrappers around unsafe operations
- Lifetimes tied to mapped storage
- Slice bounds and checked offsets
- Construction-time validation

### Success Criteria

- A valid segment opens successfully.
- Wrong magic bytes and unsupported versions are rejected.
- Truncated headers and sections are rejected.
- Invalid offsets and lengths are rejected.
- Checksum mismatches are rejected.
- Malformed files do not cause reader panics.

**Implementation checkpoint:** Pause for manual confirmation before Phase 6.

---

## Phase 6: Implement Term Lookup and Lazy Postings Iteration

### Goal

Search the mapped dictionary and decode only the requested posting list.

### Lookup Flow

1. Validate that the query is one non-empty ASCII-alphanumeric word without changing its case.
2. Binary-search the term-offset table.
3. Compare mapped term bytes without allocating a new `String`.
4. Locate the term's validated postings range.
5. Return a postings iterator over that range.
6. Decode document deltas and frequencies as the caller advances.

### Iterator State

The postings iterator stores only:

- Its remaining byte slice
- The previous absolute document ID
- The remaining posting count
- Its decoding error state

It does not allocate a vector containing the complete posting list.

### Rust and Systems Concepts

- Binary search over variable-length records
- Borrowed data tied to the segment lifetime
- Zero-copy parsing
- Custom `Iterator` implementations
- Iterators whose items contain `Result`
- Stateful delta reconstruction

### Success Criteria

- Every mapped term lookup matches the Phase 1 oracle.
- First, middle, and last dictionary terms are found.
- A missing term returns no postings.
- Missing-term lookup performs no postings decoding.
- Posting iteration is lazy and allocation-free.
- Corrupt posting bytes return an explicit decoding error.

**Implementation checkpoint:** Pause for manual confirmation before Phase 7.

---

## Phase 7: Connect the End-to-End Command-Line Interface

### Goal

Expose the smallest useful public workflow without mixing CLI concerns into the indexing library.

### Commands

- `build <corpus.txt> <segment.idx>`
- `lookup <segment.idx> <term>`
- `inspect <segment.idx>`

### Build Behavior

- Treat each input line as a document.
- Build and finalize one segment.
- Print document count, term count, segment bytes, and elapsed build time.

### Lookup Behavior

- Open the segment through the mmap reader.
- Validate the case-sensitive query term.
- Print matching document IDs and term frequencies.

### Inspect Behavior

- Print format version and section statistics.
- Validate the segment.
- Avoid dumping the complete index by default.

### Rust and Systems Concepts

- Command-line arguments
- Filesystem paths
- Layered error messages
- Keeping user-interface logic out of reusable library code

### Success Criteria

- A segment can be built in one process.
- A new process can reopen it and retrieve correct postings.
- Inspect reports metadata without reconstructing all postings.
- CLI errors identify whether failure came from input, format validation, or lookup.

**Implementation checkpoint:** Pause for manual confirmation before Phase 8.

---

## Phase 8: Harden Correctness and Corruption Handling

### Goal

Prove that all layers agree and that malformed files fail safely.

### Unit Coverage

- Whole-file document validation
- Case-preserving tokenization
- Per-document frequency counting
- Term ordering
- Varint boundaries
- Delta encoding
- Posting iteration
- Writer determinism
- Header parsing
- Binary-search hits and misses

### Integration Coverage

- Build, close, reopen, and lookup
- Empty corpus
- Invalid document rejects the whole file before indexing
- One document with one term
- Many repeated terms
- Terms shared across documents
- Non-ASCII text is rejected before indexing
- Values around varint boundaries
- A segment spanning multiple operating-system pages

### Corruption Coverage

Mutate and verify rejection of:

- Magic bytes
- Format version
- Section offsets
- Section lengths
- Term lengths
- Posting lengths
- Varint continuation bytes
- Checksum
- End-of-file boundary

### Success Criteria

- The mmap reader agrees with the in-memory oracle across a generated corpus.
- Every known corruption case returns a specific error.
- No malformed test segment causes a panic, out-of-bounds access, or infinite loop.

**Implementation checkpoint:** Pause for manual confirmation before Phase 9.

---

## Phase 9: Benchmark and Document the Result

### Goal

Measure the complete system only after correctness is established.

### Metrics

- Documents indexed per second
- Input bytes indexed per second
- Total segment bytes
- Segment bytes per document
- Segment size divided by input size
- Existing-term lookup latency
- Missing-term lookup latency
- Postings decoded per second
- Segment open, map, and validation latency

### Benchmark Rules

- Use release builds.
- Use one identified, fixed public text corpus.
- Record corpus bytes, document count, and vocabulary size.
- Use a deterministic set of common, rare, and missing query terms.
- Measure building separately from lookup.
- Distinguish first-access behavior from warm page-cache behavior.
- Prevent benchmark work from being optimized away.
- Record relevant machine and operating-system details.

### Rust and Systems Concepts

- Throughput versus latency
- Allocation costs
- Operating-system page cache behavior
- Cold versus warm mmap access
- Reproducible benchmark design

### Success Criteria

- Results report documents per second, segment size, and lookup latency.
- Corpus and environment details make the measurements reproducible.
- The final write-up explains observed tradeoffs rather than reporting numbers without context.

**Implementation checkpoint:** The correct baseline is complete after this phase. Freeze its
results before beginning the performance research track.

---

## Performance Research Track

The phases below optimize the completed baseline without changing its externally visible
semantics. They are deliberately experimental: concurrency, asynchronous I/O, compression, and
specialized data structures are hypotheses, not requirements.

Every experiment must:

1. State the bottleneck it is intended to remove.
2. Predict which metric should improve and why.
3. Change one major variable at a time.
4. Preserve results against the correctness oracle.
5. Compare against the frozen Phase 9 baseline on the same corpus and machine.
6. Report throughput, latency distribution, memory, file size, and relevant CPU or I/O counters.
7. Include implementation complexity and maintenance cost in the decision.
8. Be retained only when the evidence justifies its tradeoffs.

Rejected experiments remain documented with their measurements and explanation. A negative result
is useful research and prevents repeating an attractive but unsuitable optimization.

## Phase 10: Build the Performance Laboratory

### Goal

Make performance changes measurable, reproducible, and attributable.

### Work

1. Freeze representative small, medium, and large corpora.
2. Define common-term, rare-term, missing-term, and long-postings query sets.
3. Separate build, open, lookup, and postings-iteration benchmarks.
4. Measure median and tail latency rather than only averages.
5. Record peak memory and allocation counts where tooling permits.
6. Collect CPU profiles and identify hot call paths.
7. Collect I/O observations for cold and warm page-cache runs.
8. Add a repeatable comparison report for baseline versus experiment.
9. Define noise thresholds so insignificant changes are not treated as wins.

### Questions to Answer

- Is indexing CPU-bound, allocation-bound, memory-bandwidth-bound, or storage-bound?
- Is lookup dominated by dictionary comparisons, page faults, or postings decoding?
- Which corpus and query characteristics change the bottleneck?
- What is the theoretical minimum work for each operation?

### Rust and Systems Concepts

- Sampling and instrumentation
- Median, percentiles, variance, and confidence
- CPU time versus wall-clock time
- Allocation and memory-bandwidth costs
- Cold-cache versus warm-cache behavior
- Experimental controls

### Success Criteria

- The Phase 9 baseline can be reproduced within an agreed noise range.
- Profiles identify measured hot paths rather than suspected ones.
- Every later optimization has a standard before-and-after report.

**Implementation checkpoint:** Pause for manual confirmation before Phase 11.

---

## Phase 11: Optimize the Single-Threaded Baseline

### Goal

Remove unnecessary work before adding concurrency or asynchronous execution.

### Candidate Experiments

Only run candidates supported by Phase 10 profiles:

1. Reuse per-document term-counting buffers and collections.
2. Reduce temporary `String` creation during tokenization.
3. Compare standard and specialized hashers for term accumulation.
4. Reserve collection capacity from measured corpus statistics.
5. Replace repeated copying with borrowed slices where lifetimes remain clear.
6. Batch output writes and tune buffer sizes.
7. Reduce branches and bounds checks in varint decoding without weakening validation.
8. Compare alternate term-record layouts for cache locality.
9. Separate cold validation work from hot lookup work where safe.
10. Apply compiler and release-profile settings only after algorithmic work is measured.

### Why This Precedes Concurrency

Parallelizing waste multiplies memory traffic and synchronization. A faster single-threaded unit
also raises the ceiling of every later parallel implementation.

### Success Criteria

- Each retained change has an isolated benchmark result.
- Correctness and corruption behavior remain unchanged.
- Memory use and segment size regressions are reported alongside speedups.
- Changes below the noise threshold are reverted or documented as neutral.

**Implementation checkpoint:** Pause for manual confirmation before Phase 12.

---

## Phase 12: Research Segment Layout and Compression

### Goal

Reduce bytes read, decoded, and retained while measuring the CPU cost of denser encodings.

### Candidate Experiments

1. Block postings into independently decodable groups.
2. Compare scalar varints with group or block-oriented integer encodings.
3. Compare delta widths and bit packing within posting blocks.
4. Front-code neighboring sorted terms.
5. Compare fixed-width and compact dictionary offsets.
6. Add a sparse block index over the term dictionary.
7. Add skip information only for long posting lists.
8. Compare whole-file and per-block checksums.
9. Align selected regions or blocks to measured access patterns.
10. Evaluate a Bloom filter for missing-term-heavy workloads.

### Rules

- Every incompatible layout gets a new format version.
- The original format remains readable while an experimental version is evaluated.
- File size alone is not success; lookup and decode costs must also be measured.
- Extra metadata must earn its space on realistic workloads.

### Rust and Systems Concepts

- Cache lines and locality
- Entropy, integer distributions, and compression ratios
- Random access versus compression density
- Block indexes and skip data
- False positives in probabilistic filters
- Format evolution

### Success Criteria

- Every format variant round-trips through the oracle.
- Size, build speed, lookup latency, and decode throughput are compared together.
- The chosen layout states the workload for which it wins and the workload for which it loses.

**Implementation checkpoint:** Pause for manual confirmation before Phase 13.

---

## Phase 13: Research Parallel Index Construction

### Goal

Use multiple CPU cores for indexing while preserving deterministic document IDs and segment bytes.

### Starting Design

1. Partition the corpus into deterministic document ranges.
2. Give each worker independent tokenizer and accumulation state.
3. Produce sorted local term/postings runs.
4. Merge local runs in document-range order.
5. Serialize one deterministic immutable segment.

Thread-local accumulation is the default experiment because it avoids locking a global term map on
every token.

### Experiments

1. Compare one worker with increasing worker counts.
2. Vary partition sizes and merge fan-in.
3. Compare static partitioning with work stealing.
4. Measure the cost of local dictionaries and final merging.
5. Measure peak memory as worker count increases.
6. Determine where storage bandwidth or memory bandwidth stops scaling.
7. Test bounded channels only where pipelining provides measured overlap.

### Correctness Requirements

- Document IDs remain stable and sequential.
- Posting lists remain strictly ordered.
- Output is deterministic regardless of worker scheduling.
- Worker errors cancel the build and do not publish a partial segment.

### Rust and Systems Concepts

- Threads and scoped parallelism
- `Send` and `Sync`
- Data parallelism
- Thread-local state
- Deterministic parallel reduction
- Channels, backpressure, and cancellation
- Amdahl's law and bandwidth saturation

### Success Criteria

- Scaling is reported for each worker count.
- Speedup is compared with CPU utilization, peak memory, and merge cost.
- The selected worker strategy has a measured advantage over the optimized single-threaded build.
- Determinism and all correctness tests still pass.

**Implementation checkpoint:** Pause for manual confirmation before Phase 14.

---

## Phase 14: Research I/O, Memory Mapping, and Asynchronous Pipelines

### Goal

Determine whether I/O overlap or access-pattern hints improve build and query performance.

### Candidate Experiments

1. Tune buffered input and output sizes.
2. Pipeline corpus reading, tokenization, merging, and serialization with bounded queues.
3. Compare sequential writes with staged region buffers.
4. Measure mmap page-fault behavior for dictionary and postings access.
5. Evaluate safe prefetching or platform access hints where supported.
6. Compare mmap lookup with explicit positioned reads for cold random access.
7. Investigate asynchronous file I/O only when profiles show idle time that can be overlapped.
8. Measure batched independent queries under synchronous and asynchronous orchestration.

### Async Decision Rule

Async is not automatically faster for local indexing. Tokenization and decoding are CPU work, and
memory-mapped lookup is not naturally an async operation. An async design is retained only if a
real workload contains enough independent waiting I/O to improve throughput or resource usage
after runtime and coordination overhead.

### Rust and Systems Concepts

- Blocking versus asynchronous I/O
- Data parallelism versus task concurrency
- Bounded pipelines and backpressure
- Page faults, readahead, and prefetching
- Storage queue depth
- Runtime and context-switch overhead

### Success Criteria

- The dominant I/O path is identified with measurements.
- Buffering and pipeline changes include CPU, memory, and latency results.
- Async is either justified by evidence or explicitly rejected with evidence.
- No pipeline can publish a partial or corrupt segment.

**Implementation checkpoint:** Pause for manual confirmation before Phase 15.

---

## Phase 15: Research Concurrent Query Throughput

### Goal

Scale independent lookups across threads while preserving a zero-copy, immutable reader.

### Candidate Experiments

1. Share one immutable mapped segment among query workers.
2. Compare single-query latency with multi-threaded throughput.
3. Test common-term, rare-term, missing-term, and mixed workloads.
4. Measure contention in shared metadata or caches.
5. Compare per-thread and shared scratch storage.
6. Evaluate a bounded cache for parsed dictionary or hot-term metadata.
7. Batch sorted lookups to improve locality.
8. Measure throughput and p95/p99 latency as concurrency rises.
9. Identify CPU, memory-bandwidth, and page-fault saturation points.

### Correctness Requirements

- Readers never mutate mapped segment bytes.
- Concurrent iterators maintain independent decoding state.
- Shared caches cannot outlive their mapped segment.
- Query results remain identical at every concurrency level.

### Rust and Systems Concepts

- Immutable sharing
- `Arc`, `Send`, and `Sync`
- Lock-free reads versus synchronized caches
- False sharing and cache contention
- Throughput versus tail latency
- Load generation and coordinated omission

### Success Criteria

- Scaling curves show throughput and tail latency by concurrency level.
- The selected design identifies its saturation point.
- Any cache demonstrates a net workload-level benefit after memory cost.
- Thread safety is expressed by the type design and verified by tests.

**Implementation checkpoint:** Pause for manual confirmation before Phase 16.

---

## Phase 16: Research CPU-Level Optimizations

### Goal

Evaluate specialized low-level optimizations only after algorithms, layout, and concurrency are
understood.

### Candidate Experiments

1. Branch-reduced or table-assisted varint decoding.
2. Batch decoding of posting blocks.
3. SIMD-assisted byte classification during tokenization.
4. SIMD-assisted integer unpacking for a block format.
5. Cache-line-aware metadata layout.
6. Reduced-copy term comparison.
7. Platform-specific prefetch instructions where profiling supports them.
8. Alternative allocators only when allocation profiles justify the experiment.

### Rules

- Keep a clear scalar implementation as the correctness reference.
- Isolate architecture-specific code behind a safe interface.
- Detect CPU capabilities rather than assuming one instruction set.
- Include fallback behavior.
- Count added complexity and portability cost as regressions unless speedup is material.

### Rust and Systems Concepts

- Scalar versus SIMD execution
- CPU feature detection
- Branch prediction
- Instruction-level parallelism
- Alignment and cache lines
- Safe abstraction around architecture-specific operations

### Success Criteria

- Every optimized path matches the scalar path.
- Benchmarks cover supported and fallback paths.
- Retained low-level code produces a material measured improvement on the target workload.
- Unsafe or architecture-specific code is minimal, isolated, and documented.

**Implementation checkpoint:** Pause for manual confirmation before Phase 17.

---

## Phase 17: Synthesize the Optimized Design

### Goal

Turn experiments into a defensible final design rather than an accumulation of tricks.

### Work

1. Build a matrix of every experiment, prediction, result, and decision.
2. Retain only compatible optimizations with demonstrated combined benefit.
3. Re-run complete correctness and corruption suites.
4. Re-run the full benchmark matrix against the frozen Phase 9 baseline.
5. Measure interactions between retained optimizations.
6. Document workloads where the optimized design is worse.
7. Record rejected approaches and what evidence would justify revisiting them.
8. Produce a final architecture and segment-format explanation.

### Final Report

The report includes:

- Baseline and final indexing throughput
- Baseline and final segment size
- Baseline and final lookup latency distributions
- Baseline and final postings decode throughput
- Scaling by build worker count
- Scaling by query concurrency
- Peak memory changes
- Cold and warm access behavior
- Complexity and portability tradeoffs
- Retained and rejected experiments

### Success Criteria

- The final version passes the original oracle and all corruption tests.
- Improvements reproduce on the fixed benchmark setup.
- Performance gains are attributable to documented mechanisms.
- The final system is no more complex than its measured benefits justify.

**Implementation checkpoint:** The performance research track is complete after this phase.

## Testing Strategy

The Phase 1 in-memory index is the semantic oracle. Every later representation must reproduce its
results. Tests progress from local primitives to complete persisted behavior:

1. Tokenization and in-memory posting construction
2. Finalization invariants
3. Integer and posting codec round trips
4. Deterministic segment serialization
5. Segment validation
6. Binary-search lookup
7. Lazy postings decoding
8. Build-to-reopen integration
9. Deliberate corruption
10. Public-corpus benchmarks
11. Baseline-versus-experiment differential tests
12. Determinism under parallel scheduling
13. Scalar-versus-optimized codec equivalence
14. Concurrent reader correctness

No benchmark result can compensate for a correctness mismatch.

## Performance Principles

- Optimize only after the oracle and disk reader agree.
- Keep the reader lazy: lookup should touch dictionary records and only the requested postings.
- Keep decoding allocation-free.
- Use delta encoding because posting document IDs are ordered.
- Use mmap to let the operating system page data on demand, not as a claim that I/O disappears.
- Freeze a correct baseline before introducing structures beyond the baseline stopping boundary.
- Profile before optimizing; never infer a bottleneck from intuition alone.
- Delete unnecessary work before parallelizing or hiding it behind async execution.
- Treat async, concurrency, compression, caching, SIMD, and prefetching as workload-dependent
  experiments.
- Change one major variable per experiment and preserve rejected results.
- Optimize end-to-end workload metrics, not isolated microbenchmarks alone.
- Include memory, segment size, tail latency, complexity, and portability in every speedup decision.

## Collaboration Workflow

For every phase:

1. State the precise capability being added.
2. Start from the simplest design that could work.
3. Break it with a concrete input or failure.
4. Identify the invariant that the simple design violated.
5. Derive the minimum fix.
6. Implement one small unit at a time.
7. Review ownership, lifetimes, errors, and format consequences.
8. Run the phase's targeted tests.
9. Stop at the phase gate for manual confirmation.

The learner writes the implementation. Copilot derives the design, explains the Rust and systems
concepts, reviews code, and helps debug. Copilot writes or modifies implementation code only when
explicitly requested.

## References

- Java reference writer:
  `C:\Users\shivag\Documents\java\lucene\lucene\codecs\src\java\org\apache\lucene\codecs\simpletext\SimpleTextFieldsWriter.java`
- Java reference reader:
  `C:\Users\shivag\Documents\java\lucene\lucene\codecs\src\java\org\apache\lucene\codecs\simpletext\SimpleTextFieldsReader.java`
- SimpleText escaping and checksum reference:
  `C:\Users\shivag\Documents\java\lucene\lucene\codecs\src\java\org\apache\lucene\codecs\simpletext\SimpleTextUtil.java`
- Rust project:
  `C:\Users\shivag\Documents\rust_practice\inverted_index`
