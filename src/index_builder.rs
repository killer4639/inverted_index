use crate::doc_validator::DocumentTokenizer;
use crate::inverted_index::{DocumentId, InvertedIndex, Posting, TermFrequency};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};

pub struct IndexBuilder {
    postings: HashMap<String, Vec<Posting>>,
    next_document_id: DocumentId,
    token_count: u64,
}

impl IndexBuilder {
    pub fn new() -> Self {
        Self {
            postings: HashMap::new(),
            next_document_id: 0,
            token_count: 0,
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
                self.token_count = self
                    .token_count
                    .checked_add(1)
                    .ok_or_else(|| "token count exceeds u64".to_owned())?;

                let term_frequency = term_frequencies.entry(token.to_owned()).or_default();
                *term_frequency = term_frequency
                    .checked_add(1)
                    .ok_or_else(|| format!("term '{token}' frequency exceeds u32"))?;
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

    pub fn token_count(&self) -> u64 {
        self.token_count
    }

    pub fn finalize(self) -> Result<InvertedIndex, String> {
        self.validate()?;

        let document_count = self.next_document_id;
        let postings = self
            .postings
            .into_iter()
            .collect::<BTreeMap<String, Vec<Posting>>>();

        Ok(InvertedIndex::from_finalized_postings(
            postings,
            document_count,
        ))
    }

    fn validate(&self) -> Result<(), String> {
        for (term, postings) in &self.postings {
            if term.is_empty() {
                return Err("cannot finalize an index containing an empty term".to_owned());
            }
            if postings.is_empty() {
                return Err(format!("term '{term}' has no postings"));
            }

            let mut previous_document_id = None;
            for posting in postings {
                if posting.term_frequency == 0 {
                    return Err(format!("term '{term}' has a posting with zero frequency"));
                }

                if posting.document_id >= self.next_document_id {
                    return Err(format!(
                        "term '{term}' contains unknown document ID {}",
                        posting.document_id
                    ));
                }

                if previous_document_id.is_some_and(|previous| previous >= posting.document_id) {
                    return Err(format!(
                        "term '{term}' has document IDs that are not strictly increasing"
                    ));
                }
                previous_document_id = Some(posting.document_id);
            }
        }

        Ok(())
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
            "inverted_index_phase2_{}_{}.txt",
            std::process::id(),
            TEST_FILE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, contents).expect("write test corpus");
        path
    }

    fn finalization_error(builder: IndexBuilder) -> String {
        match builder.finalize() {
            Ok(_) => panic!("finalization should fail"),
            Err(error) => error,
        }
    }

    #[test]
    fn repeated_terms_produce_one_posting_with_correct_frequency() {
        let path = write_corpus("rust rust rust\n");
        let mut builder = IndexBuilder::new();
        builder.create_index(path.to_str().unwrap()).unwrap();

        assert_eq!(builder.token_count(), 3);
        let index = builder.finalize().unwrap();

        assert_eq!(
            index.query("rust"),
            [Posting {
                document_id: 0,
                term_frequency: 3,
            }]
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn multiple_files_continue_document_ids() {
        let first = write_corpus("alpha rust\nbeta rust\n");
        let second = write_corpus("gamma rust\n");
        let mut builder = IndexBuilder::new();
        builder.create_index(first.to_str().unwrap()).unwrap();
        builder.create_index(second.to_str().unwrap()).unwrap();

        assert_eq!(builder.token_count(), 6);
        let index = builder.finalize().unwrap();

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
        fs::remove_file(first).unwrap();
        fs::remove_file(second).unwrap();
    }

    #[test]
    fn missing_terms_return_empty_postings() {
        let path = write_corpus("rust is fast\n");
        let mut builder = IndexBuilder::new();
        builder.create_index(path.to_str().unwrap()).unwrap();

        let index = builder.finalize().unwrap();

        assert!(index.query("zebra").is_empty());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn finalized_index_matches_the_in_memory_oracle() {
        let path = write_corpus("rust fast\nrust safe\nfast\n");
        let mut builder = IndexBuilder::new();
        builder.create_index(path.to_str().unwrap()).unwrap();

        let index = builder.finalize().unwrap();

        assert_eq!(index.document_count(), 3);
        assert_eq!(index.term_count(), 3);
        assert_eq!(index.posting_count(), 5);
        assert_eq!(
            index.query("fast"),
            [
                Posting {
                    document_id: 0,
                    term_frequency: 1,
                },
                Posting {
                    document_id: 2,
                    term_frequency: 1,
                },
            ]
        );
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
            ]
        );
        assert_eq!(
            index.query("safe"),
            [Posting {
                document_id: 1,
                term_frequency: 1,
            }]
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn finalization_rejects_empty_terms() {
        let mut builder = IndexBuilder::new();
        builder.next_document_id = 1;
        builder.postings.insert(
            String::new(),
            vec![Posting {
                document_id: 0,
                term_frequency: 1,
            }],
        );

        assert_eq!(
            finalization_error(builder),
            "cannot finalize an index containing an empty term"
        );
    }

    #[test]
    fn finalization_rejects_empty_posting_lists() {
        let mut builder = IndexBuilder::new();
        builder.postings.insert("rust".to_owned(), Vec::new());

        assert_eq!(finalization_error(builder), "term 'rust' has no postings");
    }

    #[test]
    fn finalization_rejects_zero_frequencies() {
        let mut builder = IndexBuilder::new();
        builder.next_document_id = 1;
        builder.postings.insert(
            "rust".to_owned(),
            vec![Posting {
                document_id: 0,
                term_frequency: 0,
            }],
        );

        assert_eq!(
            finalization_error(builder),
            "term 'rust' has a posting with zero frequency"
        );
    }

    #[test]
    fn finalization_rejects_unknown_document_ids() {
        let mut builder = IndexBuilder::new();
        builder.next_document_id = 1;
        builder.postings.insert(
            "rust".to_owned(),
            vec![Posting {
                document_id: 1,
                term_frequency: 1,
            }],
        );

        assert_eq!(
            finalization_error(builder),
            "term 'rust' contains unknown document ID 1"
        );
    }

    #[test]
    fn finalization_rejects_unordered_document_ids() {
        let mut builder = IndexBuilder::new();
        builder.next_document_id = 2;
        builder.postings.insert(
            "rust".to_owned(),
            vec![
                Posting {
                    document_id: 1,
                    term_frequency: 1,
                },
                Posting {
                    document_id: 0,
                    term_frequency: 1,
                },
            ],
        );

        assert_eq!(
            finalization_error(builder),
            "term 'rust' has document IDs that are not strictly increasing"
        );
    }
}
