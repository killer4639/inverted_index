use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupStats {
    pub snapshot: SnapshotStats,
    pub query: TermLookupStats,
    pub timings: LookupTimings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotStats {
    pub manifest_generation: Option<u64>,
    pub segment_count: usize,
    pub total_segment_bytes: u64,
    pub total_document_count: u64,
    pub total_term_entries: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermLookupStats {
    pub searched_segment_count: usize,
    pub matching_segment_count: usize,
    pub dictionary_comparisons: u64,
    pub matched_document_count: usize,
    pub encoded_postings_bytes: usize,
}

impl TermLookupStats {
    pub fn term_found(&self) -> bool {
        self.matching_segment_count > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupTimings {
    pub dictionary_lookup_duration: Duration,
    pub postings_decode_duration: Duration,
    pub total_duration: Duration,
}
