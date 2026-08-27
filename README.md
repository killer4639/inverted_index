# Inverted Index

A small inverted index written in Rust. Each input line is treated as a
document, and every term maps to the document IDs and frequencies where it
appears.

The index has two stages: documents are collected by a mutable builder, then
written to an on-disk segment. After that write, the in-memory index is dropped
and queries run against the memory-mapped file.

## Run

```console
cargo run
```

At the prompt:

```text
index data/sample.txt
finalize
query rust
```

Input documents must be non-empty and contain only whitespace-separated ASCII
letters and digits. Terms are case-sensitive.

## Test

```console
cargo test
```
