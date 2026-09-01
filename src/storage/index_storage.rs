use crate::SegmentId;
use crate::storage::manifest::{
    MANIFEST_FILE_PREFIX, MANIFEST_FILE_SUFFIX, Manifest, ManifestError, SEGMENT_FILE_PREFIX,
    SEGMENT_FILE_SUFFIX, SegmentMetadata,
};
use crate::storage::segment_reader::SegmentReader;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const INDEX_DIRECTORY: &str = "index";
const MANIFEST_DIRECTORY: &str = "manifests";
const SEGMENT_DIRECTORY: &str = "segments";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterError {
    ManifestGenerationExhausted,
    SegmentIdExhausted,
}

#[derive(Debug)]
pub enum IndexStorageError {
    Io(io::Error),
    Counter(CounterError),
    InvalidSegmentId(u64),
    Manifest(ManifestError),
}

pub struct IndexStorage {
    index_directory: PathBuf,
    manifest_generation_next: AtomicU64,
    segment_id_next: AtomicU64,
}

impl IndexStorage {
    pub fn new() -> Result<Self, IndexStorageError> {
        let index_directory = PathBuf::from(INDEX_DIRECTORY);
        let manifest_directory = index_directory.join(MANIFEST_DIRECTORY);
        let segment_directory = index_directory.join(SEGMENT_DIRECTORY);

        fs::create_dir_all(&manifest_directory)?;
        fs::create_dir_all(&segment_directory)?;

        let manifest_generation_highest = get_highest_file_number(
            &manifest_directory,
            MANIFEST_FILE_PREFIX,
            MANIFEST_FILE_SUFFIX,
        )?;
        let segment_id_highest =
            get_highest_file_number(&segment_directory, SEGMENT_FILE_PREFIX, SEGMENT_FILE_SUFFIX)?;

        let manifest_generation_next = manifest_generation_highest
            .checked_add(1)
            .ok_or(CounterError::ManifestGenerationExhausted)?;
        let segment_id_next = segment_id_highest
            .checked_add(1)
            .ok_or(CounterError::SegmentIdExhausted)?;

        Ok(Self {
            index_directory,
            manifest_generation_next: AtomicU64::new(manifest_generation_next),
            segment_id_next: AtomicU64::new(segment_id_next),
        })
    }

    pub fn index_directory(&self) -> &Path {
        &self.index_directory
    }

    pub fn manifest_directory(&self) -> PathBuf {
        self.index_directory.join(MANIFEST_DIRECTORY)
    }

    pub fn segment_directory(&self) -> PathBuf {
        self.index_directory.join(SEGMENT_DIRECTORY)
    }

    pub fn manifest_file_path(&self, generation: u64) -> PathBuf {
        let file_name = format!("{MANIFEST_FILE_PREFIX}{generation:016}{MANIFEST_FILE_SUFFIX}");
        self.manifest_directory().join(file_name)
    }

    pub fn segment_file_path(&self, segment_id: u64) -> PathBuf {
        let file_name = format!("{SEGMENT_FILE_PREFIX}{segment_id:016}{SEGMENT_FILE_SUFFIX}");
        self.segment_directory().join(file_name)
    }

    pub fn reserve_manifest_file_path(&self) -> Result<PathBuf, CounterError> {
        let generation = self.reserve_manifest_generation()?;
        Ok(self.manifest_file_path(generation))
    }

    pub fn reserve_segment_file_path(&self) -> Result<PathBuf, CounterError> {
        let segment_id = self.reserve_segment_id()?;
        Ok(self.segment_file_path(segment_id))
    }

    pub fn manifest_from_published_segments(&self) -> Result<Manifest, IndexStorageError> {
        // Before manifest persistence exists, every valid published segment is active.
        let published_segments = get_numbered_files(
            &self.segment_directory(),
            SEGMENT_FILE_PREFIX,
            SEGMENT_FILE_SUFFIX,
        )?;
        let mut segments = Vec::with_capacity(published_segments.len());

        for published_segment in published_segments {
            let segment_id = SegmentId::new(published_segment.number)
                .map_err(|_| IndexStorageError::InvalidSegmentId(published_segment.number))?;
            let reader = SegmentReader::open(&published_segment.path)?;
            let length_bytes = fs::metadata(&published_segment.path)?.len();

            segments.push(SegmentMetadata {
                id: segment_id,
                file_name: published_segment.file_name,
                document_count: reader.document_count(),
                term_count: reader.term_count(),
                length_bytes,
                checksum: reader.checksum(),
            });
        }

        Manifest::new(segments).map_err(IndexStorageError::Manifest)
    }

    pub fn reserve_manifest_generation(&self) -> Result<u64, CounterError> {
        reserve_counter(
            &self.manifest_generation_next,
            CounterError::ManifestGenerationExhausted,
        )
    }

    pub fn reserve_segment_id(&self) -> Result<u64, CounterError> {
        reserve_counter(&self.segment_id_next, CounterError::SegmentIdExhausted)
    }
}

struct NumberedFile {
    number: u64,
    file_name: String,
    path: PathBuf,
}

fn get_highest_file_number(
    directory: &Path,
    file_prefix: &str,
    file_suffix: &str,
) -> io::Result<u64> {
    let mut highest_number = 0;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }

        let file_name = entry.file_name();
        let Some(file_number) = parse_file_number(file_name.to_str(), file_prefix, file_suffix)
        else {
            continue;
        };

        highest_number = highest_number.max(file_number);
    }

    Ok(highest_number)
}

fn get_numbered_files(
    directory: &Path,
    file_prefix: &str,
    file_suffix: &str,
) -> io::Result<Vec<NumberedFile>> {
    let mut files = Vec::new();

    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }

        let file_name = entry.file_name();
        let Some(file_number) = parse_file_number(file_name.to_str(), file_prefix, file_suffix)
        else {
            continue;
        };
        let Some(file_name) = file_name.to_str() else {
            continue;
        };

        files.push(NumberedFile {
            number: file_number,
            file_name: file_name.to_owned(),
            path: entry.path(),
        });
    }

    files.sort_unstable_by_key(|file| file.number);
    Ok(files)
}

fn parse_file_number(file_name: Option<&str>, file_prefix: &str, file_suffix: &str) -> Option<u64> {
    let number_text = file_name?
        .strip_prefix(file_prefix)?
        .strip_suffix(file_suffix)?;
    if number_text.is_empty()
        || !number_text
            .bytes()
            .all(|character| character.is_ascii_digit())
    {
        return None;
    }

    number_text.parse::<u64>().ok()
}

fn reserve_counter(counter: &AtomicU64, exhausted: CounterError) -> Result<u64, CounterError> {
    // The counter provides uniqueness only; it does not publish other memory between threads.
    match counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
        next.checked_add(1)
    }) {
        Ok(reserved) => Ok(reserved),
        Err(_) => Err(exhausted),
    }
}

impl From<io::Error> for IndexStorageError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<CounterError> for IndexStorageError {
    fn from(error: CounterError) -> Self {
        Self::Counter(error)
    }
}

impl fmt::Display for CounterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ManifestGenerationExhausted => {
                formatter.write_str("manifest generation counter exhausted")
            }
            Self::SegmentIdExhausted => formatter.write_str("segment ID counter exhausted"),
        }
    }
}

impl Error for CounterError {}

impl fmt::Display for IndexStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "index storage I/O error: {error}"),
            Self::Counter(error) => error.fmt(formatter),
            Self::InvalidSegmentId(segment_id) => {
                write!(formatter, "published segment has invalid ID {segment_id}")
            }
            Self::Manifest(error) => write!(formatter, "invalid segment manifest: {error:?}"),
        }
    }
}

impl Error for IndexStorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Counter(error) => Some(error),
            Self::InvalidSegmentId(_) | Self::Manifest(_) => None,
        }
    }
}
