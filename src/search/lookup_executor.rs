use crate::model::Posting;
use crate::search::query_engine::QueryEngine;
use crate::search::stats::{LookupSegmentStats, LookupStats, LookupTimings, TermLookupStats};
use std::fs;
use std::time::{Duration, Instant};

pub struct LookupResult {
    pub postings: Vec<Posting>,
    pub stats: LookupStats,
}

pub struct LookupExecutor {
    open_segment: Option<OpenSegment>,
}

impl Default for LookupExecutor {
    fn default() -> Self {
        Self::new()
    }
}

struct OpenSegment {
    path: String,
    bytes: u64,
    engine: QueryEngine,
}

impl LookupExecutor {
    pub fn new() -> Self {
        Self { open_segment: None }
    }

    pub fn lookup(&mut self, segment_path: &str, term: &str) -> Result<LookupResult, String> {
        let total_started = Instant::now();
        let should_open = match self.open_segment.as_ref() {
            Some(open_segment) => open_segment.path != segment_path,
            None => true,
        };

        let segment_open_duration = if should_open {
            let segment_open_started = Instant::now();
            let engine = QueryEngine::new(segment_path)
                .map_err(|error| format!("failed to open segment: {error}"))?;
            let bytes = fs::metadata(segment_path)
                .map_err(|error| format!("failed to read segment metadata: {error}"))?
                .len();

            self.open_segment = Some(OpenSegment {
                path: segment_path.to_owned(),
                bytes,
                engine,
            });
            segment_open_started.elapsed()
        } else {
            Duration::ZERO
        };

        let open_segment = self
            .open_segment
            .as_ref()
            .ok_or_else(|| "segment is not open".to_owned())?;

        let dictionary_lookup_started = Instant::now();
        let query_result = open_segment
            .engine
            .query_term_with_stats(term)
            .map_err(|error| error.to_string())?;
        let dictionary_lookup_duration = dictionary_lookup_started.elapsed();

        let dictionary_comparisons = query_result.dictionary_comparisons;
        let term_found = query_result.postings.is_some();
        let postings_decode_started = Instant::now();
        let mut postings = Vec::new();
        let mut postings_bytes = 0;

        if let Some(decoder) = query_result.postings {
            postings_bytes = decoder.remaining_bytes();
            postings.reserve(decoder.remaining_postings());

            for result in decoder {
                postings
                    .push(result.map_err(|error| format!("failed to decode postings: {error:?}"))?);
            }
        }

        let postings_decode_duration = postings_decode_started.elapsed();
        let stats = LookupStats {
            segment: LookupSegmentStats {
                opened_for_lookup: should_open,
                segment_bytes: open_segment.bytes,
                document_count: open_segment.engine.document_count(),
                term_count: open_segment.engine.term_count(),
            },
            query: TermLookupStats {
                term_found,
                dictionary_comparisons,
                matched_document_count: postings.len(),
                postings_bytes,
            },
            timings: LookupTimings {
                segment_open_duration,
                dictionary_lookup_duration,
                postings_decode_duration,
                total_duration: total_started.elapsed(),
            },
        };

        Ok(LookupResult { postings, stats })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexing::create_index;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    fn write_segment() -> std::path::PathBuf {
        let id = TEST_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let corpus_path = std::env::temp_dir().join(format!(
            "lookup-executor-corpus-{}-{id}.txt",
            std::process::id()
        ));
        let path = std::env::temp_dir().join(format!(
            "lookup-executor-segment-{}-{id}.idx",
            std::process::id()
        ));
        fs::write(&corpus_path, "rust rust\nsearch\nrust\n").unwrap();
        create_index(corpus_path.to_str().unwrap(), path.to_str().unwrap()).unwrap();
        fs::remove_file(corpus_path).unwrap();
        path
    }

    #[test]
    fn reports_lookup_work_and_reuses_the_open_segment() {
        let path = write_segment();
        let path_text = path.to_str().unwrap();
        let mut executor = LookupExecutor::new();

        let first = executor.lookup(path_text, "rust").unwrap();
        assert_eq!(first.postings.len(), 2);
        assert!(first.stats.segment.opened_for_lookup);
        assert_eq!(first.stats.segment.document_count, 3);
        assert_eq!(first.stats.segment.term_count, 2);
        assert!(first.stats.query.term_found);
        assert!(first.stats.query.dictionary_comparisons > 0);
        assert_eq!(first.stats.query.matched_document_count, 2);
        assert!(first.stats.query.postings_bytes > 0);

        let second = executor.lookup(path_text, "missing").unwrap();
        assert!(!second.stats.segment.opened_for_lookup);
        assert_eq!(second.stats.timings.segment_open_duration, Duration::ZERO);
        assert!(!second.stats.query.term_found);
        assert!(second.postings.is_empty());
        assert_eq!(second.stats.query.postings_bytes, 0);

        fs::remove_file(path).unwrap();
    }
}
