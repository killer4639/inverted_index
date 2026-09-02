mod indexing;
mod model;
mod search;
mod storage;

pub use indexing::{
    CorpusStats, IndexCreationStats, IndexCreationTimings, IndexStats, SegmentStats, create_index,
};
pub use model::document_address::{AddressedPosting, DocumentAddress, SegmentId};
pub use model::{DocumentId, InvertedIndex, Posting, TermFrequency};
pub use search::{
    LookupExecutor, LookupResult, LookupSegmentStats, LookupStats, LookupTimings,
    MultiSegmentDictionaryStats, MultiSegmentPostings, MultiSegmentQueryEngine,
    MultiSegmentTermQueryResult, QueryEngine, SegmentDecodeError, TermLookupStats, TermQueryResult,
};
pub use storage::index_storage::{CounterError, IndexStorage, IndexStorageError};
pub use storage::manifest::{Manifest, ManifestError, SegmentMetadata};
pub use storage::manifest_codec::{decode as decode_manifest, encode as encode_manifest};
pub use storage::postings::DecodeError;
pub use storage::varint::DecodeError as VarintDecodeError;
