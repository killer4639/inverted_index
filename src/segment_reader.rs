use crate::index_codec::{
    CHECKSUM_LENGTH, FORMAT_VERSION, HEADER_LENGTH, MAGIC, TERM_OFFSET_LENGTH,
};
use crate::postings_codec::PostingsDecoder;
use crate::varint;
use memmap2::Mmap;
use std::fs::File;
use std::io;
use std::io::ErrorKind;
use std::ops::Range;
use std::path::Path;

pub struct SegmentReader {
    mmap: Mmap,
    document_count: u32,
    term_count: u32,
    term_offset_table: Range<usize>,
    term_records: Range<usize>,
    postings: Range<usize>,
}

#[derive(Debug)]
struct SegmentLayout {
    document_count: u32,
    term_count: u32,
    term_offset_table: Range<usize>,
    term_records: Range<usize>,
    postings: Range<usize>,
}

impl SegmentReader {
    /// Opens a segment that remains immutable for the lifetime of this reader.
    ///
    /// Published segments must not be modified or truncated while mapped.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = File::open(path)?;
        let mmap = map_immutable_segment(&file)?;
        let layout = validate_segment(&mmap)?;

        Ok(Self {
            mmap,
            document_count: layout.document_count,
            term_count: layout.term_count,
            term_offset_table: layout.term_offset_table,
            term_records: layout.term_records,
            postings: layout.postings,
        })
    }

    pub fn document_count(&self) -> u32 {
        self.document_count
    }

    pub fn term_count(&self) -> u32 {
        self.term_count
    }

    pub fn term_offset_table(&self) -> &[u8] {
        &self.mmap[self.term_offset_table.clone()]
    }

    pub fn term_records(&self) -> &[u8] {
        &self.mmap[self.term_records.clone()]
    }

    pub fn postings(&self) -> &[u8] {
        &self.mmap[self.postings.clone()]
    }
}

fn map_immutable_segment(file: &File) -> io::Result<Mmap> {
    // SAFETY: Segment files are immutable after publication. Callers must not
    // modify or truncate the file while the returned reader is alive.
    unsafe { Mmap::map(file) }
}

fn validate_segment(bytes: &[u8]) -> io::Result<SegmentLayout> {
    if bytes.len() < HEADER_LENGTH + CHECKSUM_LENGTH {
        return Err(unexpected_eof("segment too small"));
    }
    validate_checksum(bytes)?;

    let (document_count, term_count, postings_offset) = validate_header(&bytes[..HEADER_LENGTH])?;
    let postings_start = usize_from_u64(
        postings_offset,
        "postings offset exceeds addressable memory",
    )?;
    let checksum_start = bytes.len() - CHECKSUM_LENGTH;
    if postings_start > checksum_start {
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
        if postings_start != HEADER_LENGTH {
            return Err(invalid_data(
                "empty segment must place postings immediately after the header",
            ));
        }
        if checksum_start != HEADER_LENGTH {
            return Err(invalid_data("empty segment contains unexpected payload"));
        }
        return Ok(SegmentLayout {
            document_count,
            term_count,
            term_offset_table: HEADER_LENGTH..HEADER_LENGTH,
            term_records: HEADER_LENGTH..HEADER_LENGTH,
            postings: HEADER_LENGTH..HEADER_LENGTH,
        });
    }

    let term_offsets = read_term_offsets(
        &bytes[HEADER_LENGTH..term_records_start],
        term_count_usize,
        term_records_start,
        postings_start,
    )?;
    let postings = &bytes[postings_start..checksum_start];
    validate_terms_and_postings(
        bytes,
        &term_offsets,
        postings_start,
        document_count,
        postings,
    )?;
    Ok(SegmentLayout {
        document_count,
        term_count,
        term_offset_table: HEADER_LENGTH..term_records_start,
        term_records: term_records_start..postings_start,
        postings: postings_start..checksum_start,
    })
}

fn validate_header(input: &[u8]) -> io::Result<(u32, u32, u64)> {
    if input.len() < HEADER_LENGTH {
        return Err(unexpected_eof("truncated header"));
    }

    if input[..8] != MAGIC[..] {
        return Err(invalid_data("invalid magic"));
    }

    let version = u16::from_le_bytes(input[8..10].try_into().unwrap());
    if version != FORMAT_VERSION {
        return Err(invalid_data("invalid format version"));
    }

    let document_count = u32::from_le_bytes(input[10..14].try_into().unwrap());
    let term_count = u32::from_le_bytes(input[14..18].try_into().unwrap());
    let postings_offset = u64::from_le_bytes(input[18..26].try_into().unwrap());
    Ok((document_count, term_count, postings_offset))
}

fn read_term_offsets(
    table: &[u8],
    term_count: usize,
    term_records_start: usize,
    postings_start: usize,
) -> io::Result<Vec<usize>> {
    let mut offsets = Vec::with_capacity(term_count);
    let mut index = 0;
    while index < table.len() {
        let offset =
            u64::from_le_bytes(table[index..index + TERM_OFFSET_LENGTH].try_into().unwrap());
        let offset = usize_from_u64(offset, "term-record offset exceeds addressable memory")?;
        if offset < term_records_start || offset >= postings_start {
            return Err(invalid_data(
                "term-record offset is outside the term-record section",
            ));
        }
        if let Some(&previous) = offsets.last() {
            if offset <= previous {
                return Err(invalid_data(
                    "term-record offsets are not strictly increasing",
                ));
            }
        }
        offsets.push(offset);
        index += TERM_OFFSET_LENGTH;
    }

    if offsets.len() != term_count {
        return Err(invalid_data("term-offset table does not match term count"));
    }
    if offsets[0] != term_records_start {
        return Err(invalid_data(
            "first term-record offset must start at the term-record section",
        ));
    }

    Ok(offsets)
}

fn validate_terms_and_postings(
    bytes: &[u8],
    term_offsets: &[usize],
    postings_start: usize,
    document_count: u32,
    postings: &[u8],
) -> io::Result<()> {
    let mut previous_term: Option<&str> = None;
    let mut posting_ranges = Vec::with_capacity(term_offsets.len());

    for (index, &term_start) in term_offsets.iter().enumerate() {
        let term_end = if index + 1 < term_offsets.len() {
            term_offsets[index + 1]
        } else {
            postings_start
        };

        let (term, posting_range) =
            validate_term_record(bytes, term_start, term_end, document_count, postings)?;

        if let Some(previous) = previous_term {
            if term <= previous {
                return Err(invalid_data("terms are not strictly increasing"));
            }
        }
        previous_term = Some(term);
        posting_ranges.push(posting_range);
    }

    posting_ranges.sort_by_key(|range| range.0);
    let mut expected = 0;
    for (start, end) in posting_ranges {
        if start != expected {
            return Err(invalid_data("postings ranges are not contiguous"));
        }
        if end <= start {
            return Err(invalid_data("postings range is empty"));
        }
        expected = end;
    }
    if expected != postings.len() {
        return Err(invalid_data("postings section is not fully covered"));
    }

    Ok(())
}

fn validate_term_record<'a>(
    bytes: &'a [u8],
    start: usize,
    end: usize,
    document_count: u32,
    postings: &[u8],
) -> io::Result<(&'a str, (usize, usize))> {
    if start >= end {
        return Err(invalid_data("empty term record"));
    }

    let (term_length, term_length_size) = varint::decode(&bytes[start..end])
        .map_err(|error| invalid_data(format!("invalid term length: {error:?}")))?;
    if term_length == 0 {
        return Err(invalid_data("empty term"));
    }

    let term_bytes_start = start + term_length_size;
    let term_len = usize::try_from(term_length)
        .map_err(|_| invalid_data("term length exceeds addressable memory"))?;
    let term_bytes_end = term_bytes_start
        .checked_add(term_len)
        .ok_or_else(|| invalid_data("term length overflow"))?;
    if term_bytes_end > end {
        return Err(unexpected_eof("truncated term bytes"));
    }

    let term = std::str::from_utf8(&bytes[term_bytes_start..term_bytes_end])
        .map_err(|_| invalid_data("term is not valid UTF-8"))?;
    for character in term.chars() {
        if !character.is_ascii_alphanumeric() {
            return Err(invalid_data(format!(
                "term '{term}' is not ASCII alphanumeric"
            )));
        }
    }

    let (document_frequency, frequency_size) = varint::decode(&bytes[term_bytes_end..end])
        .map_err(|error| {
            invalid_data(format!(
                "invalid document frequency for term '{term}': {error:?}"
            ))
        })?;
    if document_frequency == 0 {
        return Err(invalid_data(format!("term '{term}' has no postings")));
    }

    let postings_meta_start = term_bytes_end + frequency_size;
    let postings_meta_end = postings_meta_start
        .checked_add(16)
        .ok_or_else(|| unexpected_eof("truncated postings metadata"))?;
    if postings_meta_end > end {
        return Err(unexpected_eof(format!(
            "truncated postings metadata for term '{term}'"
        )));
    }
    if postings_meta_end != end {
        return Err(invalid_data(format!(
            "term '{term}' record does not fill its allocated range"
        )));
    }

    let relative_offset = u64::from_le_bytes(
        bytes[postings_meta_start..postings_meta_start + 8]
            .try_into()
            .unwrap(),
    );
    let postings_length = u64::from_le_bytes(
        bytes[postings_meta_start + 8..postings_meta_end]
            .try_into()
            .unwrap(),
    );
    if postings_length == 0 {
        return Err(invalid_data(format!("term '{term}' has empty postings")));
    }

    let range_start = usize_from_u64(
        relative_offset,
        "postings offset exceeds addressable memory",
    )?;
    let range_length = usize_from_u64(
        postings_length,
        "postings length exceeds addressable memory",
    )?;
    let range_end = range_start
        .checked_add(range_length)
        .ok_or_else(|| invalid_data(format!("postings range overflow for term '{term}'")))?;
    if range_end > postings.len() {
        return Err(invalid_data(format!(
            "postings range for term '{term}' is outside the postings section"
        )));
    }

    let posting_count = usize::try_from(document_frequency).map_err(|_| {
        invalid_data(format!(
            "document frequency for term '{term}' exceeds addressable memory"
        ))
    })?;
    let decoder = PostingsDecoder::new(&postings[range_start..range_end], posting_count);
    for result in decoder {
        let posting = match result {
            Ok(posting) => posting,
            Err(error) => {
                return Err(invalid_data(format!(
                    "invalid postings for term '{term}': {error:?}"
                )));
            }
        };
        if posting.document_id >= document_count {
            return Err(invalid_data(format!(
                "term '{term}' contains unknown document ID {}",
                posting.document_id
            )));
        }
    }

    Ok((term, (range_start, range_end)))
}

fn validate_checksum(bytes: &[u8]) -> io::Result<()> {
    let checksum_start = bytes.len() - CHECKSUM_LENGTH;
    let stored = u32::from_le_bytes(bytes[checksum_start..].try_into().unwrap());
    let computed = crc32fast::hash(&bytes[..checksum_start]);
    if stored != computed {
        return Err(invalid_data("checksum mismatch"));
    }
    Ok(())
}

fn usize_from_u64(value: u64, message: &str) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| invalid_data(message))
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message.into())
}

fn unexpected_eof(message: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::UnexpectedEof, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_codec;
    use crate::inverted_index::{InvertedIndex, Posting};
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

    fn write_segment(index: &InvertedIndex) -> std::path::PathBuf {
        let id = TEST_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("segment-reader-{}-{id}.idx", std::process::id()));
        index_codec::encode(&path, index).unwrap();
        path
    }

    fn replace_checksum(bytes: &mut Vec<u8>) {
        bytes.truncate(bytes.len() - CHECKSUM_LENGTH);
        let checksum = crc32fast::hash(bytes);
        bytes.extend_from_slice(&checksum.to_le_bytes());
    }

    #[test]
    fn valid_segment_exposes_metadata_and_regions() {
        let path = write_segment(&test_index());
        let reader = SegmentReader::open(&path).unwrap();

        assert_eq!(reader.document_count(), 4);
        assert_eq!(reader.term_count(), 2);
        assert_eq!(reader.term_offset_table().len(), 2 * TERM_OFFSET_LENGTH);
        assert!(!reader.term_records().is_empty());
        assert_eq!(reader.postings(), &[0x00, 0x02, 0x03, 0x01, 0x01, 0x01]);

        drop(reader);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn empty_segment_is_accepted() {
        let index = InvertedIndex::from_finalized_postings(BTreeMap::new(), 0);
        let path = write_segment(&index);
        let reader = SegmentReader::open(&path).unwrap();

        assert_eq!(reader.document_count(), 0);
        assert_eq!(reader.term_count(), 0);
        assert!(reader.term_offset_table().is_empty());
        assert!(reader.term_records().is_empty());
        assert!(reader.postings().is_empty());

        drop(reader);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn truncated_segment_is_rejected() {
        let error = validate_segment(&[0; 10]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    }

    #[test]
    fn invalid_magic_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] = b'X';
        replace_checksum(&mut bytes);

        let error = validate_segment(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "invalid magic");
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[8..10].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
        replace_checksum(&mut bytes);

        let error = validate_segment(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "invalid format version");
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;

        let error = validate_segment(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.to_string(), "checksum mismatch");
    }

    #[test]
    fn impossible_term_offset_table_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[14..18].copy_from_slice(&u32::MAX.to_le_bytes());
        replace_checksum(&mut bytes);

        let error = validate_segment(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "term records overlap the postings section"
        );
    }

    #[test]
    fn invalid_term_record_offset_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[26..34].copy_from_slice(&(HEADER_LENGTH as u64).to_le_bytes());
        replace_checksum(&mut bytes);

        let error = validate_segment(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "term-record offset is outside the term-record section"
        );
    }

    #[test]
    fn truncated_term_record_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[42] = 127;
        replace_checksum(&mut bytes);

        let error = validate_segment(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
        assert_eq!(error.to_string(), "truncated term bytes");
    }

    #[test]
    fn invalid_utf8_term_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[43] = 0xFF;
        replace_checksum(&mut bytes);

        let error = validate_segment(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "term is not valid UTF-8");
    }

    #[test]
    fn zero_document_frequency_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[47] = 0;
        replace_checksum(&mut bytes);

        let error = validate_segment(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "term 'rust' has no postings");
    }

    #[test]
    fn posting_range_outside_postings_section_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[56..64].copy_from_slice(&u64::MAX.to_le_bytes());
        replace_checksum(&mut bytes);

        let error = validate_segment(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(error.to_string().contains("postings"));
    }

    #[test]
    fn unsorted_terms_are_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[43..47].copy_from_slice(b"zzzz");
        replace_checksum(&mut bytes);

        let error = validate_segment(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "terms are not strictly increasing");
    }

    #[test]
    fn duplicate_terms_are_rejected() {
        let mut postings = BTreeMap::new();
        postings.insert(
            "alpha".to_owned(),
            vec![Posting {
                document_id: 0,
                term_frequency: 1,
            }],
        );
        postings.insert(
            "bravo".to_owned(),
            vec![Posting {
                document_id: 1,
                term_frequency: 1,
            }],
        );
        let index = InvertedIndex::from_finalized_postings(postings, 2);
        let path = write_segment(&index);
        let mut bytes = fs::read(&path).unwrap();
        let second_term_offset = u64::from_le_bytes(bytes[34..42].try_into().unwrap()) as usize;
        bytes[second_term_offset + 1..second_term_offset + 6].copy_from_slice(b"alpha");
        replace_checksum(&mut bytes);

        let error = validate_segment(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "terms are not strictly increasing");
    }

    #[test]
    fn noncanonical_term_length_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[42] = 0x80;
        bytes[43] = 0;
        replace_checksum(&mut bytes);

        let error = validate_segment(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(error.to_string().contains("NonCanonical"));
    }

    #[test]
    fn unknown_document_id_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[10..14].copy_from_slice(&1u32.to_le_bytes());
        replace_checksum(&mut bytes);

        let error = validate_segment(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(error.to_string().contains("unknown document ID"));
    }

    #[test]
    fn truncated_segments_are_rejected_without_panicking() {
        let path = write_segment(&test_index());
        let bytes = fs::read(&path).unwrap();

        for length in 0..bytes.len() {
            let result = std::panic::catch_unwind(|| validate_segment(&bytes[..length]));
            assert!(result.is_ok(), "validation panicked at length {length}");
            assert!(
                result.unwrap().is_err(),
                "truncated segment was accepted at length {length}"
            );
        }

        fs::remove_file(&path).unwrap();
    }
}
