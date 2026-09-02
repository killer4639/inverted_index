use crate::model::document_address::AddressedPosting;
use crate::search::stats::{LookupStats, LookupTimings, SnapshotStats, TermLookupStats};
use crate::storage::index_snapshot::IndexSnapshot;
use std::time::Instant;

pub struct LookupResult {
    pub postings: Vec<AddressedPosting>,
    pub stats: LookupStats,
}

pub struct LookupExecutor;

impl Default for LookupExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl LookupExecutor {
    pub fn new() -> Self {
        Self
    }

    pub fn lookup(
        &self,
        index_snapshot: &IndexSnapshot,
        term: &str,
    ) -> Result<LookupResult, String> {
        let total_started = Instant::now();
        let dictionary_lookup_started = Instant::now();
        let query_result = index_snapshot
            .query_term_with_stats(term)
            .map_err(|error| format!("failed to query index: {error}"))?;
        let dictionary_lookup_duration = dictionary_lookup_started.elapsed();
        let dictionary_stats = query_result.dictionary_stats;

        let postings_decode_started = Instant::now();
        let mut postings = Vec::new();
        for posting in query_result.postings {
            let posting = posting.map_err(|error| {
                format!(
                    "failed to decode postings in segment {}: {:?}",
                    error.segment_id, error.source
                )
            })?;
            postings.push(posting);
        }
        let postings_decode_duration = postings_decode_started.elapsed();

        let stats = LookupStats {
            snapshot: SnapshotStats {
                manifest_generation: index_snapshot.generation(),
                segment_count: index_snapshot.segment_count(),
                total_segment_bytes: index_snapshot.total_segment_bytes(),
                total_document_count: index_snapshot.total_document_count(),
                total_term_entries: index_snapshot.total_term_entries(),
            },
            query: TermLookupStats {
                searched_segment_count: dictionary_stats.searched_segment_count,
                matching_segment_count: dictionary_stats.matching_segment_count,
                dictionary_comparisons: dictionary_stats.dictionary_comparisons,
                matched_document_count: postings.len(),
                encoded_postings_bytes: dictionary_stats.encoded_postings_bytes,
            },
            timings: LookupTimings {
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
    use crate::SegmentId;
    use crate::model::{InvertedIndex, Posting};
    use crate::storage::manifest::{
        FILE_NUMBER_WIDTH, Manifest, SEGMENT_FILE_PREFIX, SEGMENT_FILE_SUFFIX, SegmentMetadata,
    };
    use crate::storage::segment_codec;
    use crate::storage::segment_reader::SegmentReader;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn reports_snapshot_query_work_and_timings() {
        let segment_directory = test_segment_directory();
        let metadata = write_segment(&segment_directory);
        let manifest = Manifest::new(vec![metadata]).unwrap();
        let index_snapshot =
            IndexSnapshot::from_manifest(7, &manifest, &segment_directory).unwrap();
        let executor = LookupExecutor::new();

        let found = executor.lookup(&index_snapshot, "rust").unwrap();
        assert_eq!(found.postings.len(), 2);
        assert_eq!(found.stats.snapshot.manifest_generation, Some(7));
        assert_eq!(found.stats.snapshot.segment_count, 1);
        assert_eq!(found.stats.snapshot.total_document_count, 3);
        assert_eq!(found.stats.snapshot.total_term_entries, 2);
        assert!(found.stats.snapshot.total_segment_bytes > 0);
        assert_eq!(found.stats.query.searched_segment_count, 1);
        assert_eq!(found.stats.query.matching_segment_count, 1);
        assert!(found.stats.query.dictionary_comparisons > 0);
        assert_eq!(found.stats.query.matched_document_count, 2);
        assert!(found.stats.query.encoded_postings_bytes > 0);
        assert!(found.stats.query.term_found());
        assert!(
            found.stats.timings.total_duration
                >= found.stats.timings.dictionary_lookup_duration
                    + found.stats.timings.postings_decode_duration
        );

        let missing = executor.lookup(&index_snapshot, "missing").unwrap();
        assert!(missing.postings.is_empty());
        assert_eq!(missing.stats.query.searched_segment_count, 1);
        assert_eq!(missing.stats.query.matching_segment_count, 0);
        assert_eq!(missing.stats.query.matched_document_count, 0);
        assert_eq!(missing.stats.query.encoded_postings_bytes, 0);
        assert!(!missing.stats.query.term_found());

        drop(index_snapshot);
        fs::remove_dir_all(segment_directory).unwrap();
    }

    fn test_segment_directory() -> PathBuf {
        let id = TEST_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let segment_directory =
            std::env::temp_dir().join(format!("lookup-executor-index-{}-{id}", std::process::id()));
        fs::create_dir(&segment_directory).unwrap();
        segment_directory
    }

    fn write_segment(segment_directory: &Path) -> SegmentMetadata {
        let segment_id = 1;
        let file_name = format!(
            "{SEGMENT_FILE_PREFIX}{segment_id:0width$}{SEGMENT_FILE_SUFFIX}",
            width = FILE_NUMBER_WIDTH
        );
        let path = segment_directory.join(&file_name);
        let mut postings = BTreeMap::new();
        postings.insert("rust".to_owned(), vec![posting(0, 2), posting(2, 1)]);
        postings.insert("search".to_owned(), vec![posting(1, 1)]);
        let index = InvertedIndex::from_finalized_postings(postings, 3);
        segment_codec::encode(&path, &index).unwrap();

        let reader = SegmentReader::open(&path).unwrap();
        SegmentMetadata {
            id: SegmentId::new(segment_id).unwrap(),
            file_name,
            document_count: reader.document_count(),
            term_count: reader.term_count(),
            length_bytes: fs::metadata(path).unwrap().len(),
            checksum: reader.checksum(),
        }
    }

    fn posting(document_id: u32, term_frequency: u32) -> Posting {
        Posting {
            document_id,
            term_frequency,
        }
    }
}
