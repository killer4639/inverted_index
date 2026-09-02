use crate::SegmentId;
use crate::storage::binary_file::{
    self, BinaryFileCodec, CHECKSUM_LENGTH, Decoder, invalid_data, invalid_input,
};
use crate::storage::manifest::{
    FILE_NUMBER_WIDTH, MANIFEST_FILE_PREFIX, MANIFEST_FILE_SUFFIX, Manifest, SegmentMetadata,
};
use crate::storage::varint;
use std::io;
use std::path::Path;

pub const MAGIC: &[u8; 8] = b"INVMAN\0\0";
pub const FORMAT_VERSION: u16 = 1;
pub const HEADER_LENGTH: usize = 22;

const SEGMENT_RECORD_FIXED_LENGTH: usize = 28;
const SEGMENT_RECORD_MIN_LENGTH: usize = SEGMENT_RECORD_FIXED_LENGTH + 1;

struct ManifestCodec;

impl BinaryFileCodec for ManifestCodec {
    type Input = Manifest;
    type Decoded = Manifest;

    const FILE_TYPE: &'static str = "manifest";
    const MAGIC: &'static [u8; 8] = MAGIC;
    const FORMAT_VERSION: u16 = FORMAT_VERSION;

    fn encode_body(path: &Path, manifest: &Self::Input, output: &mut Vec<u8>) -> io::Result<()> {
        let generation = generation_from_path(path)?;
        manifest
            .validate()
            .map_err(|error| invalid_input(format!("invalid manifest: {error:?}")))?;

        let segment_count = u32::try_from(manifest.segment_count())
            .map_err(|_| invalid_input("segment count exceeds the manifest format limit"))?;
        let encoded_length = encoded_length(manifest)?;
        let additional_capacity = encoded_length
            .checked_sub(output.len())
            .ok_or_else(|| invalid_input("manifest length is smaller than its header"))?;
        output.reserve(additional_capacity);

        output.extend_from_slice(&generation.to_le_bytes());
        output.extend_from_slice(&segment_count.to_le_bytes());

        for segment in manifest.segments() {
            encode_segment_record(output, segment)?;
        }

        assert_eq!(output.len() + CHECKSUM_LENGTH, encoded_length);
        Ok(())
    }

    fn decode_body(path: &Path, decoder: &mut Decoder<'_>) -> io::Result<Self::Decoded> {
        let expected_generation = generation_from_path(path)?;
        let generation = decoder.read_u64("manifest generation")?;
        if generation != expected_generation {
            return Err(invalid_data("manifest generation does not match file name"));
        }

        let segment_count = decoder.read_u32("segment count")?;
        let segment_count = usize::try_from(segment_count)
            .map_err(|_| invalid_data("segment count exceeds addressable memory"))?;
        validate_segment_count(segment_count, decoder.remaining())?;

        let mut segments = Vec::with_capacity(segment_count);
        let mut total_document_count = 0_u64;
        let mut total_segment_bytes = 0_u64;
        let mut total_term_entries = 0_u64;

        for _ in 0..segment_count {
            let segment = decode_segment_record(decoder)?;
            total_document_count = total_document_count
                .checked_add(u64::from(segment.document_count))
                .ok_or_else(|| invalid_data("total document count overflow"))?;
            total_segment_bytes = total_segment_bytes
                .checked_add(segment.length_bytes)
                .ok_or_else(|| invalid_data("total segment bytes overflow"))?;
            total_term_entries = total_term_entries
                .checked_add(u64::from(segment.term_count))
                .ok_or_else(|| invalid_data("total term entries overflow"))?;
            segments.push(segment);
        }

        if !decoder.is_finished() {
            return Err(invalid_data("trailing manifest bytes"));
        }

        Ok(Manifest::from_decoded_segments_unchecked(segments))
    }
}

pub fn encode(path: impl AsRef<Path>, manifest: &Manifest) -> io::Result<()> {
    binary_file::encode::<ManifestCodec>(path, manifest)
}

pub fn decode(path: impl AsRef<Path>) -> io::Result<Manifest> {
    binary_file::decode::<ManifestCodec>(path).map(|decoded| decoded.value)
}

fn encoded_length(manifest: &Manifest) -> io::Result<usize> {
    let mut length = HEADER_LENGTH
        .checked_add(CHECKSUM_LENGTH)
        .ok_or_else(|| invalid_input("manifest length overflow"))?;

    for segment in manifest.segments() {
        let file_name_length = u32::try_from(segment.file_name.len())
            .map_err(|_| invalid_input("segment file name is too long"))?;
        let varint_length = varint::encoded_length(file_name_length);
        length = length
            .checked_add(SEGMENT_RECORD_FIXED_LENGTH)
            .and_then(|value| value.checked_add(varint_length))
            .and_then(|value| value.checked_add(segment.file_name.len()))
            .ok_or_else(|| invalid_input("manifest length overflow"))?;
    }

    Ok(length)
}

fn encode_segment_record(output: &mut Vec<u8>, segment: &SegmentMetadata) -> io::Result<()> {
    let file_name_length = u32::try_from(segment.file_name.len())
        .map_err(|_| invalid_input("segment file name is too long"))?;

    output.extend_from_slice(&segment.id.value().to_le_bytes());
    varint::encode(file_name_length, output);
    output.extend_from_slice(segment.file_name.as_bytes());
    output.extend_from_slice(&segment.document_count.to_le_bytes());
    output.extend_from_slice(&segment.term_count.to_le_bytes());
    output.extend_from_slice(&segment.length_bytes.to_le_bytes());
    output.extend_from_slice(&segment.checksum.to_le_bytes());
    Ok(())
}

fn validate_segment_count(segment_count: usize, remaining_bytes: usize) -> io::Result<()> {
    let maximum_segment_count = remaining_bytes / SEGMENT_RECORD_MIN_LENGTH;
    if segment_count > maximum_segment_count {
        return Err(invalid_data("segment count exceeds manifest length"));
    }
    Ok(())
}

fn decode_segment_record(decoder: &mut Decoder<'_>) -> io::Result<SegmentMetadata> {
    let segment_id = decoder.read_u64("segment ID")?;
    let segment_id =
        SegmentId::new(segment_id).map_err(|_| invalid_data("segment ID must be nonzero"))?;

    let file_name_length = decoder.read_varint("segment file-name length")?;
    let file_name_length = usize::try_from(file_name_length)
        .map_err(|_| invalid_data("segment file-name length exceeds addressable memory"))?;
    let file_name_bytes = decoder.read_exact(file_name_length, "segment file name")?;
    let file_name = std::str::from_utf8(file_name_bytes)
        .map_err(|_| invalid_data("segment file name is not valid UTF-8"))?
        .to_owned();

    Ok(SegmentMetadata {
        id: segment_id,
        file_name,
        document_count: decoder.read_u32("segment document count")?,
        term_count: decoder.read_u32("segment term count")?,
        length_bytes: decoder.read_u64("segment length")?,
        checksum: decoder.read_u32("segment checksum")?,
    })
}

fn generation_from_path(path: &Path) -> io::Result<u64> {
    let file_name = path
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .ok_or_else(|| invalid_input("manifest path must have a UTF-8 file name"))?;
    let number_text = file_name
        .strip_prefix(MANIFEST_FILE_PREFIX)
        .and_then(|name| name.strip_suffix(MANIFEST_FILE_SUFFIX))
        .ok_or_else(|| invalid_input("invalid manifest file name"))?;
    if number_text.len() != FILE_NUMBER_WIDTH
        || !number_text
            .bytes()
            .all(|character| character.is_ascii_digit())
    {
        return Err(invalid_input("invalid manifest generation in file name"));
    }

    let generation = number_text
        .parse::<u64>()
        .map_err(|_| invalid_input("manifest generation overflow"))?;
    if generation == 0 {
        return Err(invalid_input(
            "manifest generation must be greater than zero",
        ));
    }
    Ok(generation)
}

#[cfg(test)]
fn build_manifest(generation: u64, manifest: &Manifest) -> io::Result<Vec<u8>> {
    let path = manifest_file_path(generation);
    binary_file::encode_bytes::<ManifestCodec>(&path, manifest)
}

#[cfg(test)]
fn decode_bytes(bytes: &[u8], expected_generation: u64) -> io::Result<Manifest> {
    let path = manifest_file_path(expected_generation);
    binary_file::decode_bytes::<ManifestCodec>(&path, bytes).map(|decoded| decoded.value)
}

#[cfg(test)]
fn manifest_file_path(generation: u64) -> std::path::PathBuf {
    format!(
        "{MANIFEST_FILE_PREFIX}{generation:0width$}{MANIFEST_FILE_SUFFIX}",
        width = FILE_NUMBER_WIDTH
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::binary_file::append_checksum;
    use crate::storage::manifest::{SEGMENT_FILE_PREFIX, SEGMENT_FILE_SUFFIX};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn header_and_segment_record_match_the_format() {
        let manifest = populated_manifest();
        let bytes = build_manifest(42, &manifest).unwrap();

        assert_eq!(&bytes[0..8], MAGIC);
        assert_eq!(&bytes[8..10], &FORMAT_VERSION.to_le_bytes());
        assert_eq!(&bytes[10..18], &42_u64.to_le_bytes());
        assert_eq!(&bytes[18..22], &2_u32.to_le_bytes());

        let first = &manifest.segments()[0];
        let mut position = HEADER_LENGTH;
        assert_eq!(
            u64::from_le_bytes(bytes[position..position + 8].try_into().unwrap()),
            first.id.value()
        );
        position += 8;

        let (file_name_length, varint_length) = varint::decode(&bytes[position..]).unwrap();
        position += varint_length;
        assert_eq!(file_name_length as usize, first.file_name.len());
        assert_eq!(
            &bytes[position..position + first.file_name.len()],
            first.file_name.as_bytes()
        );
        position += first.file_name.len();

        assert_eq!(
            u32::from_le_bytes(bytes[position..position + 4].try_into().unwrap()),
            first.document_count
        );
        position += 4;
        assert_eq!(
            u32::from_le_bytes(bytes[position..position + 4].try_into().unwrap()),
            first.term_count
        );
        position += 4;
        assert_eq!(
            u64::from_le_bytes(bytes[position..position + 8].try_into().unwrap()),
            first.length_bytes
        );
        position += 8;
        assert_eq!(
            u32::from_le_bytes(bytes[position..position + 4].try_into().unwrap()),
            first.checksum
        );

        let checksum_start = bytes.len() - CHECKSUM_LENGTH;
        let stored = u32::from_le_bytes(bytes[checksum_start..].try_into().unwrap());
        assert_eq!(stored, crc32fast::hash(&bytes[..checksum_start]));
    }

    #[test]
    fn encoding_is_deterministic_and_round_trips() {
        let manifest = populated_manifest();
        let first = build_manifest(7, &manifest).unwrap();
        let second = build_manifest(7, &manifest).unwrap();
        assert_eq!(first, second);

        let directory = test_directory();
        let path = manifest_path(&directory, 7);
        encode(&path, &manifest).unwrap();
        let decoded = decode(&path).unwrap();

        assert_eq!(decoded.segments(), manifest.segments());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn empty_manifest_round_trips() {
        let manifest = Manifest::new(Vec::new()).unwrap();
        let bytes = build_manifest(1, &manifest).unwrap();

        assert_eq!(bytes.len(), HEADER_LENGTH + CHECKSUM_LENGTH);
        let decoded = decode_bytes(&bytes, 1).unwrap();
        assert!(decoded.segments().is_empty());
    }

    #[test]
    fn existing_manifest_is_not_replaced() {
        let directory = test_directory();
        let path = manifest_path(&directory, 1);
        fs::write(&path, b"existing").unwrap();

        let error = encode(&path, &populated_manifest()).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"existing");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn truncated_manifests_are_rejected_without_panicking() {
        let bytes = build_manifest(1, &populated_manifest()).unwrap();

        for length in 0..bytes.len() {
            let result = std::panic::catch_unwind(|| decode_bytes(&bytes[..length], 1));
            assert!(result.is_ok(), "decoder panicked at length {length}");
            assert!(
                result.unwrap().is_err(),
                "truncated manifest was accepted at length {length}"
            );
        }
    }

    #[test]
    fn checksum_and_version_errors_are_rejected() {
        let mut checksum_error = build_manifest(1, &populated_manifest()).unwrap();
        checksum_error[0] ^= 0xFF;
        assert_eq!(
            decode_bytes(&checksum_error, 1).unwrap_err().to_string(),
            "manifest checksum mismatch"
        );

        let mut version_error = build_manifest(1, &populated_manifest()).unwrap();
        version_error[8..10].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
        replace_checksum(&mut version_error);
        assert_eq!(
            decode_bytes(&version_error, 1).unwrap_err().to_string(),
            "unsupported manifest format version"
        );
    }

    #[test]
    fn noncanonical_varint_and_trailing_bytes_are_rejected() {
        let mut noncanonical = build_manifest(1, &populated_manifest()).unwrap();
        let file_name_length_offset = HEADER_LENGTH + 8;
        noncanonical[file_name_length_offset] |= 0x80;
        noncanonical.insert(file_name_length_offset + 1, 0);
        replace_checksum(&mut noncanonical);
        assert_eq!(
            decode_bytes(&noncanonical, 1).unwrap_err().to_string(),
            "noncanonical segment file-name length"
        );

        let mut trailing = build_manifest(1, &populated_manifest()).unwrap();
        trailing.insert(trailing.len() - CHECKSUM_LENGTH, 0);
        replace_checksum(&mut trailing);
        assert_eq!(
            decode_bytes(&trailing, 1).unwrap_err().to_string(),
            "trailing manifest bytes"
        );
    }

    #[test]
    fn impossible_segment_count_and_total_overflow_are_rejected() {
        let mut impossible_count = build_manifest(1, &populated_manifest()).unwrap();
        impossible_count[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
        replace_checksum(&mut impossible_count);
        assert_eq!(
            decode_bytes(&impossible_count, 1).unwrap_err().to_string(),
            "segment count exceeds manifest length"
        );

        let mut overflow = build_manifest(1, &populated_manifest()).unwrap();
        let length_offsets = segment_length_offsets(&overflow);
        overflow[length_offsets[0]..length_offsets[0] + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        overflow[length_offsets[1]..length_offsets[1] + 8].copy_from_slice(&1_u64.to_le_bytes());
        replace_checksum(&mut overflow);
        assert_eq!(
            decode_bytes(&overflow, 1).unwrap_err().to_string(),
            "total segment bytes overflow"
        );
    }

    #[test]
    fn writer_revalidates_manifest_state() {
        let duplicate_ids = Manifest::from_decoded_segments_unchecked(vec![
            segment(1, 10, 5, 100, 11),
            SegmentMetadata {
                id: SegmentId::new(1).unwrap(),
                file_name: segment_file_name(2),
                document_count: 20,
                term_count: 8,
                length_bytes: 200,
                checksum: 22,
            },
        ]);
        assert_eq!(
            build_manifest(1, &duplicate_ids).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );

        let unsafe_name = Manifest::from_decoded_segments_unchecked(vec![SegmentMetadata {
            id: SegmentId::new(1).unwrap(),
            file_name: "../segment.idx".to_owned(),
            document_count: 1,
            term_count: 1,
            length_bytes: 1,
            checksum: 1,
        }]);
        assert_eq!(
            build_manifest(1, &unsafe_name).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    fn populated_manifest() -> Manifest {
        Manifest::new(vec![segment(1, 10, 5, 100, 11), segment(2, 20, 8, 200, 22)]).unwrap()
    }

    fn segment(
        id: u64,
        document_count: u32,
        term_count: u32,
        length_bytes: u64,
        checksum: u32,
    ) -> SegmentMetadata {
        SegmentMetadata {
            id: SegmentId::new(id).unwrap(),
            file_name: segment_file_name(id),
            document_count,
            term_count,
            length_bytes,
            checksum,
        }
    }

    fn segment_file_name(id: u64) -> String {
        format!(
            "{SEGMENT_FILE_PREFIX}{id:0width$}{SEGMENT_FILE_SUFFIX}",
            width = FILE_NUMBER_WIDTH
        )
    }

    fn test_directory() -> PathBuf {
        let id = TEST_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("manifest-codec-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        path
    }

    fn manifest_path(directory: &Path, generation: u64) -> PathBuf {
        directory.join(format!(
            "{MANIFEST_FILE_PREFIX}{generation:0width$}{MANIFEST_FILE_SUFFIX}",
            width = FILE_NUMBER_WIDTH
        ))
    }

    fn replace_checksum(bytes: &mut Vec<u8>) {
        bytes.truncate(bytes.len() - CHECKSUM_LENGTH);
        append_checksum(bytes);
    }

    fn segment_length_offsets(bytes: &[u8]) -> Vec<usize> {
        let segment_count = u32::from_le_bytes(bytes[18..22].try_into().unwrap());
        let mut offsets = Vec::with_capacity(segment_count as usize);
        let mut position = HEADER_LENGTH;

        for _ in 0..segment_count {
            position += 8;
            let (file_name_length, varint_length) = varint::decode(&bytes[position..]).unwrap();
            position += varint_length + file_name_length as usize;
            position += 4;
            position += 4;
            offsets.push(position);
            position += 8;
            position += 4;
        }

        offsets
    }
}
