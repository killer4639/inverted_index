use crate::search::multi_segment_postings::MultiSegmentPostings;
use crate::search::query_engine::{QueryEngine, validate_query_term};
use crate::storage::manifest::{Manifest, SegmentMetadata};
use std::io;
use std::path::Path;

pub struct MultiSegmentQueryEngine {
    segments: Vec<ManagedSegment>,
}

struct ManagedSegment {
    metadata: SegmentMetadata,
    query_engine: QueryEngine,
}

pub struct MultiSegmentTermQueryResult<'a> {
    pub postings: MultiSegmentPostings<'a>,
    pub dictionary_stats: MultiSegmentDictionaryStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultiSegmentDictionaryStats {
    pub searched_segment_count: usize,
    pub matching_segment_count: usize,
    pub dictionary_comparisons: u64,
    pub encoded_postings_bytes: usize,
}

impl MultiSegmentQueryEngine {
    pub fn new(manifest: Manifest, segment_directory: impl AsRef<Path>) -> io::Result<Self> {
        let segments = manifest.into_segments();
        let mut managed_segments = Vec::with_capacity(segments.len());

        for segment in segments {
            let segment_path = segment_directory.as_ref().join(&segment.file_name);
            let query_engine = QueryEngine::new(segment_path)?;
            managed_segments.push(ManagedSegment {
                metadata: segment,
                query_engine,
            });
        }

        Ok(Self {
            segments: managed_segments,
        })
    }

    pub fn query_term(&self, term: &str) -> io::Result<MultiSegmentPostings<'_>> {
        Ok(self.query_term_with_stats(term)?.postings)
    }

    pub fn query_term_with_stats(&self, term: &str) -> io::Result<MultiSegmentTermQueryResult<'_>> {
        validate_query_term(term)?;

        let mut segment_decoders = Vec::new();
        let mut matching_segment_count = 0;
        let mut dictionary_comparisons = 0_u64;
        let mut encoded_postings_bytes = 0_usize;

        for segment in &self.segments {
            let query_result = segment.query_engine.query_term_with_stats(term)?;
            dictionary_comparisons = dictionary_comparisons
                .checked_add(u64::from(query_result.dictionary_comparisons))
                .ok_or_else(|| invalid_data("dictionary comparison count overflow"))?;

            if let Some(decoder) = query_result.postings {
                matching_segment_count += 1;
                encoded_postings_bytes = encoded_postings_bytes
                    .checked_add(decoder.remaining_bytes())
                    .ok_or_else(|| invalid_data("encoded postings byte count overflow"))?;
                segment_decoders.push((segment.metadata.id, decoder));
            }
        }

        Ok(MultiSegmentTermQueryResult {
            postings: MultiSegmentPostings::new(segment_decoders),
            dictionary_stats: MultiSegmentDictionaryStats {
                searched_segment_count: self.segments.len(),
                matching_segment_count,
                dictionary_comparisons,
                encoded_postings_bytes,
            },
        })
    }

    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    pub fn total_document_count(&self) -> u64 {
        let mut total = 0_u64;
        for segment in &self.segments {
            total = total
                .checked_add(u64::from(segment.metadata.document_count))
                .expect("validated manifest document count cannot overflow");
        }
        total
    }

    pub fn total_segment_bytes(&self) -> u64 {
        let mut total = 0_u64;
        for segment in &self.segments {
            total = total
                .checked_add(segment.metadata.length_bytes)
                .expect("validated manifest segment bytes cannot overflow");
        }
        total
    }

    pub fn total_term_entries(&self) -> u64 {
        let mut total = 0_u64;
        for segment in &self.segments {
            total = total
                .checked_add(u64::from(segment.metadata.term_count))
                .expect("validated manifest term entries cannot overflow");
        }
        total
    }
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::MultiSegmentQueryEngine;
    use crate::model::{InvertedIndex, Posting};
    use crate::storage::manifest::{
        MANIFEST_FILE_PREFIX, Manifest, SEGMENT_FILE_PREFIX, SEGMENT_FILE_SUFFIX, SegmentMetadata,
    };
    use crate::storage::segment_codec;
    use crate::storage::segment_reader::SegmentReader;
    use crate::{AddressedPosting, DocumentAddress, SegmentId};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn queries_multiple_segments_in_manifest_order() {
        let directory = test_directory();
        let first = write_segment(&directory, 1, 2, vec![posting(0, 2)]);
        let second = write_segment(&directory, 2, 3, vec![posting(0, 3), posting(2, 1)]);
        let manifest = Manifest::new(vec![first, second]).unwrap();
        let engine = MultiSegmentQueryEngine::new(manifest, &directory).unwrap();

        let result = engine.query_term_with_stats("rust").unwrap();
        let stats = result.dictionary_stats;
        let postings = result.postings.collect::<Result<Vec<_>, _>>().unwrap();

        assert_eq!(
            postings,
            vec![
                addressed_posting(1, 0, 2),
                addressed_posting(2, 0, 3),
                addressed_posting(2, 2, 1),
            ]
        );
        assert_eq!(stats.searched_segment_count, 2);
        assert_eq!(stats.matching_segment_count, 2);
        assert!(stats.dictionary_comparisons > 0);
        assert!(stats.encoded_postings_bytes > 0);

        drop(engine);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn missing_term_and_empty_manifest_return_empty_results() {
        let directory = test_directory();
        let segment = write_segment(&directory, 1, 1, vec![posting(0, 1)]);
        let manifest = Manifest::new(vec![segment]).unwrap();
        let engine = MultiSegmentQueryEngine::new(manifest, &directory).unwrap();

        let missing = engine.query_term_with_stats("missing").unwrap();
        assert_eq!(missing.dictionary_stats.matching_segment_count, 0);
        assert_eq!(missing.postings.count(), 0);

        drop(engine);
        fs::remove_dir_all(&directory).unwrap();

        let empty = Manifest::new(Vec::new()).unwrap();
        let empty_engine = MultiSegmentQueryEngine::new(empty, &directory).unwrap();
        let empty_result = empty_engine.query_term_with_stats("rust").unwrap();
        assert_eq!(empty_result.dictionary_stats.searched_segment_count, 0);
        assert_eq!(empty_result.postings.count(), 0);
    }

    fn test_directory() -> PathBuf {
        let id = TEST_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "{MANIFEST_FILE_PREFIX}query-engine-{}-{id}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        directory
    }

    fn write_segment(
        directory: &Path,
        segment_id: u64,
        document_count: u32,
        rust_postings: Vec<Posting>,
    ) -> SegmentMetadata {
        let file_name = format!("{SEGMENT_FILE_PREFIX}{segment_id:016}{SEGMENT_FILE_SUFFIX}");
        let path = directory.join(&file_name);
        let mut terms = BTreeMap::new();
        terms.insert("rust".to_owned(), rust_postings);
        let index = InvertedIndex::from_finalized_postings(terms, document_count);
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

    fn addressed_posting(
        segment_id: u64,
        local_document_id: u32,
        term_frequency: u32,
    ) -> AddressedPosting {
        AddressedPosting {
            address: DocumentAddress {
                segment_id: SegmentId::new(segment_id).unwrap(),
                local_document_id,
            },
            term_frequency,
        }
    }
}
