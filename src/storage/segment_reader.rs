use crate::storage::binary_file::{Decoder, invalid_data, invalid_input, unexpected_eof};
use crate::storage::postings::PostingsDecoder;
use crate::storage::segment_codec::{self, TERM_OFFSET_LENGTH};
#[cfg(test)]
use crate::storage::segment_codec::{HEADER_LENGTH, SegmentLayout};
use memmap2::Mmap;
use std::fs::File;
use std::io;
use std::ops::Range;
use std::path::Path;

pub struct SegmentReader {
    mmap: Mmap,
    checksum: u32,
    document_count: u32,
    term_count: u32,
    term_offset_table: Range<usize>,
    term_records: Range<usize>,
    postings: Range<usize>,
}

impl SegmentReader {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let mmap = map_immutable_segment(&file)?;
        let decoded = segment_codec::decode_bytes(path, &mmap)?;
        let layout = decoded.value;

        Ok(Self {
            mmap,
            checksum: decoded.checksum,
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

    pub fn checksum(&self) -> u32 {
        self.checksum
    }

    pub fn term_offset_table(&self) -> &[u8] {
        &self.mmap[self.term_offset_table.clone()]
    }

    #[cfg(test)]
    fn term_records(&self) -> &[u8] {
        &self.mmap[self.term_records.clone()]
    }

    pub fn postings(&self) -> &[u8] {
        &self.mmap[self.postings.clone()]
    }

    pub fn get_term(&self, index: u32) -> io::Result<&str> {
        let range = self.term_record_range(index)?;
        let record = parse_term_record(&self.mmap, range.start, range.end)?;
        Ok(record.term)
    }

    pub fn get_postings(&self, index: u32) -> io::Result<PostingsDecoder<'_>> {
        let range = self.term_record_range(index)?;
        let record = parse_term_record(&self.mmap, range.start, range.end)?;
        let postings = self.postings();
        if record.postings_end > postings.len() {
            return Err(invalid_data(
                "postings range is outside the postings section",
            ));
        }

        let posting_count = usize::try_from(record.document_frequency)
            .map_err(|_| invalid_data("document frequency exceeds addressable memory"))?;
        Ok(PostingsDecoder::new(
            &postings[record.postings_start..record.postings_end],
            posting_count,
        ))
    }

    fn term_record_range(&self, index: u32) -> io::Result<Range<usize>> {
        if index >= self.term_count {
            return Err(invalid_input("term index out of range"));
        }

        let table = self.term_offset_table();
        let table_index = usize::try_from(index)
            .map_err(|_| invalid_data("term index exceeds addressable memory"))?;
        let offset_start = table_index
            .checked_mul(TERM_OFFSET_LENGTH)
            .ok_or_else(|| invalid_data("term-offset table index overflow"))?;
        let offset_end = offset_start
            .checked_add(TERM_OFFSET_LENGTH)
            .ok_or_else(|| invalid_data("term-offset table index overflow"))?;
        if offset_end > table.len() {
            return Err(unexpected_eof("truncated term-offset table"));
        }

        let mut offset_decoder = Decoder::new(&table[offset_start..offset_end]);
        let term_start = offset_decoder.read_u64("term-record offset")?;
        let term_start =
            usize_from_u64(term_start, "term-record offset exceeds addressable memory")?;

        let term_end = if index + 1 < self.term_count {
            let next_end = offset_end
                .checked_add(TERM_OFFSET_LENGTH)
                .ok_or_else(|| invalid_data("term-offset table index overflow"))?;
            if next_end > table.len() {
                return Err(unexpected_eof("truncated term-offset table"));
            }
            let mut next_offset_decoder = Decoder::new(&table[offset_end..next_end]);
            let next_start = next_offset_decoder.read_u64("term-record offset")?;
            usize_from_u64(next_start, "term-record offset exceeds addressable memory")?
        } else {
            self.postings.start
        };

        if term_start < self.term_records.start
            || term_end > self.term_records.end
            || term_start >= term_end
        {
            return Err(invalid_data("term-record range is invalid"));
        }

        Ok(term_start..term_end)
    }
}

fn map_immutable_segment(file: &File) -> io::Result<Mmap> {
    // SAFETY: Segment files are immutable after publication. Callers must not
    // modify or truncate the file while the returned reader is alive.
    unsafe { Mmap::map(file) }
}

struct ParsedTermRecord<'a> {
    term: &'a str,
    document_frequency: u32,
    postings_start: usize,
    postings_end: usize,
}

fn parse_term_record(bytes: &[u8], start: usize, end: usize) -> io::Result<ParsedTermRecord<'_>> {
    if start >= end {
        return Err(invalid_data("empty term record"));
    }

    let mut decoder = Decoder::new(&bytes[start..end]);
    let term_length = decoder.read_varint("term length")?;
    if term_length == 0 {
        return Err(invalid_data("empty term"));
    }

    let term_len = usize::try_from(term_length)
        .map_err(|_| invalid_data("term length exceeds addressable memory"))?;
    let term_bytes = decoder.read_exact(term_len, "term bytes")?;
    let term =
        std::str::from_utf8(term_bytes).map_err(|_| invalid_data("term is not valid UTF-8"))?;
    for character in term.chars() {
        if !character.is_ascii_alphanumeric() {
            return Err(invalid_data(format!(
                "term '{term}' is not ASCII alphanumeric"
            )));
        }
    }

    let document_frequency =
        decoder.read_varint(&format!("document frequency for term '{term}'"))?;
    if document_frequency == 0 {
        return Err(invalid_data(format!("term '{term}' has no postings")));
    }

    let relative_offset = decoder.read_u64(&format!("postings offset for term '{term}'"))?;
    let postings_length = decoder.read_u64(&format!("postings length for term '{term}'"))?;
    if !decoder.is_finished() {
        return Err(invalid_data(format!(
            "term '{term}' record does not fill its allocated range"
        )));
    }

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

    Ok(ParsedTermRecord {
        term,
        document_frequency,
        postings_start: range_start,
        postings_end: range_end,
    })
}

fn usize_from_u64(value: u64, message: &str) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| invalid_data(message))
}

#[cfg(test)]
fn read_segment_layout(bytes: &[u8]) -> io::Result<SegmentLayout> {
    segment_codec::decode_bytes(Path::new("segment.idx"), bytes).map(|decoded| decoded.value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InvertedIndex, Posting};
    use crate::storage::binary_file::{CHECKSUM_LENGTH, append_checksum};
    use crate::storage::segment_codec::FORMAT_VERSION;
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::ErrorKind;
    use std::path::PathBuf;
    use std::sync::OnceLock;
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
        segment_codec::encode(&path, index).unwrap();
        path
    }

    fn replace_checksum(bytes: &mut Vec<u8>) {
        bytes.truncate(bytes.len() - CHECKSUM_LENGTH);
        append_checksum(bytes);
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
    fn segment_too_small_for_header_and_checksum_is_rejected() {
        let error = read_segment_layout(&[0; 10]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[8..10].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
        replace_checksum(&mut bytes);

        let error = read_segment_layout(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "unsupported segment format version");
    }

    #[test]
    fn invalid_magic_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] ^= 0xFF;
        replace_checksum(&mut bytes);

        let error = read_segment_layout(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "invalid segment magic");
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;

        let error = read_segment_layout(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.to_string(), "segment checksum mismatch");
    }

    #[test]
    fn term_offset_table_outside_the_file_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[14..18].copy_from_slice(&u32::MAX.to_le_bytes());
        replace_checksum(&mut bytes);

        let error = read_segment_layout(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "term records overlap the postings section"
        );
    }

    #[test]
    fn postings_offset_past_the_checksum_is_rejected() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[18..26].copy_from_slice(&u64::MAX.to_le_bytes());
        replace_checksum(&mut bytes);

        let error = read_segment_layout(&bytes).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(
            error.to_string() == "postings offset exceeds addressable memory"
                || error.to_string() == "postings offset is past the checksum"
        );
    }

    #[test]
    fn invalid_term_record_offset_is_rejected_when_accessed() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[26..34].copy_from_slice(&(HEADER_LENGTH as u64).to_le_bytes());
        replace_checksum(&mut bytes);
        fs::write(&path, bytes).unwrap();

        let reader = SegmentReader::open(&path).unwrap();
        let error = reader.get_term(0).unwrap_err();
        drop(reader);
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "term-record range is invalid");
    }

    #[test]
    fn truncated_term_record_is_rejected_when_accessed() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[42] = 127;
        replace_checksum(&mut bytes);
        fs::write(&path, bytes).unwrap();

        let reader = SegmentReader::open(&path).unwrap();
        let error = reader.get_term(0).unwrap_err();
        drop(reader);
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
        assert_eq!(error.to_string(), "truncated term bytes");
    }

    #[test]
    fn invalid_utf8_term_is_rejected_when_accessed() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[43] = 0xFF;
        replace_checksum(&mut bytes);
        fs::write(&path, bytes).unwrap();

        let reader = SegmentReader::open(&path).unwrap();
        let error = reader.get_term(0).unwrap_err();
        drop(reader);
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "term is not valid UTF-8");
    }

    #[test]
    fn zero_document_frequency_is_rejected_when_accessed() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[47] = 0;
        replace_checksum(&mut bytes);
        fs::write(&path, bytes).unwrap();

        let reader = SegmentReader::open(&path).unwrap();
        let error = match reader.get_postings(0) {
            Ok(_) => panic!("zero document frequency should fail"),
            Err(error) => error,
        };
        drop(reader);
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "term 'rust' has no postings");
    }

    #[test]
    fn posting_range_outside_postings_section_is_rejected_when_accessed() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[56..64].copy_from_slice(&u64::MAX.to_le_bytes());
        replace_checksum(&mut bytes);
        fs::write(&path, bytes).unwrap();

        let reader = SegmentReader::open(&path).unwrap();
        let error = match reader.get_postings(0) {
            Ok(_) => panic!("invalid postings range should fail"),
            Err(error) => error,
        };
        drop(reader);
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(error.to_string().contains("postings"));
    }

    #[test]
    fn term_order_is_not_eagerly_revalidated() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[43..47].copy_from_slice(b"zzzz");
        replace_checksum(&mut bytes);
        fs::write(&path, bytes).unwrap();

        let reader = SegmentReader::open(&path).unwrap();
        assert_eq!(reader.get_term(0).unwrap(), "zzzz");
        drop(reader);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn noncanonical_term_length_is_rejected_when_accessed() {
        let path = write_segment(&test_index());
        let mut bytes = fs::read(&path).unwrap();
        bytes[42] = 0x80;
        bytes[43] = 0;
        replace_checksum(&mut bytes);
        fs::write(&path, bytes).unwrap();

        let reader = SegmentReader::open(&path).unwrap();
        let error = reader.get_term(0).unwrap_err();
        drop(reader);
        fs::remove_file(&path).unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(error.to_string().contains("noncanonical"));
    }

    #[test]
    fn truncated_segments_fail_checksum_validation_without_panicking() {
        let path = write_segment(&test_index());
        let bytes = fs::read(&path).unwrap();

        for length in 0..bytes.len() {
            let result = std::panic::catch_unwind(|| read_segment_layout(&bytes[..length]));
            assert!(result.is_ok(), "validation panicked at length {length}");
            assert!(
                result.unwrap().is_err(),
                "truncated segment was accepted at length {length}"
            );
        }

        fs::remove_file(&path).unwrap();
    }

    static TEST_SEGMENT: OnceLock<()> = OnceLock::new();

    fn test_segment_path() -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("reader-test-segment-{}.idx", std::process::id()));
        TEST_SEGMENT.get_or_init(|| {
            if !path.exists() {
                segment_codec::encode(&path, &test_index()).expect("write data/test_segment.idx");
            }
        });
        path
    }

    fn collect_postings(decoder: PostingsDecoder<'_>) -> Vec<Posting> {
        let mut postings = Vec::new();
        for result in decoder {
            postings.push(result.expect("postings should decode"));
        }
        postings
    }

    fn posting(document_id: u32, term_frequency: u32) -> Posting {
        Posting {
            document_id,
            term_frequency,
        }
    }

    #[test]
    fn get_term_returns_sorted_dictionary_terms() {
        let reader = SegmentReader::open(test_segment_path()).unwrap();

        assert_eq!(reader.get_term(0).unwrap(), "rust");
        assert_eq!(reader.get_term(1).unwrap(), "search");
    }

    #[test]
    fn get_term_rejects_out_of_range_index() {
        let reader = SegmentReader::open(test_segment_path()).unwrap();

        let error = reader.get_term(2).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(error.to_string(), "term index out of range");
    }

    #[test]
    fn get_postings_returns_decoded_lists() {
        let reader = SegmentReader::open(test_segment_path()).unwrap();

        let rust_postings = collect_postings(reader.get_postings(0).unwrap());
        let search_postings = collect_postings(reader.get_postings(1).unwrap());

        assert_eq!(rust_postings, vec![posting(0, 2), posting(3, 1)]);
        assert_eq!(search_postings, vec![posting(1, 1)]);
    }

    #[test]
    fn get_postings_rejects_out_of_range_index() {
        let reader = SegmentReader::open(test_segment_path()).unwrap();

        let error = match reader.get_postings(2) {
            Ok(_) => panic!("out-of-range postings lookup should fail"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(error.to_string(), "term index out of range");
    }

    #[test]
    fn empty_segment_rejects_term_lookup() {
        let index = InvertedIndex::from_finalized_postings(BTreeMap::new(), 0);
        let path = write_segment(&index);
        let reader = SegmentReader::open(&path).unwrap();

        let error = reader.get_term(0).unwrap_err();
        drop(reader);
        fs::remove_file(&path).unwrap();

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(error.to_string(), "term index out of range");
    }
}
