use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupStats {
    pub segment: LookupSegmentStats,
    pub query: TermLookupStats,
    pub timings: LookupTimings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupSegmentStats {
    pub opened_for_lookup: bool,
    pub segment_bytes: u64,
    pub document_count: u32,
    pub term_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermLookupStats {
    pub term_found: bool,
    pub dictionary_comparisons: u32,
    pub matched_document_count: usize,
    pub postings_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupTimings {
    pub segment_open_duration: Duration,
    pub dictionary_lookup_duration: Duration,
    pub postings_decode_duration: Duration,
    pub total_duration: Duration,
}
