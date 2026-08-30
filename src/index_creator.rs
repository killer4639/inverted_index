use crate::doc_validator::validate_doc;
use crate::index_builder::IndexBuilder;
use crate::index_codec;
use crate::index_creation_stats::{
    CorpusStats, IndexCreationStats, IndexCreationTimings, IndexStats, SegmentStats,
};
use std::fs;
use std::time::Instant;

pub fn create_index(corpus_path: &str, segment_path: &str) -> Result<IndexCreationStats, String> {
    let total_started = Instant::now();
    let input_bytes = fs::metadata(corpus_path)
        .map_err(|error| format!("failed to read document file metadata: {error}"))?
        .len();

    let validation_started = Instant::now();
    validate_doc(corpus_path)?;
    let validation_duration = validation_started.elapsed();

    let mut builder = IndexBuilder::new();
    let indexing_started = Instant::now();
    builder.create_index(corpus_path)?;
    let indexing_duration = indexing_started.elapsed();
    let token_count = builder.token_count();

    let finalization_started = Instant::now();
    let index = builder.finalize()?;
    let finalization_duration = finalization_started.elapsed();

    let segment_write_started = Instant::now();
    index_codec::encode(segment_path, &index)
        .map_err(|error| format!("failed to write segment: {error}"))?;
    let segment_write_duration = segment_write_started.elapsed();

    let segment_bytes = fs::metadata(segment_path)
        .map_err(|error| format!("failed to read segment metadata: {error}"))?
        .len();

    Ok(IndexCreationStats {
        corpus: CorpusStats {
            input_bytes,
            document_count: index.document_count(),
            token_count,
        },
        index: IndexStats {
            unique_term_count: index.term_count(),
            posting_count: index.posting_count(),
        },
        segment: SegmentStats { segment_bytes },
        timings: IndexCreationTimings {
            validation_duration,
            indexing_duration,
            finalization_duration,
            segment_write_duration,
            total_duration: total_started.elapsed(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn reports_index_creation_statistics() {
        let id = TEST_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let corpus_path = std::env::temp_dir().join(format!(
            "index-creator-corpus-{}-{id}.txt",
            std::process::id()
        ));
        let segment_path = std::env::temp_dir().join(format!(
            "index-creator-segment-{}-{id}.idx",
            std::process::id()
        ));
        fs::write(&corpus_path, "rust rust search\nsearch engine\n").unwrap();

        let stats = create_index(
            corpus_path.to_str().unwrap(),
            segment_path.to_str().unwrap(),
        )
        .unwrap();

        assert_eq!(stats.corpus.input_bytes, 31);
        assert_eq!(stats.corpus.document_count, 2);
        assert_eq!(stats.corpus.token_count, 5);
        assert_eq!(stats.index.unique_term_count, 3);
        assert_eq!(stats.index.posting_count, 4);
        assert_eq!(
            stats.segment.segment_bytes,
            fs::metadata(&segment_path).unwrap().len()
        );

        let measured_duration = stats.timings.validation_duration
            + stats.timings.indexing_duration
            + stats.timings.finalization_duration
            + stats.timings.segment_write_duration;
        assert!(stats.timings.total_duration >= measured_duration);

        fs::remove_file(corpus_path).unwrap();
        fs::remove_file(segment_path).unwrap();
    }
}
