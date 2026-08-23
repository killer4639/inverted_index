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
