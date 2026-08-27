use crate::postings_codec::PostingsDecoder;
use crate::segment_reader::SegmentReader;
use std::io;
use std::path::Path;

pub struct QueryEngine {
    segment_reader: SegmentReader,
}

impl QueryEngine {
    pub fn new(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            segment_reader: SegmentReader::open(path)?,
        })
    }

    pub fn query_term(&self, term: &str) -> io::Result<Option<PostingsDecoder<'_>>> {
        if term.is_empty()
            || !term
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "query must be one ASCII-alphanumeric word",
            ));
        }

        if self.segment_reader.term_count() == 0 {
            return Ok(None);
        }

        let mut first_term_idx: u32 = 0;
        let mut last_term_idx: u32 = self.segment_reader.term_count();

        while first_term_idx < last_term_idx {
            let mid = first_term_idx + (last_term_idx - first_term_idx) / 2;
            let mid_term = self.segment_reader.get_term(mid)?;

            if mid_term == term {
                let decoder = self.segment_reader.get_postings(mid)?;
                return Ok(Some(decoder));
            } else if mid_term < term {
                first_term_idx = mid + 1;
            } else {
                last_term_idx = mid;
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_codec;
    use crate::inverted_index::{InvertedIndex, Posting};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    static TEST_SEGMENT: OnceLock<()> = OnceLock::new();

    fn test_segment_path() -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("test_segment.idx");
        TEST_SEGMENT.get_or_init(|| {
            if !path.exists() {
                let mut postings = BTreeMap::new();
                postings.insert("rust".to_owned(), vec![posting(0, 2), posting(3, 1)]);
                postings.insert("search".to_owned(), vec![posting(1, 1)]);
                let index = InvertedIndex::from_finalized_postings(postings, 4);
                index_codec::encode(&path, &index).expect("write data/test_segment.idx");
            }
        });
        path
    }

    fn posting(document_id: u32, term_frequency: u32) -> Posting {
        Posting {
            document_id,
            term_frequency,
        }
    }

    fn write_empty_segment() -> PathBuf {
        let id = TEST_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "query-engine-empty-{}-{id}.idx",
            std::process::id()
        ));
        let index = InvertedIndex::from_finalized_postings(BTreeMap::new(), 0);
        index_codec::encode(&path, &index).unwrap();
        path
    }

    fn query_postings(engine: &QueryEngine, term: &str) -> Vec<Posting> {
        let decoder = match engine.query_term(term).unwrap() {
            Some(decoder) => decoder,
            None => return Vec::new(),
        };

        let mut postings = Vec::new();
        for result in decoder {
            postings.push(result.unwrap());
        }
        postings
    }

    #[test]
    fn query_term_finds_first_and_last_dictionary_terms() {
        let engine = QueryEngine::new(test_segment_path()).unwrap();

        assert_eq!(
            query_postings(&engine, "rust"),
            vec![posting(0, 2), posting(3, 1)]
        );
        assert_eq!(query_postings(&engine, "search"), vec![posting(1, 1)]);
    }

    #[test]
    fn query_term_decodes_postings_lazily() {
        let engine = QueryEngine::new(test_segment_path()).unwrap();
        let mut decoder = engine.query_term("rust").unwrap().unwrap();

        assert_eq!(decoder.remaining_postings(), 2);
        assert_eq!(decoder.next(), Some(Ok(posting(0, 2))));
        assert_eq!(decoder.remaining_postings(), 1);
    }

    #[test]
    fn query_term_returns_empty_postings_for_a_missing_term() {
        let engine = QueryEngine::new(test_segment_path()).unwrap();

        assert!(engine.query_term("missing").unwrap().is_none());
        assert!(engine.query_term("aaa").unwrap().is_none());
        assert!(engine.query_term("zzz").unwrap().is_none());
    }

    #[test]
    fn query_term_is_case_sensitive() {
        let engine = QueryEngine::new(test_segment_path()).unwrap();

        assert!(engine.query_term("Rust").unwrap().is_none());
        assert!(engine.query_term("SEARCH").unwrap().is_none());
    }

    #[test]
    fn query_term_rejects_invalid_queries() {
        let engine = QueryEngine::new(test_segment_path()).unwrap();

        let empty = engine.query_term("").unwrap_err();
        let punctuation = engine.query_term("rust!").unwrap_err();
        let whitespace = engine.query_term("rus t").unwrap_err();

        assert_eq!(empty.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(punctuation.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(whitespace.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn query_term_on_an_empty_segment_returns_no_postings() {
        let path = write_empty_segment();
        let engine = QueryEngine::new(&path).unwrap();
        let postings = engine.query_term("rust").unwrap();

        assert!(postings.is_none());
        drop(postings);
        drop(engine);
        fs::remove_file(&path).unwrap();
    }
}
