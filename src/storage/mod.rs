#[expect(dead_code, reason = "used by later multi-segment storage phases")]
pub(crate) mod index_storage;
#[expect(dead_code, reason = "used by later multi-segment storage phases")]
mod manifest;
pub(crate) mod postings;
pub(crate) mod segment_reader;
pub(crate) mod segment_writer;
pub(crate) mod varint;
