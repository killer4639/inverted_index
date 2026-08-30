use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexCreationStats {
    pub corpus: CorpusStats,
    pub index: IndexStats,
    pub segment: SegmentStats,
    pub timings: IndexCreationTimings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusStats {
    pub input_bytes: u64,
    pub document_count: u32,
    pub token_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexStats {
    pub unique_term_count: usize,
    pub posting_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentStats {
    pub segment_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexCreationTimings {
    pub validation_duration: Duration,
    pub indexing_duration: Duration,
    pub finalization_duration: Duration,
    pub segment_write_duration: Duration,
    pub total_duration: Duration,
}
