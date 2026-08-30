# Inverted Index

A small search engine written in Rust to explore how immutable inverted indexes
work. It builds a compact binary segment from a text corpus, memory-maps that
segment, finds terms with binary search, and decodes postings lazily.

Each input line is one document. A posting stores the document ID and the number
of times the term occurs in that document.

```text
term -> [(document_id, term_frequency), ...]
```

## Features

- Sequential `u32` document IDs
- Per-document term frequencies
- Deterministically sorted term dictionary
- Delta-encoded document IDs
- Variable-length integer encoding
- Immutable, versioned segment files
- CRC32 corruption detection
- Read-only memory-mapped segment reader
- Binary-search term lookup without allocating term strings
- Lazy postings decoding without materializing a postings list
- Reuse of an open memory map for repeated lookups

## Usage

Start the interactive CLI:

```console
cargo run --release
```

Build a segment:

```text
> build data/sample.txt segment.idx
built 135 document(s), 463 term(s), 1038 posting(s) into segment.idx
index creation statistics:
  input bytes: 6129
  tokens: 1053
  segment bytes: 16693
  validation: <varies>
  indexing: <varies>
  finalization: <varies>
  segment write: <varies>
  total: <varies>
```

Durations vary by machine and run. The statistics report raw corpus and segment
sizes, token count, and the time spent in each current index-creation phase.

Look up one or more terms:

```text
> lookup segment.idx rust
7 matching document(s)
document 6: frequency 1
document 58: frequency 3
document 63: frequency 1
document 88: frequency 1
document 89: frequency 1
document 90: frequency 1
document 106: frequency 1

> lookup segment.idx mmap
2 matching document(s)
document 66: frequency 1
document 107: frequency 1
lookup statistics:
  segment access: reused
  segment bytes: 16693
  indexed documents: 135
  indexed terms: 463
  term found: true
  dictionary comparisons: 9
  matched documents: 2
  postings bytes: 4
  segment open: 0.000 ms
  dictionary lookup: <varies>
  postings decode: <varies>
  total: <varies>
```

Repeated lookups against the same segment reuse its existing memory map. Enter
EOF (`Ctrl+Z`, then Enter on Windows) to exit.

### Commands

```text
build <corpus> <segment>
lookup <segment> <term>
lookup-stats <segment> <term>
```

The writer will not replace an existing segment file.
`lookup-stats` executes and decodes the complete lookup without printing each
matching document, making it suitable for large posting lists and baseline
measurements.

## Corpus format

- Each line is treated as one document.
- Documents must not be empty.
- Tokens are separated by whitespace.
- Tokens may contain only ASCII letters and digits.
- Terms are case-sensitive.

Example:

```text
rust makes systems programming practical
search uses an inverted index
rust makes indexing fast
```

## Architecture

The indexing path first accumulates postings in memory and then finalizes them
into an immutable representation. The segment writer serializes that
representation to a temporary file and atomically publishes the completed
segment.

After writing, queries do not use the in-memory index:

```text
corpus
  -> IndexBuilder
  -> finalized in-memory index
  -> immutable segment file
  -> read-only mmap
  -> binary-search term lookup
  -> lazy postings decoder
```

The source tree is grouped by responsibility:

```text
src/
  indexing/   corpus validation, index construction, and creation statistics
  storage/    integer/postings codecs and immutable segment I/O
  search/     term lookup, lookup execution, and lookup statistics
  model/      shared index and posting types
  lib.rs      public library boundary
  main.rs     interactive CLI only
```

The reader verifies the checksum and reads enough header layout information to
access the mapped sections safely. Term records and postings are parsed and
checked only when a lookup accesses them; semantic segment correctness is
enforced by the writer.

## Segment format

```text
+----------------------------------+
| Header (26 bytes)                |
|   magic                   8 bytes|
|   format version             u16 |
|   document count             u32 |
|   term count                 u32 |
|   postings absolute offset   u64 |
+----------------------------------+
| Term-offset table                |
|   absolute term offset       u64 | x term count
+----------------------------------+
| Term records                     |
|   term byte length        varint |
|   UTF-8 term bytes               |
|   document frequency      varint |
|   relative postings offset  u64  |
|   postings byte length      u64  |
+----------------------------------+
| Postings region                  |
|   document ID delta       varint |
|   term frequency          varint |
|   ...                            |
+----------------------------------+
| CRC32 checksum            4 bytes|
+----------------------------------+
```

Term-record offsets are absolute file positions. A term's postings offset is
relative to the beginning of the postings region. The checksum covers every
byte before the checksum itself.

## Development

Run the test suite:

```console
cargo test
```

Format the code:

```console
cargo fmt
```

## Current scope

The engine currently builds one immutable segment in a single process. It
supports exact term lookup and term frequencies, but intentionally does not yet
implement stored documents, positions, phrase queries, scoring, deletes,
incremental updates, or segment merging.

## Roadmap

- [ ] Add reproducible performance benchmarks using large corpora such as
      Wikipedia dumps.
- [ ] Record corpus size, document count, vocabulary size, segment size,
      indexing throughput, lookup latency, and postings-decoding throughput.
- [ ] Establish a single-threaded release-build baseline before optimizing.
- [ ] Parallelize tokenization and per-document term-frequency collection across
      worker threads.
- [ ] Use multiple CPU cores for segment construction while preserving
      deterministic term and posting ordering.
- [ ] Compare single-threaded and multicore throughput, memory usage, and scaling
      efficiency on the same datasets.
- [ ] Support incremental ingestion by writing multiple independent immutable
      segments instead of rebuilding one segment for every batch.
- [ ] Search across multiple segments and combine their posting results.
- [ ] Add background segment merging, similar to the immutable-segment model
      used by Elasticsearch and Lucene.
- [ ] Define merge policies, document-ID remapping, atomic segment publication,
      and safe cleanup of obsolete segments.
