use crate::storage::varint;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) const CHECKSUM_LENGTH: usize = size_of::<u32>();

const COMMON_HEADER_LENGTH: usize = 8 + size_of::<u16>();
const TEMP_FILE_ATTEMPTS: usize = 100;

static TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) trait BinaryFileCodec {
    type Input: ?Sized;
    type Decoded;

    const FILE_TYPE: &'static str;
    const MAGIC: &'static [u8; 8];
    const FORMAT_VERSION: u16;

    fn encode_body(path: &Path, input: &Self::Input, output: &mut Vec<u8>) -> io::Result<()>;

    fn decode_body(path: &Path, decoder: &mut Decoder<'_>) -> io::Result<Self::Decoded>;
}

pub(crate) struct DecodedFile<T> {
    pub value: T,
    pub checksum: u32,
}

pub(crate) struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) fn length(&self) -> usize {
        self.bytes.len()
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.position == self.bytes.len()
    }

    pub(crate) fn read_exact(&mut self, length: usize, field: &str) -> io::Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| invalid_data(format!("{field} range overflow")))?;
        if end > self.bytes.len() {
            return Err(unexpected_eof(format!("truncated {field}")));
        }

        let bytes = &self.bytes[self.position..end];
        self.position = end;
        Ok(bytes)
    }

    pub(crate) fn read_u16(&mut self, field: &str) -> io::Result<u16> {
        let bytes = self.read_exact(size_of::<u16>(), field)?;
        Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub(crate) fn read_u32(&mut self, field: &str) -> io::Result<u32> {
        let bytes = self.read_exact(size_of::<u32>(), field)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub(crate) fn read_u64(&mut self, field: &str) -> io::Result<u64> {
        let bytes = self.read_exact(size_of::<u64>(), field)?;
        Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub(crate) fn read_varint(&mut self, field: &str) -> io::Result<u32> {
        let input = &self.bytes[self.position..];
        let (value, length) = varint::decode(input).map_err(|error| match error {
            varint::DecodeError::Truncated => unexpected_eof(format!("truncated {field}")),
            varint::DecodeError::Overflow => invalid_data(format!("{field} overflow")),
            varint::DecodeError::NonCanonical => invalid_data(format!("noncanonical {field}")),
        })?;
        self.position += length;
        Ok(value)
    }
}

pub(crate) fn encode<C: BinaryFileCodec>(
    path: impl AsRef<Path>,
    input: &C::Input,
) -> io::Result<()> {
    let path = path.as_ref();
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} path already exists", C::FILE_TYPE),
        ));
    }

    let bytes = encode_bytes::<C>(path, input)?;
    publish(path, &bytes, C::FILE_TYPE)
}

pub(crate) fn decode<C: BinaryFileCodec>(
    path: impl AsRef<Path>,
) -> io::Result<DecodedFile<C::Decoded>> {
    let path = path.as_ref();
    let bytes = fs::read(path)?;
    decode_bytes::<C>(path, &bytes)
}

pub(crate) fn encode_bytes<C: BinaryFileCodec>(
    path: &Path,
    input: &C::Input,
) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    output.extend_from_slice(C::MAGIC);
    output.extend_from_slice(&C::FORMAT_VERSION.to_le_bytes());
    C::encode_body(path, input, &mut output)?;
    append_checksum(&mut output);
    Ok(output)
}

pub(crate) fn decode_bytes<C: BinaryFileCodec>(
    path: &Path,
    bytes: &[u8],
) -> io::Result<DecodedFile<C::Decoded>> {
    if bytes.len() < COMMON_HEADER_LENGTH + CHECKSUM_LENGTH {
        return Err(unexpected_eof(format!("{} too small", C::FILE_TYPE)));
    }

    let checksum_start = bytes.len() - CHECKSUM_LENGTH;
    let stored_checksum = u32::from_le_bytes(bytes[checksum_start..].try_into().unwrap());
    let payload = &bytes[..checksum_start];
    let computed_checksum = crc32fast::hash(payload);
    if stored_checksum != computed_checksum {
        return Err(invalid_data(format!("{} checksum mismatch", C::FILE_TYPE)));
    }

    let mut decoder = Decoder::new(payload);
    let magic = decoder.read_exact(C::MAGIC.len(), "file magic")?;
    if magic != C::MAGIC {
        return Err(invalid_data(format!("invalid {} magic", C::FILE_TYPE)));
    }

    let version = decoder.read_u16("file format version")?;
    if version != C::FORMAT_VERSION {
        return Err(invalid_data(format!(
            "unsupported {} format version",
            C::FILE_TYPE
        )));
    }

    let value = C::decode_body(path, &mut decoder)?;
    Ok(DecodedFile {
        value,
        checksum: stored_checksum,
    })
}

pub(crate) fn append_checksum(bytes: &mut Vec<u8>) {
    let checksum = crc32fast::hash(bytes);
    bytes.extend_from_slice(&checksum.to_le_bytes());
}

fn publish(path: &Path, bytes: &[u8], file_type: &str) -> io::Result<()> {
    let (temporary_path, temporary_file) = create_temporary_file(path, file_type)?;
    let _cleanup = TemporaryFileCleanup::new(temporary_path.clone());

    write_temporary_file(temporary_file, bytes)?;
    fs::rename(&temporary_path, path)?;
    Ok(())
}

fn create_temporary_file(path: &Path, file_type: &str) -> io::Result<(PathBuf, File)> {
    let file_name = path
        .file_name()
        .ok_or_else(|| invalid_input(format!("{file_type} path must include a file name")))?;
    let parent = path.parent().unwrap_or_else(|| Path::new(""));

    for _ in 0..TEMP_FILE_ATTEMPTS {
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
        format!("could not create a unique temporary {file_type} file"),
    ))
}

fn write_temporary_file(file: File, bytes: &[u8]) -> io::Result<()> {
    let mut writer = BufWriter::new(file);
    writer.write_all(bytes)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);
    Ok(())
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

pub(crate) fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

pub(crate) fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(crate) fn unexpected_eof(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestCodec;

    impl BinaryFileCodec for TestCodec {
        type Input = [u8];
        type Decoded = Vec<u8>;

        const FILE_TYPE: &'static str = "test";
        const MAGIC: &'static [u8; 8] = b"TESTBIN\0";
        const FORMAT_VERSION: u16 = 3;

        fn encode_body(_path: &Path, input: &Self::Input, output: &mut Vec<u8>) -> io::Result<()> {
            output.extend_from_slice(input);
            Ok(())
        }

        fn decode_body(_path: &Path, decoder: &mut Decoder<'_>) -> io::Result<Self::Decoded> {
            Ok(decoder
                .read_exact(decoder.remaining(), "test payload")?
                .to_vec())
        }
    }

    #[test]
    fn generic_codec_round_trips() {
        let path = test_path("round-trip");

        encode::<TestCodec>(&path, b"payload").unwrap();
        let decoded = decode::<TestCodec>(&path).unwrap();

        assert_eq!(decoded.value, b"payload");
        assert_ne!(decoded.checksum, 0);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn existing_file_is_not_replaced() {
        let path = test_path("existing");
        fs::write(&path, b"existing").unwrap();

        let error = encode::<TestCodec>(&path, b"replacement").unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"existing");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn temporary_file_cleanup_removes_unpublished_file() {
        let path = test_path("cleanup");
        fs::write(&path, b"partial").unwrap();

        let cleanup = TemporaryFileCleanup::new(path.clone());
        drop(cleanup);

        assert!(!path.exists());
    }

    fn test_path(label: &str) -> PathBuf {
        let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "binary-file-{label}-{}-{id}.bin",
            std::process::id()
        ))
    }
}
