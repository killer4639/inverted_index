use crate::inverted_index::InvertedIndex;
use crate::{postings_codec, varint};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const MAGIC: &[u8; 8] = b"INVIDX\0\0";
pub const FORMAT_VERSION: u16 = 1;
pub const HEADER_LENGTH: usize = 26;
pub const TERM_OFFSET_LENGTH: usize = 8;
pub const CHECKSUM_LENGTH: usize = 4;

static TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

struct SegmentSections {
    term_record_offsets: Vec<u64>,
    term_records: Vec<u8>,
    postings: Vec<u8>,
}

struct TemporaryFileCleanup {
    path: PathBuf,
}

impl TemporaryFileCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TemporaryFileCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn encode(path: impl AsRef<Path>, index: &InvertedIndex) -> io::Result<()> {
    let path = path.as_ref();
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "segment path already exists",
        ));
    }

    let segment = build_segment(index)?;
    let (temporary_path, temporary_file) = create_temporary_file(path)?;
    let _cleanup = TemporaryFileCleanup::new(temporary_path.clone());

    write_temporary_file(temporary_file, &segment)?;
    fs::rename(&temporary_path, path)?;
    Ok(())
}

fn build_segment(index: &InvertedIndex) -> io::Result<Vec<u8>> {
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
    let postings_offset_u64 =
        u64::try_from(postings_offset).map_err(|_| invalid_input("postings offset exceeds u64"))?;

    let segment_length = postings_offset
        .checked_add(sections.postings.len())
        .and_then(|length| length.checked_add(CHECKSUM_LENGTH))
        .ok_or_else(|| invalid_input("segment length overflow"))?;
    let mut segment = Vec::with_capacity(segment_length);

    write_header(
        &mut segment,
        index.document_count(),
        term_count,
        postings_offset_u64,
    );

    let term_records_start_u64 = u64::try_from(term_records_start)
        .map_err(|_| invalid_input("term-record offset exceeds u64"))?;
    for relative_offset in sections.term_record_offsets {
        let absolute_offset = term_records_start_u64
            .checked_add(relative_offset)
            .ok_or_else(|| invalid_input("absolute term-record offset overflow"))?;
        segment.extend_from_slice(&absolute_offset.to_le_bytes());
    }

    segment.extend_from_slice(&sections.term_records);
    segment.extend_from_slice(&sections.postings);

    let checksum = crc32fast::hash(&segment);
    segment.extend_from_slice(&checksum.to_le_bytes());
    Ok(segment)
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
        postings_codec::encode(term_postings, &mut postings).map_err(|error| {
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

fn write_header(buffer: &mut Vec<u8>, document_count: u32, term_count: u32, postings_offset: u64) {
    buffer.extend_from_slice(MAGIC);
    buffer.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    buffer.extend_from_slice(&document_count.to_le_bytes());
    buffer.extend_from_slice(&term_count.to_le_bytes());
    buffer.extend_from_slice(&postings_offset.to_le_bytes());
}

fn create_temporary_file(path: &Path) -> io::Result<(PathBuf, File)> {
    let file_name = path
        .file_name()
        .ok_or_else(|| invalid_input("segment path must include a file name"))?;
    let parent = path.parent().unwrap_or_else(|| Path::new(""));

    for _ in 0..100 {
        let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let mut temporary_name = OsString::from(file_name);
        temporary_name.push(format!(".tmp-{}-{id}", std::process::id()));
        let temporary_path = parent.join(temporary_name);

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique temporary segment file",
    ))
}

fn write_temporary_file(file: File, segment: &[u8]) -> io::Result<()> {
    let mut writer = BufWriter::new(file);
    writer.write_all(segment)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);
    Ok(())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverted_index::Posting;
    use std::collections::BTreeMap;

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
        let mut bytes = Vec::new();

        write_header(&mut bytes, 3, 5, 42);

        assert_eq!(bytes.len(), HEADER_LENGTH);
        assert_eq!(&bytes[0..8], MAGIC);
        assert_eq!(&bytes[8..10], &FORMAT_VERSION.to_le_bytes());
        assert_eq!(&bytes[10..14], &3u32.to_le_bytes());
        assert_eq!(&bytes[14..18], &5u32.to_le_bytes());
        assert_eq!(&bytes[18..26], &42u64.to_le_bytes());
    }

    #[test]
    fn segment_contains_term_records_postings_and_footer() {
        let segment = build_segment(&test_index()).unwrap();

        assert_eq!(u64::from_le_bytes(segment[18..26].try_into().unwrap()), 88);
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

        let checksum_offset = segment.len() - CHECKSUM_LENGTH;
        let stored_checksum = u32::from_le_bytes(segment[checksum_offset..].try_into().unwrap());
        assert_eq!(
            stored_checksum,
            crc32fast::hash(&segment[..checksum_offset])
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
        let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "inverted-index-segment-{}-{id}.idx",
            std::process::id()
        ));
        let expected = build_segment(&test_index()).unwrap();

        encode(&path, &test_index()).unwrap();

        assert_eq!(fs::read(&path).unwrap(), expected);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn existing_segment_is_not_replaced() {
        let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "existing-inverted-index-segment-{}-{id}.idx",
            std::process::id()
        ));
        fs::write(&path, b"existing").unwrap();

        let error = encode(&path, &test_index()).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"existing");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn temporary_file_cleanup_removes_unpublished_file() {
        let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "inverted-index-cleanup-{}-{id}.tmp",
            std::process::id()
        ));
        fs::write(&path, b"partial").unwrap();

        let cleanup = TemporaryFileCleanup::new(path.clone());
        drop(cleanup);

        assert!(!path.exists());
    }
}
