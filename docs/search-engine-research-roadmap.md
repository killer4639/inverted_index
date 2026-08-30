# Search-Native Storage and Analytics Engine Research Roadmap

Last updated: **2026-08-27**

The current short-term execution plan is maintained in
[immediate-next-steps.md](immediate-next-steps.md).

## North star

Evolve the current immutable inverted-index prototype into a single-node,
durable, concurrent, search-native storage engine with:

- incremental inserts, updates, and deletes;
- immutable segments and background compaction;
- crash recovery and snapshot-consistent reads;
- ranked full-text retrieval;
- Arrow-native structured columns and query results;
- analytical SQL through DataFusion;
- a stable logical change stream;
- experimentally validated implementations of important research papers.

The intended result is not a smaller clone of Lucene, Elasticsearch, DuckDB, or
Kafka. The differentiating system should answer this question:

> How efficiently can one engine combine sparse, relevance-ordered search with
> dense, vectorized analytics while retaining durable writes, reproducible
> snapshots, and a verifiable change history?

## Current starting point

The repository already has a useful storage-format baseline:

- deterministic, immutable segment files;
- sorted term dictionary;
- delta-encoded document IDs and variable-length integers;
- memory-mapped reads;
- binary-search term lookup;
- lazy postings decoding;
- format versioning, safe layout parsing, and CRC32 corruption detection.

The current engine deliberately lacks multiple segments, stable global document
identity, writes, deletes, positions, scoring, stored fields, compaction, and
analytical execution. Those are not independent features. They must be added in
an order that preserves a coherent correctness model.

## Assumptions to challenge

| Assumption | First-principles conclusion |
|---|---|
| Supporting writes requires mutable segment files | Writes can mutate the set of visible immutable segments instead. |
| A WAL and CDC stream can be the same log | Recovery and external consumption require different stability, retention, and compatibility contracts. |
| Arrow should replace the postings format | Arrow fits typed columns and batches; compressed sparse postings need a specialized representation. |
| Concurrency means adding threads throughout the code | The actual requirement is concurrent progress with deterministic publication and snapshot visibility. |
| A deeper query engine requires writing SQL infrastructure | Search execution is the original work; DataFusion can supply SQL, optimization infrastructure, and vectorized analytics. |
| A paper implementation is useful once it compiles | It is useful only when its central claim is reproduced, falsified, or extended under a controlled workload. |

## Irreducible system model

### Write path

```text
write batch
    -> validate and prepare outside the commit lock
    -> assign commit sequence
    -> append framed WAL records
    -> fsync according to durability policy
    -> apply to active memtable
    -> acknowledge
    -> freeze full memtable
    -> flush immutable segment
    -> atomically publish a new manifest generation
    -> retire WAL ranges represented by published segments
```

An update is logically a deletion of one document version followed by insertion
of another. A delete is a versioned tombstone. In-place mutation of persisted
postings is not required.

### Read path

```text
query
    -> pin manifest generation and visible sequence
    -> inspect active/frozen memtable snapshots
    -> inspect immutable search segments
    -> apply visible tombstones
    -> execute sparse search operators
    -> bridge selected document IDs into Arrow batches
    -> execute filters, projections, and aggregations
```

Readers must never observe half-published segments, a document version newer
than their snapshot, or a deleted version without the corresponding tombstone.

### Durability contract

If commit sequence `S` was acknowledged in durable mode, recovery after any
process crash must expose `S` or a later visible version of the same logical
document.

### CDC contract

A consumer resuming from sequence `S` receives every committed logical change
after `S` at least once, with deterministic event identities. CDC does not
promise that raw WAL bytes or internal segment layouts remain stable.

## How research papers will be used

Every paper implementation follows the same contract:

1. **Extract the claim.** Write one measurable sentence describing what should
   improve and under which workload.
2. **Reproduce the baseline.** Implement the weaker alternative first.
3. **Reproduce the paper mechanism.** Avoid unrelated improvements that obscure
   causality.
4. **Reproduce a crossover curve.** Report where the mechanism wins and loses,
   not one favorable number.
5. **Attack it.** Test skew, updates, deletes, small datasets, cold cache,
   constrained memory, and adversarial distributions.
6. **Extend it.** Add one original hypothesis connected to this engine.
7. **Keep or remove it.** A mechanism that does not justify its complexity is
   removed or retained only as an experimental implementation.

Each experiment records:

- commit hash and format version;
- corpus generator seed or immutable dataset identifier;
- machine, CPU, memory, OS, compiler, and build profile;
- warm-cache and cold-cache results;
- throughput, p50, p95, and p99 latency;
- peak resident memory;
- logical and physical bytes read and written;
- correctness-oracle result;
- confidence intervals or repeated-run variance.

## Paper implementation portfolio

The papers are grouped by the subsystem they can improve. This is a prioritized
implementation portfolio, not a requirement to implement every paper.

### Track A: Ranked retrieval and dynamic pruning

#### A1. Block-Max WAND

**Paper:** Shuai Ding and Torsten Suel, *Faster Top-k Document Retrieval Using
Block-Max Indexes*, SIGIR 2011.

Primary source:
[ACM DOI](https://doi.org/10.1145/2009916.2010048)

**Failure that motivates it:** A BM25 query over several common terms decodes
and scores millions of postings even though only the best 10 documents are
returned.

**Mechanism to implement:**

- divide postings into blocks;
- store a safe maximum score contribution per block;
- skip a block when its upper bound cannot enter the current top-k heap.

**Baseline:** exhaustive disjunctive document-at-a-time scoring.

**Claim to reproduce:** exact top-k results with substantially fewer postings
decoded for selective and moderately skewed queries.

**Original extension:** choose block boundaries using observed score
distribution and cache-line cost rather than a fixed posting count.

**Kill criterion:** if metadata growth and branch overhead make p95 slower for
the majority of the declared workload, retain exhaustive execution for that
regime through an explicit planner decision.

#### A2. BMP for learned sparse retrieval

**Paper:** Antonio Mallia, Torsten Suel, and Nicola Tonellotto, *Faster Learned
Sparse Retrieval with Block-Max Pruning*, SIGIR 2024.

Primary source:
[arXiv:2405.01117](https://arxiv.org/abs/2405.01117)

**Failure that motivates it:** Learned sparse representations have different
weight and term distributions from BM25, making assumptions behind traditional
dynamic pruning less effective.

**Mechanism to implement:**

- add weighted sparse query and document terms;
- construct block summaries;
- execute safe and approximate pruning modes;
- measure latency-recall tradeoffs.

**Baseline:** Block-Max WAND and exhaustive weighted sparse scoring.

**Original extension:** use one physical index that selects BM25-oriented or
learned-sparse-oriented pruning per query, based on query-vector statistics.

**Dependency:** ranked lexical retrieval and a repeatable learned sparse dataset
must exist first. Do not make model inference part of the initial experiment.

#### A3. Seismic

**Paper:** Sebastian Bruch et al., *Efficient Inverted Indexes for Approximate
Retrieval over Learned Sparse Representations*, SIGIR 2024 Best Paper.

Primary source:
[arXiv:2404.18812](https://arxiv.org/abs/2404.18812)

**Failure that motivates it:** A conventional posting list groups documents by
document ID, not by similarity of their sparse learned representations, leaving
weak summaries and limited skipping opportunities.

**Mechanism to implement:**

- partition posting lists into geometrically cohesive blocks;
- create summary vectors for blocks;
- rank or skip blocks before evaluating individual documents;
- expose the latency-recall-memory frontier.

**Baseline:** BMP and a conventional document-ordered sparse inverted index.

**Original extension:** investigate whether block organization can be shared
with Arrow min/max, Bloom, or dictionary statistics so search and analytics
reuse one pruning decision.

**Research value:** This is the most ambitious retrieval-paper reproduction in
the roadmap and should come after the engine has reliable benchmarks.

#### A4. BitFunnel as a contrasting retrieval architecture

**Paper:** Bob Goodwin et al., *BitFunnel: Revisiting Signatures for Search*,
SIGIR 2017.

Primary source:
[Microsoft Research publication](https://www.microsoft.com/en-us/research/publication/bitfunnel-revisiting-signatures-search/)

**Question:** At what document density, update pattern, and query shape does a
bit-sliced signature representation beat compressed postings?

**Implementation scope:** A restricted term-existence index and Boolean query
executor, not a replacement for ranked positional search.

**Original extension:** let the engine select signatures, postings, or both per
segment using measured density and update behavior.

### Track B: Learned and SIMD-friendly compression

#### B1. LeCo

**Paper:** Yihao Liu, Xinyu Zeng, and Huanchen Zhang, *LeCo: Lightweight
Compression via Learning Serial Correlations*, SIGMOD 2024.

Primary sources:
[ACM DOI](https://doi.org/10.1145/3639320) and
[author PDF](http://people.iiis.tsinghua.edu.cn/~huanchen/publications/leco-sigmod24.pdf)

**Failure that motivates it:** Delta-varint encoding exploits monotonicity but
does not model longer-range regularity in sorted document IDs or numeric
columns.

**Mechanism to implement:**

- learn a compact predictor for values within a block;
- encode residuals;
- support block decompression and random access;
- compare scalar and SIMD-friendly decoding paths.

**Baselines:**

- current delta-varint encoding;
- frame-of-reference bit packing;
- BP128 or Stream VByte;
- Roaring bitmap for dense posting lists.

**Original extension:** train or select a representation independently per
posting block using predicted end-to-end query cost, not compression ratio
alone.

#### B2. The PGM-index

**Paper:** Paolo Ferragina and Giorgio Vinciguerra, *The PGM-index: A Fully
Dynamic Compressed Learned Index with Provable Worst-Case Bounds*, PVLDB 2020.

Primary source:
[PVLDB paper](https://www.vldb.org/pvldb/vol13/p1162-ferragina.pdf)

**Question:** Can a piecewise-linear model reduce term-offset or numeric-column
index size while retaining bounded lookup work?

**Potential applications:**

- locating terms in a sorted dictionary;
- locating posting blocks by document ID;
- locating row groups by sorted numeric value.

**Baseline:** binary search over fixed-width offsets.

**Original extension:** compare one shared learned locator against specialized
locators for the term dictionary, postings, and Arrow columns. The likely result
may be that learning helps numeric distributions but not variable-length term
bytes; that negative result is still valuable.

#### B3. Learned-compression survey and reproduction matrix

**Paper:** Qiyu Liu et al., *Learned Data Compression: Challenges and
Opportunities for the Future*, 2024.

Primary source:
[arXiv:2412.10770](https://arxiv.org/abs/2412.10770)

Use this survey to create a matrix of:

- model size;
- encoding cost;
- decoding throughput;
- random-access capability;
- SIMD suitability;
- retraining requirement;
- robustness to distribution shifts.

The engine should never require a learned model to open a segment unless the
model is embedded, checksummed, versioned, and guarded by a non-learned fallback
or migration path.

### Track C: Write amplification and adaptive compaction

#### C1. Monkey

**Paper:** Niv Dayan, Manos Athanassoulis, and Stratos Idreos, *Monkey: Optimal
Navigable Key-Value Store*, SIGMOD 2017.

Primary source:
[ACM DOI](https://doi.org/10.1145/3035918.3064054)

**Failure that motivates it:** Assigning equal Bloom-filter precision to every
level spends memory where it prevents little I/O.

**Mechanism to adapt:**

- model per-level lookup probability and I/O cost;
- allocate filter memory non-uniformly;
- measure negative-lookup I/O under fixed memory.

**Search-engine adaptation:** Allocate Bloom-filter or term-summary memory
across segments using segment size, recency, term distribution, and observed
query probability.

**Original extension:** optimize memory jointly across search summaries and
Arrow/Parquet row-group statistics.

#### C2. Dostoevsky

**Paper:** Niv Dayan and Stratos Idreos, *Dostoevsky: Better Space-Time
Trade-Offs for LSM-Tree Based Key-Value Stores via Adaptive Removal of
Superfluous Merging*, SIGMOD 2018.

Primary source:
[ACM DOI](https://doi.org/10.1145/3183713.3196927)

**Failure that motivates it:** Pure leveling spends excessive write bandwidth;
pure tiering increases read amplification.

**Mechanism to adapt:** Explore lazy leveling and hybrid merge policies for
immutable search segments.

**Original extension:** optimize compaction using measured search work:

```text
objective =
    expected segments touched
  + alpha * bytes rewritten
  + beta  * tombstone retention
  + gamma * stale statistics penalty
```

Unlike a key-value LSM tree, search segments contain posting distributions,
positions, score bounds, and columnar fragments. The experiment should determine
whether those differences require a genuinely different policy.

#### C3. FASTER as a competing write architecture

**Paper:** Badrish Chandramouli et al., *FASTER: A Concurrent Key-Value Store
with In-Place Updates*, SIGMOD 2018.

Primary source:
[Microsoft Research publication](https://www.microsoft.com/en-us/research/publication/faster-a-concurrent-key-value-store-with-in-place-updates/)

**Question:** Is the repository's memtable plus immutable-segment design still
superior when updates are extremely hot and concentrated?

Do not port FASTER wholesale. Implement a bounded experiment comparing:

- append-only versioned updates;
- in-memory in-place update metadata;
- hybrid-log behavior for a small stored-field workload.

The result defines where this search-native architecture is wrong.

### Track D: Arrow and vectorized execution

#### D1. MonetDB/X100 vectorized execution

**Paper:** Peter Boncz et al., *MonetDB/X100: Hyper-Pipelining Query Execution*,
CIDR 2005.

Primary source:
[CIDR PDF](https://www.cidrdb.org/cidr2005/papers/P19.pdf)

**Failure that motivates it:** Processing one value per virtual function call
or iterator step wastes CPU on interpretation, branches, and poor instruction
locality.

**Mechanism to implement:**

- execute structured predicates over bounded vectors;
- compare row-at-a-time, whole-column, and vector-at-a-time execution;
- vary Arrow batch size;
- collect cycles, branches, cache misses, and elapsed time.

**Original extension:** dynamically choose the conversion point from sparse
posting iterators to dense Arrow batches.

#### D2. Morsel-driven parallelism

**Paper:** Viktor Leis et al., *Morsel-Driven Parallelism: A NUMA-Aware Query
Evaluation Framework for the Many-Core Age*, SIGMOD 2014.

Primary source:
[ACM DOI](https://doi.org/10.1145/2588555.2610507)

**Failure that motivates it:** Static partitioning leaves threads idle when
posting lists, filters, or segments have uneven costs.

**Mechanism to adapt:**

- divide segment and column scans into small work units;
- use cooperative scheduling and work stealing;
- preserve cancellation and snapshot pinning;
- measure load balance and scheduling overhead.

**Original extension:** estimate morsel size using postings density and expected
Arrow selectivity instead of only row counts.

#### D3. DataFusion as an extensible analytical substrate

**System paper:** *Apache Arrow DataFusion: A Fast, Embeddable, Modular Analytic
Query Engine*, SIGMOD 2024 Industry Track.

Canonical project sources:
[DataFusion concepts and readings](https://datafusion.apache.org/user-guide/concepts-readings-events.html)
and [DataFusion repository](https://github.com/apache/datafusion).

**Implementation target:**

- expose documents as a DataFusion `TableProvider`;
- implement `text_match` and scoring expressions;
- push full-text predicates into the search engine;
- return candidate document IDs or Arrow selection vectors;
- execute projection, filtering, sorting, and aggregation in DataFusion;
- include search operators in `EXPLAIN ANALYZE`.

**Original research question:** Which bridge representation minimizes total work
between sparse retrieval and dense analytics?

Candidates:

- sorted document-ID stream;
- Roaring bitmap;
- dense bitset;
- Arrow Boolean mask;
- Arrow indices/selection vector;
- materialized `RecordBatch`.

The planner should learn a deterministic crossover model from cardinality,
density, ordering requirements, and downstream operators.

### Track E: CDC and snapshot correctness

#### E1. DBLog

**Paper:** Netflix, *DBLog: A Watermark Based Change-Data-Capture Framework*,
PVLDB.

Primary project source:
[Netflix DBLog repository](https://github.com/Netflix/DBLog)

**Failure that motivates it:** A source snapshot and a concurrently advancing
change log cannot be naively concatenated without missing or duplicating state.

**Mechanism to implement:**

- insert or identify watermark boundaries;
- interleave snapshot chunks with log consumption;
- suppress duplicates deterministically;
- resume from a durable checkpoint.

**Local application:** Implement snapshot-plus-tail export from this engine's
logical sequence history into Arrow batches or Parquet.

**Original extension:** emit a verifiable boundary receipt containing source
sequence, manifest generation, schema version, row count, and bucketed content
hashes. This should prove equality at a declared boundary rather than merely
report low CDC lag.

### Track F: Adaptive indexing

#### F1. Database cracking

**Paper:** Stratos Idreos, Martin Kersten, and Stefan Manegold, *Database
Cracking*, CIDR 2007.

Primary source:
[CIDR proceedings](https://www.cidrdb.org/cidr2007/program.html)

**Failure that motivates it:** Building every possible structured index eagerly
spends write and storage work on fields and ranges that may never be queried.

**Mechanism to adapt:** Incrementally refine organization of selected Arrow
columns as predicates arrive.

**Original extension:** allow analytical filters repeatedly paired with text
queries to create segment-local adaptive structures. Account for the additional
write amplification and ensure old snapshots remain readable.

This is a late-stage experiment. It should not complicate the first correct
columnar format.

## Original research directions

The following projects go beyond reproducing a paper. Each has a falsifiable
claim and fits the same engine.

### 1. Density-adaptive posting blocks

**Hypothesis:** No single representation minimizes query CPU and space across
all term densities. Selecting delta-varint, bit-packed FOR, LeCo-style residuals,
Roaring, or dense bitsets per block will beat a global encoding policy.

**Experiment:** Generate posting distributions varying density, clustering,
gaps, and update history. Train an offline deterministic cost model, then verify
whether predicted choices reduce end-to-end latency without excessive metadata.

### 2. Query-aware compaction

**Hypothesis:** A merge policy using term hotness, delete density, score-bound
quality, and analytical filter frequency can reduce total query work at the
same write amplification as size-only compaction.

**Risk:** Workload adaptation can overfit and cause unstable merge decisions.
Evaluate abrupt workload shifts and impose bounded deviation from a simple
policy.

### 3. Sparse-to-columnar crossover optimizer

**Hypothesis:** Search-plus-analytics queries have predictable crossover points
where iterator execution should become bitmap or Arrow-batch execution.

**Contribution:** A cost model and DataFusion physical operator that chooses the
bridge representation at runtime.

### 4. Unified pruning metadata

**Hypothesis:** One segment-level metadata budget can be allocated jointly among
block-max scores, Bloom filters, sparse summaries, and column min/max statistics
more effectively than each subsystem managing memory independently.

**Experiment:** Hold metadata bytes constant and optimize total workload
latency. Compare independent fixed budgets against a joint allocator.

### 5. Snapshot-consistent search analytics

**Hypothesis:** One sequence and manifest model can provide reproducible results
across postings, stored Arrow columns, deletes, and CDC without distributed
transactions inside a single process.

**Proof obligations:**

- no visible posting refers to an invisible column row;
- no column row is returned after a visible delete;
- search and SQL see the same snapshot;
- CDC snapshot-plus-tail reconstructs exactly that snapshot.

### 6. Retrieval receipts

Produce a receipt for a query containing:

- visible sequence and manifest generation;
- analyzer and schema versions;
- logical and physical plans;
- segment IDs and statistics versions;
- approximate-mode parameters;
- result hash.

**Hypothesis:** Result changes can be attributed to data, analyzer, statistics,
plan, or approximation changes without retaining a complete running server.

### 7. Continuous search over the commit stream

**Hypothesis:** Saved Boolean and phrase queries can share term-triggered
evaluation over committed document batches more efficiently than rerunning every
query independently.

Begin with exact term and Boolean subscriptions. Ranking, retractions, updates,
and windowed analytics come later.

## Twelve-month execution order

The schedule is intentionally aggressive. Advancement is controlled by
correctness and measurement gates rather than calendar completion alone.

| Month | Core system work | Paper/research work | Exit condition |
|---:|---|---|---|
| 1 | Reproducible benchmark harness; generated corpora; property, fuzz, corruption, and crash-test foundations | Compression baseline matrix | Repeatable single-thread baseline with variance reported |
| 2 | Multiple segments, stable global document IDs, immutable manifest generations, reader pinning | Posting representation crossover study | Queries remain correct across arbitrary segment sets |
| 3 | Active/frozen memtables, framed WAL, group commit, recovery, checkpoints | Crash-boundary study | Every acknowledged write survives injected crashes |
| 4 | Versioned deletes and updates, snapshot visibility, background flush | FASTER-inspired hot-update comparison | Snapshot oracle passes randomized histories |
| 5 | Compaction, safe file reclamation, merge scheduling | Monkey and Dostoevsky adaptations | Bounded segment/read amplification under sustained writes |
| 6 | Positions, Boolean execution, BM25, top-k heap | Exhaustive versus Block-Max WAND | Exact top-k equality and pruning crossover curves |
| 7 | Phrase/proximity queries, skip structures, `EXPLAIN ANALYZE` | Adaptive block-boundary extension | Query plans expose estimates and actual work |
| 8 | Arrow stored fields, typed schema, projections and filters | X100-style batch-size and execution experiment | Search results join correctly to Arrow rows at every snapshot |
| 9 | DataFusion `TableProvider`, predicate pushdown, analytical SQL | Sparse-to-columnar crossover optimizer | Hybrid search/aggregation beats forced materialization on declared workloads |
| 10 | Logical CDC, retention, resume tokens, snapshot-plus-tail export | DBLog reproduction and boundary receipts | Export reconstructs source state at a declared sequence |
| 11 | Weighted sparse vectors and approximate retrieval mode | BMP, then Seismic reproduction | Published recall-latency-memory frontier |
| 12 | Adaptive physical representations and integrated optimizer | Unified pruning metadata or query-aware compaction | One defensible original result with adversarial evaluation |

## Parallel engineering tracks

These continue throughout the roadmap:

### Correctness

- simple in-memory reference model;
- randomized operation histories;
- model-based snapshot tests;
- crash injection before and after every durability boundary;
- malformed segment and WAL fuzzing;
- deterministic concurrency tests where practical;
- checksums and strict validation for every persisted structure.

### Observability

- per-query plan and operator timings;
- postings visited, decoded, and skipped;
- segments and bytes touched;
- Arrow batches, rows, and selection density;
- WAL queue depth and fsync latency;
- flush and compaction backlog;
- write, read, and space amplification;
- pinned manifest generations blocking reclamation;
- CDC consumer lag and retained-log bytes.

### Benchmark workloads

- uniform and Zipfian vocabulary;
- common-term conjunctions and disjunctions;
- phrase-heavy queries;
- selective structured filters;
- unselective structured filters;
- sparse and dense search results entering analytics;
- append-only ingestion;
- random updates;
- highly concentrated hot-document updates;
- delete-heavy churn;
- mixed read/write workloads;
- cold cache, warm cache, and memory-constrained runs.

## Architecture boundaries

### Implement directly

- segment and manifest lifecycle;
- WAL and recovery semantics;
- MVCC/document-version visibility;
- postings, positions, scoring, and dynamic pruning;
- search-aware compaction;
- sparse-to-columnar execution bridge;
- CDC sequence and snapshot contract;
- experimental paper mechanisms.

### Reuse initially

- Arrow arrays, schemas, record batches, and compute kernels;
- DataFusion parser, logical planning, generic physical operators, and SQL;
- Parquet if experiments show it meets durable stored-field requirements;
- established bitmap and SIMD crates when the experiment is about policy rather
  than reimplementing machine instructions.

### Do not build yet

- distributed consensus;
- a custom SQL parser;
- a Kafka-compatible broker;
- generic workflow orchestration;
- model training infrastructure;
- a networked cluster before single-node recovery is proven;
- mutable in-place persisted postings;
- lock-free structures without measured lock contention.

## Decision gates

### Gate 1: storage engine

Do not add Arrow analytics until multi-segment publication, recovery, and
snapshot visibility pass randomized and crash-injected tests.

### Gate 2: query engine

Do not add learned sparse retrieval until BM25 exhaustive execution and exact
dynamic pruning share a correctness oracle.

### Gate 3: analytical integration

Do not optimize DataFusion integration until a deliberately simple candidate-ID
materialization path is correct and measured.

### Gate 4: CDC

Do not claim CDC correctness until snapshot-plus-tail output can reconstruct and
hash-match a source snapshot at an explicit sequence.

### Gate 5: originality

Do not call an optimization novel merely because it combines two components.
The contribution must contain:

- a new mechanism, model, or correctness contract;
- a baseline that could plausibly win;
- a workload regime where the proposal loses;
- an independently repeatable experiment.

## Expected six-month result

A durable single-node search engine with:

- concurrent preparation and serialized commits;
- WAL recovery;
- multiple immutable segments;
- updates, deletes, snapshots, flush, and compaction;
- positions, Boolean queries, phrases, BM25, and exact top-k pruning;
- reproducible performance and crash experiments.

## Expected twelve-month result

A search-native analytical engine with:

- Arrow-backed stored fields and result batches;
- DataFusion SQL with full-text predicate pushdown;
- adaptive sparse-to-columnar execution;
- logical CDC with verifiable snapshot-plus-tail export;
- learned sparse retrieval experiments;
- at least one reproduced modern paper result;
- at least one original mechanism evaluated against strong baselines.

## Highest-value initial paper sequence

If time forces a narrow selection, use this order:

1. **Block-Max WAND** -- immediately relevant after BM25 and teaches exact
   dynamic pruning.
2. **MonetDB/X100** -- establishes the execution model needed before serious
   Arrow work.
3. **Monkey and Dostoevsky** -- turn compaction and metadata allocation into
   measurable policies rather than copied defaults.
4. **LeCo** -- creates a rigorous adaptive-compression track grounded in the
   existing postings format.
5. **DBLog** -- gives CDC a real snapshot-consistency contract.
6. **BMP** -- extends exact top-k work into learned sparse retrieval.
7. **Seismic** -- the ambitious capstone for approximate sparse retrieval.

This order follows the engine's dependency graph: durable state first, exact
search second, columnar execution third, external consistency fourth, and
approximate learned retrieval last.
