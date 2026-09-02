use crate::SegmentId;
use std::collections::HashSet;
use std::path::{Component, Path};

pub const MANIFEST_FILE_PREFIX: &str = "manifest-";
pub const MANIFEST_FILE_SUFFIX: &str = ".bin";
pub const SEGMENT_FILE_PREFIX: &str = "segment-";
pub const SEGMENT_FILE_SUFFIX: &str = ".idx";
pub const FILE_NUMBER_WIDTH: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    SegmentIdsNotStrictlyIncreasing {
        previous: SegmentId,
        current: SegmentId,
    },
    DuplicateFileName(String),
    UnsafeFileName(String),
    TotalDocumentCountOverflow,
    TotalSegmentBytesOverflow,
    TotalTermEntriesOverflow,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Manifest {
    segments: Vec<SegmentMetadata>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SegmentMetadata {
    pub id: SegmentId,
    pub file_name: String,
    pub document_count: u32,
    pub term_count: u32,
    pub length_bytes: u64,
    pub checksum: u32,
}

impl Manifest {
    pub fn new(segments: Vec<SegmentMetadata>) -> Result<Self, ManifestError> {
        validate_segments(&segments)?;
        Ok(Self { segments })
    }

    pub(crate) fn from_decoded_segments_unchecked(segments: Vec<SegmentMetadata>) -> Self {
        Self { segments }
    }

    pub(crate) fn validate(&self) -> Result<(), ManifestError> {
        validate_segments(&self.segments)
    }

    pub fn segments(&self) -> &[SegmentMetadata] {
        &self.segments
    }

    pub fn into_segments(self) -> Vec<SegmentMetadata> {
        self.segments
    }

    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    pub fn total_document_count(&self) -> u64 {
        let mut total = 0_u64;
        for segment in &self.segments {
            total = total
                .checked_add(u64::from(segment.document_count))
                .expect("Manifest::new validates total document count");
        }
        total
    }

    pub fn total_segment_bytes(&self) -> u64 {
        let mut total = 0_u64;
        for segment in &self.segments {
            total = total
                .checked_add(segment.length_bytes)
                .expect("Manifest::new validates total segment bytes");
        }
        total
    }

    pub fn total_term_entries(&self) -> u64 {
        let mut total = 0_u64;
        for segment in &self.segments {
            total = total
                .checked_add(u64::from(segment.term_count))
                .expect("Manifest::new validates total term entries");
        }
        total
    }
}

fn validate_segments(segments: &[SegmentMetadata]) -> Result<(), ManifestError> {
    let mut file_names = HashSet::with_capacity(segments.len());
    let mut previous_segment_id = None;
    let mut total_document_count = 0_u64;
    let mut total_segment_bytes = 0_u64;
    let mut total_term_entries = 0_u64;

    for segment in segments {
        if let Some(previous) = previous_segment_id {
            if segment.id <= previous {
                return Err(ManifestError::SegmentIdsNotStrictlyIncreasing {
                    previous,
                    current: segment.id,
                });
            }
        }
        previous_segment_id = Some(segment.id);

        if !is_safe_file_name(&segment.file_name) {
            return Err(ManifestError::UnsafeFileName(segment.file_name.clone()));
        }
        if !file_names.insert(segment.file_name.as_str()) {
            return Err(ManifestError::DuplicateFileName(segment.file_name.clone()));
        }

        total_document_count = total_document_count
            .checked_add(u64::from(segment.document_count))
            .ok_or(ManifestError::TotalDocumentCountOverflow)?;
        total_segment_bytes = total_segment_bytes
            .checked_add(segment.length_bytes)
            .ok_or(ManifestError::TotalSegmentBytesOverflow)?;
        total_term_entries = total_term_entries
            .checked_add(u64::from(segment.term_count))
            .ok_or(ManifestError::TotalTermEntriesOverflow)?;
    }

    Ok(())
}

fn is_safe_file_name(file_name: &str) -> bool {
    if file_name.is_empty() || file_name.contains('/') || file_name.contains('\\') {
        return false;
    }

    let mut components = Path::new(file_name).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    )
}

#[cfg(test)]
mod tests {
    use super::{Manifest, ManifestError, SegmentMetadata};
    use crate::SegmentId;

    #[test]
    fn accepts_empty_and_populated_manifests() {
        let empty = Manifest::new(Vec::new()).unwrap();
        assert!(empty.segments().is_empty());

        let populated = Manifest::new(vec![
            segment(1, "segment-0001.idx", 10),
            segment(2, "segment-0002.idx", 20),
        ])
        .unwrap();
        assert_eq!(populated.segments().len(), 2);
        assert_eq!(populated.segment_count(), 2);
        assert_eq!(populated.total_document_count(), 20);
        assert_eq!(populated.total_segment_bytes(), 30);
        assert_eq!(populated.total_term_entries(), 10);
    }

    #[test]
    fn rejects_duplicate_and_unordered_segment_ids() {
        let duplicate = manifest_error(vec![
            segment(1, "segment-0001.idx", 10),
            segment(1, "segment-0002.idx", 20),
        ]);
        assert!(matches!(
            duplicate,
            ManifestError::SegmentIdsNotStrictlyIncreasing { .. }
        ));

        let unordered = manifest_error(vec![
            segment(2, "segment-0002.idx", 20),
            segment(1, "segment-0001.idx", 10),
        ]);
        assert!(matches!(
            unordered,
            ManifestError::SegmentIdsNotStrictlyIncreasing { .. }
        ));
    }

    #[test]
    fn rejects_duplicate_and_unsafe_file_names() {
        let duplicate = manifest_error(vec![
            segment(1, "segment.idx", 10),
            segment(2, "segment.idx", 20),
        ]);
        assert_eq!(
            duplicate,
            ManifestError::DuplicateFileName("segment.idx".to_owned())
        );

        let unsafe_name = manifest_error(vec![segment(1, "../segment.idx", 10)]);
        assert_eq!(
            unsafe_name,
            ManifestError::UnsafeFileName("../segment.idx".to_owned())
        );
    }

    #[test]
    fn rejects_total_segment_bytes_overflow() {
        let error = manifest_error(vec![
            segment(1, "segment-0001.idx", u64::MAX),
            segment(2, "segment-0002.idx", 1),
        ]);
        assert_eq!(error, ManifestError::TotalSegmentBytesOverflow);
    }

    fn segment(id: u64, file_name: &str, length_bytes: u64) -> SegmentMetadata {
        SegmentMetadata {
            id: SegmentId::new(id).unwrap(),
            file_name: file_name.to_owned(),
            document_count: 10,
            term_count: 5,
            length_bytes,
            checksum: 0,
        }
    }

    fn manifest_error(segments: Vec<SegmentMetadata>) -> ManifestError {
        match Manifest::new(segments) {
            Ok(_) => panic!("manifest construction unexpectedly succeeded"),
            Err(error) => error,
        }
    }
}
