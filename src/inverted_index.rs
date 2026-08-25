use std::collections::BTreeMap;

pub type DocumentId = u32;
pub type TermFrequency = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Posting {
    pub document_id: DocumentId,
    pub term_frequency: TermFrequency,
}

pub struct InvertedIndex {
    postings: BTreeMap<String, Vec<Posting>>,
    document_count: DocumentId,
    posting_count: usize,
}

impl InvertedIndex {
    pub(crate) fn from_finalized_postings(
        postings: BTreeMap<String, Vec<Posting>>,
        document_count: DocumentId,
    ) -> Self {
        let posting_count = postings.values().map(Vec::len).sum();

        Self {
            postings,
            document_count,
            posting_count,
        }
    }

    pub fn query(&self, term: &str) -> &[Posting] {
        self.postings
            .get(term)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn document_count(&self) -> DocumentId {
        self.document_count
    }

    pub fn term_count(&self) -> usize {
        self.postings.len()
    }

    pub fn posting_count(&self) -> usize {
        self.posting_count
    }
    pub(crate) fn terms(&self) -> impl Iterator<Item = (&String, &Vec<Posting>)> {
        self.postings.iter()
    }
}
