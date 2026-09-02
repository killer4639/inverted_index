use crate::{
    IndexStorage, IndexStorageError, Manifest, ManifestError, MultiSegmentPostings, QueryEngine,
    SegmentMetadata,
};
use std::io;
use std::path::Path;

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

pub struct ManagedSegment {
    metadata: SegmentMetadata,
    query_engine: QueryEngine,
}

pub struct IndexSnapshot {
    generation: Option<u64>,
    segments: Vec<ManagedSegment>,
}

impl IndexSnapshot {
    pub fn new(index_storage: &IndexStorage) -> Result<IndexSnapshot, IndexStorageError> {
        let Some((generation, manifest)) = index_storage.open_latest_manifest_with_gen()? else {
            return Ok(Self::empty());
        };

        Self::from_manifest(generation, &manifest, &index_storage.segment_directory())
            .map_err(IndexStorageError::from)
    }

    pub fn refresh(
        &mut self,
        generation: u64,
        manifest: Manifest,
        index_storage: &IndexStorage,
    ) -> Result<(), IndexStorageError> {
        let refreshed =
            Self::from_manifest(generation, &manifest, &index_storage.segment_directory())?;
        *self = refreshed;
        Ok(())
    }

    pub fn manifest_with_segment(
        &self,
        new_segment: SegmentMetadata,
    ) -> Result<Manifest, ManifestError> {
        let segment_count = self
            .segments
            .len()
            .checked_add(1)
            .ok_or(ManifestError::SegmentCountOverflow)?;
        let mut segments = Vec::with_capacity(segment_count);

        for segment in &self.segments {
            segments.push(segment.metadata.clone());
        }
        segments.push(new_segment);
        Manifest::new(segments)
    }

    fn empty() -> Self {
        Self {
            generation: None,
            segments: Vec::new(),
        }
    }

    pub(crate) fn from_manifest(
        generation: u64,
        manifest: &Manifest,
        segment_directory: &Path,
    ) -> io::Result<Self> {
        if generation == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "manifest generation must be greater than zero",
            ));
        }

        let mut managed_segments = Vec::with_capacity(manifest.segment_count());

        for segment in manifest.segments() {
            let segment_path = segment_directory.join(&segment.file_name);
            let query_engine = QueryEngine::new(segment_path)?;
            managed_segments.push(ManagedSegment {
                metadata: segment.clone(),
                query_engine,
            });
        }

        Ok(Self {
            generation: Some(generation),
            segments: managed_segments,
        })
    }

    pub fn generation(&self) -> Option<u64> {
        self.generation
    }

    pub fn query_term(&self, term: &str) -> io::Result<MultiSegmentPostings<'_>> {
        Ok(self.query_term_with_stats(term)?.postings)
    }

    pub fn query_term_with_stats(&self, term: &str) -> io::Result<MultiSegmentTermQueryResult<'_>> {
        crate::search::query_engine::validate_query_term(term)?;

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
    use super::IndexSnapshot;
    use crate::model::{InvertedIndex, Posting};
    use crate::storage::manifest::{
        FILE_NUMBER_WIDTH, Manifest, SEGMENT_FILE_PREFIX, SEGMENT_FILE_SUFFIX, SegmentMetadata,
    };
    use crate::storage::segment_codec;
    use crate::storage::segment_reader::SegmentReader;
    use crate::{AddressedPosting, DocumentAddress, IndexStorage, SegmentId, encode_manifest};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn published_manifest_refreshes_and_reopens_snapshot() {
        let index_directory = test_index_directory();
        let index_storage = IndexStorage::new_at(&index_directory).unwrap();
        let mut index_snapshot = IndexSnapshot::new(&index_storage).unwrap();
        assert_eq!(index_snapshot.generation(), None);

        let segment_id = index_storage.reserve_segment_id().unwrap();
        let expected_metadata = write_segment(
            &index_storage.segment_directory(),
            segment_id,
            2,
            vec![posting(0, 2), posting(1, 1)],
        );
        let segment_metadata = index_storage.load_segment_metadata(segment_id).unwrap();
        assert_eq!(segment_metadata, expected_metadata);

        let manifest = index_snapshot
            .manifest_with_segment(segment_metadata)
            .unwrap();
        let generation = index_storage.reserve_manifest_generation().unwrap();
        let manifest_path = index_storage.manifest_file_path(generation);
        encode_manifest(&manifest_path, &manifest).unwrap();
        index_snapshot
            .refresh(generation, manifest, &index_storage)
            .unwrap();

        assert_eq!(index_snapshot.generation(), Some(1));
        assert_eq!(index_snapshot.segment_count(), 1);

        let reopened_snapshot = IndexSnapshot::new(&index_storage).unwrap();
        assert_eq!(reopened_snapshot.generation(), Some(1));
        let postings = reopened_snapshot
            .query_term("rust")
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            postings,
            vec![addressed_posting(1, 0, 2), addressed_posting(1, 1, 1)]
        );

        drop(reopened_snapshot);
        drop(index_snapshot);
        fs::remove_dir_all(index_directory).unwrap();
    }

    #[test]
    fn queries_multiple_segments_in_manifest_order() {
        let segment_directory = test_segment_directory();
        let first = write_segment(&segment_directory, 1, 2, vec![posting(0, 2)]);
        let second = write_segment(&segment_directory, 2, 3, vec![posting(0, 3), posting(2, 1)]);
        let manifest = Manifest::new(vec![first, second]).unwrap();
        let index_snapshot =
            IndexSnapshot::from_manifest(1, &manifest, &segment_directory).unwrap();

        let result = index_snapshot.query_term_with_stats("rust").unwrap();
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

        drop(index_snapshot);
        fs::remove_dir_all(segment_directory).unwrap();
    }

    #[test]
    fn missing_term_and_empty_manifest_return_empty_results() {
        let segment_directory = test_segment_directory();
        let segment = write_segment(&segment_directory, 1, 1, vec![posting(0, 1)]);
        let manifest = Manifest::new(vec![segment]).unwrap();
        let index_snapshot =
            IndexSnapshot::from_manifest(1, &manifest, &segment_directory).unwrap();

        let missing = index_snapshot.query_term_with_stats("missing").unwrap();
        assert_eq!(missing.dictionary_stats.matching_segment_count, 0);
        assert_eq!(missing.postings.count(), 0);

        drop(index_snapshot);
        fs::remove_dir_all(&segment_directory).unwrap();

        let empty = Manifest::new(Vec::new()).unwrap();
        let empty_index_snapshot =
            IndexSnapshot::from_manifest(2, &empty, &segment_directory).unwrap();
        let empty_result = empty_index_snapshot.query_term_with_stats("rust").unwrap();
        assert_eq!(empty_result.dictionary_stats.searched_segment_count, 0);
        assert_eq!(empty_result.postings.count(), 0);
    }

    fn test_segment_directory() -> PathBuf {
        let id = TEST_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let segment_directory =
            std::env::temp_dir().join(format!("index-snapshot-{}-{id}", std::process::id()));
        fs::create_dir(&segment_directory).unwrap();
        segment_directory
    }

    fn test_index_directory() -> PathBuf {
        let id = TEST_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "manifest-index-snapshot-{}-{id}",
            std::process::id()
        ))
    }

    fn write_segment(
        segment_directory: &Path,
        segment_id: u64,
        document_count: u32,
        rust_postings: Vec<Posting>,
    ) -> SegmentMetadata {
        let file_name = format!(
            "{SEGMENT_FILE_PREFIX}{segment_id:0width$}{SEGMENT_FILE_SUFFIX}",
            width = FILE_NUMBER_WIDTH
        );
        let path = segment_directory.join(&file_name);
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
