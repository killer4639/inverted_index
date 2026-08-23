use crate::doc_validator::DocumentTokenizer;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};

pub type DocumentId = u32;
pub type TermFrequency = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Posting {
    pub document_id: DocumentId,
    pub term_frequency: TermFrequency,
}

pub struct InvertedIndex {
    postings: BTreeMap<String, Vec<Posting>>,
    next_document_id: DocumentId,
}

impl InvertedIndex {
    pub fn new() -> Self {
        Self {
            postings: BTreeMap::new(),
            next_document_id: 0,
        }
    }

    pub fn create_index(&mut self, path: &str) -> Result<(), String> {
        let file =
            File::open(path).map_err(|error| format!("failed to open document file: {error}"))?;
        let reader = BufReader::new(file);

        for (index, line) in reader.lines().enumerate() {
            let line_number = index + 1;
            let line =
                line.map_err(|error| format!("failed to read line {line_number}: {error}"))?;

            let mut term_frequencies = HashMap::<String, TermFrequency>::new();

            for token in DocumentTokenizer::new(&line) {
                *term_frequencies.entry(token.to_owned()).or_default() += 1;
            }

            for (term, term_frequency) in term_frequencies {
                self.postings.entry(term).or_default().push(Posting {
                    document_id: self.next_document_id,
                    term_frequency,
                });
            }

            self.next_document_id += 1;
        }

        Ok(())
    }

    pub fn query(&self, term: &str) -> &[Posting] {
        self.postings
            .get(term)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    fn write_corpus(contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "inverted_index_phase1_{}_{}.txt",
            std::process::id(),
            TEST_FILE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, contents).expect("write test corpus");
        path
    }

    #[test]
    fn repeated_terms_produce_one_posting_with_correct_frequency() {
        let path = write_corpus("rust rust rust\n");
        let mut index = InvertedIndex::new();
        index.create_index(path.to_str().unwrap()).unwrap();

        let postings = index.query("rust");
        assert_eq!(
            postings,
            [Posting {
                document_id: 0,
                term_frequency: 3,
            }]
        );
    }

    #[test]
    fn shared_terms_have_ordered_document_ids() {
        let path = write_corpus("rust here\nother\nrust again\n");
        let mut index = InvertedIndex::new();
        index.create_index(path.to_str().unwrap()).unwrap();

        let postings = index.query("rust");
        let document_ids: Vec<DocumentId> =
            postings.iter().map(|posting| posting.document_id).collect();
        assert_eq!(document_ids, [0, 2]);
        assert!(
            document_ids.windows(2).all(|pair| pair[0] < pair[1]),
            "document IDs should be strictly increasing"
        );
        assert_eq!(postings[0].term_frequency, 1);
        assert_eq!(postings[1].term_frequency, 1);
    }

    #[test]
    fn queries_are_case_sensitive() {
        let path = write_corpus("Rust rust RUST\n");
        let mut index = InvertedIndex::new();
        index.create_index(path.to_str().unwrap()).unwrap();

        assert_eq!(
            index.query("Rust"),
            [Posting {
                document_id: 0,
                term_frequency: 1,
            }]
        );
        assert_eq!(
            index.query("rust"),
            [Posting {
                document_id: 0,
                term_frequency: 1,
            }]
        );
        assert_eq!(
            index.query("RUST"),
            [Posting {
                document_id: 0,
                term_frequency: 1,
            }]
        );
        assert!(index.query("rUsT").is_empty());
    }

    #[test]
    fn missing_terms_return_empty_postings() {
        let path = write_corpus("rust is fast\n");
        let mut index = InvertedIndex::new();
        index.create_index(path.to_str().unwrap()).unwrap();

        assert!(index.query("zebra").is_empty());
        assert!(index.query("Rust").is_empty());
    }

    #[test]
    fn multiple_indexed_files_continue_document_ids() {
        let first = write_corpus("alpha rust\nbeta rust\n");
        let second = write_corpus("gamma rust\n");
        let mut index = InvertedIndex::new();
        index.create_index(first.to_str().unwrap()).unwrap();
        index.create_index(second.to_str().unwrap()).unwrap();

        assert_eq!(
            index.query("rust"),
            [
                Posting {
                    document_id: 0,
                    term_frequency: 1,
                },
                Posting {
                    document_id: 1,
                    term_frequency: 1,
                },
                Posting {
                    document_id: 2,
                    term_frequency: 1,
                },
            ]
        );
        assert_eq!(
            index.query("gamma"),
            [Posting {
                document_id: 2,
                term_frequency: 1,
            }]
        );
    }
}
