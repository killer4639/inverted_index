use crate::model::InvertedIndex;
use crate::storage::binary_file::{
    self, BinaryFileCodec, CHECKSUM_LENGTH, DecodedFile, Decoder, invalid_data, invalid_input,
};
use crate::storage::{postings, varint};
use std::io;
use std::ops::Range;
use std::path::Path;

pub const MAGIC: &[u8; 8] = b"INVIDX\0\0";
pub const FORMAT_VERSION: u16 = 1;
pub const HEADER_LENGTH: usize = 26;
pub const TERM_OFFSET_LENGTH: usize = 8;

pub(crate) struct SegmentCodec;

#[derive(Debug)]
pub(crate) struct SegmentLayout {
    pub document_count: u32,
    pub term_count: u32,
    pub term_offset_table: Range<usize>,
    pub term_records: Range<usize>,
    pub postings: Range<usize>,
}

struct SegmentSections {
    term_record_offsets: Vec<u64>,
    term_records: Vec<u8>,
    postings: Vec<u8>,
}

impl BinaryFileCodec for SegmentCodec {
    type Input = InvertedIndex;
    type Decoded = SegmentLayout;

    const FILE_TYPE: &'static str = "segment";
    const MAGIC: &'static [u8; 8] = MAGIC;
    const FORMAT_VERSION: u16 = FORMAT_VERSION;

    fn encode_body(_path: &Path, index: &Self::Input, output: &mut Vec<u8>) -> io::Result<()> {
        let term_count = u32::try_from(index.term_count())
            .map_err(|_| invalid_input("term count exceeds the segment format limit"))?;
        let sections = build_sections(index)?;

        let term_offset_table_length = index
            .term_count()
            .checked_mul(TERM_OFFSET_LENGTH)
            .ok_or_else(|| invalid_input("term-offset table length overflow"))?;
        let term_records_start = HEADER_LENGTH
            .checked_add(term_offset_table_length)
            .ok_or_else(|| invalid_input("term-record offset overflow"))?;
        let postings_offset = term_records_start
            .checked_add(sections.term_records.len())
            .ok_or_else(|| invalid_input("postings offset overflow"))?;
        let postings_offset_u64 = u64::try_from(postings_offset)
            .map_err(|_| invalid_input("postings offset exceeds u64"))?;

        let segment_length = postings_offset
            .checked_add(sections.postings.len())
            .and_then(|length| length.checked_add(CHECKSUM_LENGTH))
            .ok_or_else(|| invalid_input("segment length overflow"))?;
        let additional_capacity = segment_length
            .checked_sub(output.len())
            .ok_or_else(|| invalid_input("segment length is smaller than its header"))?;
        output.reserve(additional_capacity);

        output.extend_from_slice(&index.document_count().to_le_bytes());
        output.extend_from_slice(&term_count.to_le_bytes());
        output.extend_from_slice(&postings_offset_u64.to_le_bytes());

        let term_records_start_u64 = u64::try_from(term_records_start)
            .map_err(|_| invalid_input("term-record offset exceeds u64"))?;
        for relative_offset in sections.term_record_offsets {
            let absolute_offset = term_records_start_u64
                .checked_add(relative_offset)
                .ok_or_else(|| invalid_input("absolute term-record offset overflow"))?;
            output.extend_from_slice(&absolute_offset.to_le_bytes());
        }

        output.extend_from_slice(&sections.term_records);
        output.extend_from_slice(&sections.postings);
        assert_eq!(output.len() + CHECKSUM_LENGTH, segment_length);
        Ok(())
    }

    fn decode_body(_path: &Path, decoder: &mut Decoder<'_>) -> io::Result<Self::Decoded> {
        let document_count = decoder.read_u32("segment document count")?;
        let term_count = decoder.read_u32("segment term count")?;
        let postings_offset = decoder.read_u64("segment postings offset")?;
        let postings_start = usize_from_u64(
            postings_offset,
            "postings offset exceeds addressable memory",
        )?;
        let payload_length = decoder.length();
        if postings_start > payload_length {
            return Err(invalid_data("postings offset is past the checksum"));
        }

        let term_count_usize = usize::try_from(term_count)
            .map_err(|_| invalid_data("term count exceeds addressable memory"))?;
        let term_offset_table_length = term_count_usize
            .checked_mul(TERM_OFFSET_LENGTH)
            .ok_or_else(|| invalid_data("term-offset table length overflow"))?;
        let term_records_start = HEADER_LENGTH
            .checked_add(term_offset_table_length)
            .ok_or_else(|| invalid_data("term-record section overflow"))?;
        if term_records_start > postings_start {
            return Err(invalid_data("term records overlap the postings section"));
        }

        if term_count == 0 {
            return Ok(SegmentLayout {
                document_count,
                term_count,
                term_offset_table: HEADER_LENGTH..HEADER_LENGTH,
                term_records: HEADER_LENGTH..HEADER_LENGTH,
                postings: HEADER_LENGTH..HEADER_LENGTH,
            });
        }

        Ok(SegmentLayout {
            document_count,
            term_count,
            term_offset_table: HEADER_LENGTH..term_records_start,
            term_records: term_records_start..postings_start,
            postings: postings_start..payload_length,
        })
    }
}

pub(crate) fn encode(path: impl AsRef<Path>, index: &InvertedIndex) -> io::Result<()> {
    binary_file::encode::<SegmentCodec>(path, index)
}

pub(crate) fn decode_bytes(path: &Path, bytes: &[u8]) -> io::Result<DecodedFile<SegmentLayout>> {
    binary_file::decode_bytes::<SegmentCodec>(path, bytes)
}

fn build_sections(index: &InvertedIndex) -> io::Result<SegmentSections> {
    let mut term_record_offsets = Vec::with_capacity(index.term_count());
    let mut term_records = Vec::new();
    let mut postings = Vec::new();

    for (term, term_postings) in index.terms() {
        let term_record_offset = u64::try_from(term_records.len())
            .map_err(|_| invalid_input("term-record offset exceeds u64"))?;
        term_record_offsets.push(term_record_offset);

        let postings_offset = u64::try_from(postings.len())
            .map_err(|_| invalid_input("postings offset exceeds u64"))?;
        let postings_start = postings.len();
        postings::encode(term_postings, &mut postings).map_err(|error| {
            invalid_data(format!(
                "cannot encode postings for term '{term}': {error:?}"
            ))
        })?;
        let postings_length = postings
            .len()
            .checked_sub(postings_start)
            .ok_or_else(|| invalid_data("postings length underflow"))?;
        if postings_length == 0 {
            return Err(invalid_data(format!(
                "term '{term}' has no encoded postings"
            )));
        }
        let postings_length = u64::try_from(postings_length)
            .map_err(|_| invalid_input("postings length exceeds u64"))?;

        let term_length = u32::try_from(term.len())
            .map_err(|_| invalid_input("term length exceeds the segment format limit"))?;
        let document_frequency = u32::try_from(term_postings.len())
            .map_err(|_| invalid_input("document frequency exceeds the segment format limit"))?;
        if document_frequency == 0 {
            return Err(invalid_data(format!("term '{term}' has no postings")));
        }

        varint::encode(term_length, &mut term_records);
        term_records.extend_from_slice(term.as_bytes());
        varint::encode(document_frequency, &mut term_records);
        term_records.extend_from_slice(&postings_offset.to_le_bytes());
        term_records.extend_from_slice(&postings_length.to_le_bytes());
    }

    Ok(SegmentSections {
        term_record_offsets,
        term_records,
        postings,
    })
}

fn usize_from_u64(value: u64, message: &str) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| invalid_data(message))
}

#[cfg(test)]
fn build_segment(index: &InvertedIndex) -> io::Result<Vec<u8>> {
    binary_file::encode_bytes::<SegmentCodec>(Path::new("segment.idx"), index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Posting;
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    fn test_index() -> InvertedIndex {
        let mut postings = BTreeMap::new();
        postings.insert(
            "rust".to_owned(),
            vec![
                Posting {
                    document_id: 0,
                    term_frequency: 2,
                },
                Posting {
                    document_id: 3,
                    term_frequency: 1,
                },
            ],
        );
        postings.insert(
            "search".to_owned(),
            vec![Posting {
                document_id: 1,
                term_frequency: 1,
            }],
        );
        InvertedIndex::from_finalized_postings(postings, 4)
    }

    fn empty_index() -> InvertedIndex {
        InvertedIndex::from_finalized_postings(BTreeMap::new(), 0)
    }

    #[test]
    fn header_matches_the_format_contract() {
        let bytes = build_segment(&test_index()).unwrap();

        assert_eq!(&bytes[0..8], MAGIC);
        assert_eq!(&bytes[8..10], &FORMAT_VERSION.to_le_bytes());
        assert_eq!(&bytes[10..14], &4u32.to_le_bytes());
        assert_eq!(&bytes[14..18], &2u32.to_le_bytes());
        assert_eq!(&bytes[18..26], &88u64.to_le_bytes());
    }

    #[test]
    fn segment_contains_term_records_postings_and_footer() {
        let segment = build_segment(&test_index()).unwrap();

        assert_eq!(u64::from_le_bytes(segment[26..34].try_into().unwrap()), 42);
        assert_eq!(u64::from_le_bytes(segment[34..42].try_into().unwrap()), 64);
        assert_eq!(&segment[88..94], &[0x00, 0x02, 0x03, 0x01, 0x01, 0x01]);

        let checksum_offset = segment.len() - CHECKSUM_LENGTH;
        let stored_checksum = u32::from_le_bytes(segment[checksum_offset..].try_into().unwrap());
        assert_eq!(
            stored_checksum,
            crc32fast::hash(&segment[..checksum_offset])
        );
    }

    #[test]
    fn empty_segment_contains_only_header_and_footer() {
        let segment = build_segment(&empty_index()).unwrap();

        assert_eq!(segment.len(), HEADER_LENGTH + CHECKSUM_LENGTH);
        assert_eq!(u32::from_le_bytes(segment[14..18].try_into().unwrap()), 0);
        assert_eq!(
            u64::from_le_bytes(segment[18..26].try_into().unwrap()),
            HEADER_LENGTH as u64
        );
    }

    #[test]
    fn term_records_match_the_format_contract() {
        let segment = build_segment(&test_index()).unwrap();

        assert_eq!(segment[42], 4);
        assert_eq!(&segment[43..47], b"rust");
        assert_eq!(segment[47], 2);
        assert_eq!(u64::from_le_bytes(segment[48..56].try_into().unwrap()), 0);
        assert_eq!(u64::from_le_bytes(segment[56..64].try_into().unwrap()), 4);

        assert_eq!(segment[64], 6);
        assert_eq!(&segment[65..71], b"search");
        assert_eq!(segment[71], 1);
        assert_eq!(u64::from_le_bytes(segment[72..80].try_into().unwrap()), 4);
        assert_eq!(u64::from_le_bytes(segment[80..88].try_into().unwrap()), 2);
    }

    #[test]
    fn stored_offsets_and_posting_ranges_stay_in_their_sections() {
        let segment = build_segment(&test_index()).unwrap();
        let postings_start = u64::from_le_bytes(segment[18..26].try_into().unwrap()) as usize;
        let footer_start = segment.len() - CHECKSUM_LENGTH;
        let term_record_offsets = [
            u64::from_le_bytes(segment[26..34].try_into().unwrap()) as usize,
            u64::from_le_bytes(segment[34..42].try_into().unwrap()) as usize,
        ];

        for offset in term_record_offsets {
            assert!((42..postings_start).contains(&offset));
        }

        let posting_ranges = [(0usize, 4usize), (4usize, 2usize)];
        for (relative_offset, length) in posting_ranges {
            let start = postings_start + relative_offset;
            let end = start + length;
            assert!(start >= postings_start);
            assert!(end <= footer_start);
        }
    }

    #[test]
    fn segment_encoding_is_deterministic() {
        let index = test_index();

        let first = build_segment(&index).unwrap();
        let second = build_segment(&index).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn segment_is_published_from_a_temporary_file() {
        let id = TEST_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "inverted-index-segment-{}-{id}.idx",
            std::process::id()
        ));
        let expected = build_segment(&test_index()).unwrap();

        encode(&path, &test_index()).unwrap();

        assert_eq!(fs::read(&path).unwrap(), expected);
        fs::remove_file(path).unwrap();
    }
}
