use crate::inverted_index::{DocumentId, Posting};
use crate::varint;

#[derive(Debug, PartialEq, Eq)]
pub enum EncodeError {
    ZeroFrequency {
        document_id: DocumentId,
    },
    DocumentIdsNotStrictlyIncreasing {
        previous: DocumentId,
        current: DocumentId,
    },
}

fn validate(postings: &[Posting]) -> Result<(), EncodeError> {
    let mut last_document_id = None;

    for posting in postings {
        match last_document_id {
            Some(previous) if previous >= posting.document_id => {
                return Err(EncodeError::DocumentIdsNotStrictlyIncreasing {
                    previous,
                    current: posting.document_id,
                });
            }
            _ => {}
        }

        if posting.term_frequency == 0 {
            return Err(EncodeError::ZeroFrequency {
                document_id: posting.document_id,
            });
        }

        last_document_id = Some(posting.document_id);
    }

    Ok(())
}

pub fn encode(postings: &[Posting], output: &mut Vec<u8>) -> Result<(), EncodeError> {
    validate(postings)?;
    let mut last_document_id = None;

    for posting in postings {
        let document_delta = match last_document_id {
            Some(previous) => posting.document_id - previous,
            None => posting.document_id,
        };

        varint::encode(document_delta, output);
        varint::encode(posting.term_frequency, output);
        last_document_id = Some(posting.document_id);
    }

    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    InvalidDocumentDelta(varint::DecodeError),
    InvalidTermFrequency(varint::DecodeError),
    DocumentIdOverflow,
    ZeroDelta { previous_document_id: DocumentId },
    ZeroFrequency { document_id: DocumentId },
    TrailingBytes,
}

#[derive(Debug)]
pub struct PostingsDecoder<'a> {
    remaining_bytes: &'a [u8],
    previous_document_id: Option<DocumentId>,
    remaining_postings: usize,
    finished: bool,
}

impl<'a> PostingsDecoder<'a> {
    pub fn new(bytes: &'a [u8], posting_count: usize) -> Self {
        Self {
            remaining_bytes: bytes,
            previous_document_id: None,
            remaining_postings: posting_count,
            finished: false,
        }
    }

    pub fn remaining_postings(&self) -> usize {
        self.remaining_postings
    }

    pub fn remaining_bytes(&self) -> usize {
        self.remaining_bytes.len()
    }

    fn fail(&mut self, error: DecodeError) -> Option<Result<Posting, DecodeError>> {
        self.finished = true;
        Some(Err(error))
    }
}

impl Iterator for PostingsDecoder<'_> {
    type Item = Result<Posting, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        if self.remaining_postings == 0 {
            self.finished = true;
            if self.remaining_bytes.is_empty() {
                return None;
            }
            return Some(Err(DecodeError::TrailingBytes));
        }

        let (document_delta, delta_bytes) = match varint::decode(self.remaining_bytes) {
            Ok(decoded) => decoded,
            Err(error) => {
                return self.fail(DecodeError::InvalidDocumentDelta(error));
            }
        };
        self.remaining_bytes = &self.remaining_bytes[delta_bytes..];

        let document_id = match self.previous_document_id {
            Some(previous) => {
                if document_delta == 0 {
                    return self.fail(DecodeError::ZeroDelta {
                        previous_document_id: previous,
                    });
                }
                match previous.checked_add(document_delta) {
                    Some(document_id) => document_id,
                    None => return self.fail(DecodeError::DocumentIdOverflow),
                }
            }
            None => document_delta,
        };

        let (term_frequency, frequency_bytes) = match varint::decode(self.remaining_bytes) {
            Ok(decoded) => decoded,
            Err(error) => {
                return self.fail(DecodeError::InvalidTermFrequency(error));
            }
        };

        if term_frequency == 0 {
            return self.fail(DecodeError::ZeroFrequency { document_id });
        }

        self.remaining_bytes = &self.remaining_bytes[frequency_bytes..];
        self.remaining_postings -= 1;
        self.previous_document_id = Some(document_id);

        Some(Ok(Posting {
            document_id,
            term_frequency,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posting(document_id: DocumentId, term_frequency: u32) -> Posting {
        Posting {
            document_id,
            term_frequency,
        }
    }

    #[test]
    fn encode_empty_posting_list() {
        let mut output = Vec::new();

        encode(&[], &mut output).unwrap();

        assert!(output.is_empty());
    }

    #[test]
    fn encode_posting_for_document_zero() {
        let postings = [posting(0, 1)];
        let mut output = Vec::new();

        encode(&postings, &mut output).unwrap();

        assert_eq!(output, [0x00, 0x01]);
    }

    #[test]
    fn encode_document_deltas_and_frequencies() {
        let postings = [posting(0, 1), posting(127, 2), posting(255, 3)];
        let mut output = Vec::new();

        encode(&postings, &mut output).unwrap();

        assert_eq!(
            output,
            [
                0x00, 0x01, // document 0: delta 0, frequency 1
                0x7F, 0x02, // document 127: delta 127, frequency 2
                0x80, 0x01, 0x03, // document 255: delta 128, frequency 3
            ]
        );
    }

    #[test]
    fn encode_appends_to_existing_output() {
        let postings = [posting(3, 2)];
        let mut output = vec![0xAA];

        encode(&postings, &mut output).unwrap();

        assert_eq!(output, [0xAA, 0x03, 0x02]);
    }

    #[test]
    fn encode_rejects_duplicate_document_ids() {
        let postings = [posting(3, 1), posting(3, 2)];
        let mut output = Vec::new();

        let error = encode(&postings, &mut output).unwrap_err();

        assert_eq!(
            error,
            EncodeError::DocumentIdsNotStrictlyIncreasing {
                previous: 3,
                current: 3,
            }
        );
    }

    #[test]
    fn encode_rejects_decreasing_document_ids() {
        let postings = [posting(5, 1), posting(2, 1)];
        let mut output = Vec::new();

        let error = encode(&postings, &mut output).unwrap_err();

        assert_eq!(
            error,
            EncodeError::DocumentIdsNotStrictlyIncreasing {
                previous: 5,
                current: 2,
            }
        );
    }

    #[test]
    fn encode_rejects_zero_frequency() {
        let postings = [posting(4, 0)];
        let mut output = Vec::new();

        let error = encode(&postings, &mut output).unwrap_err();

        assert_eq!(error, EncodeError::ZeroFrequency { document_id: 4 });
    }

    #[test]
    fn decode_empty_posting_list() {
        let mut decoder = PostingsDecoder::new(&[], 0);

        assert_eq!(decoder.next(), None);
    }

    #[test]
    fn decode_reconstructs_document_ids_lazily() {
        let bytes = [0x00, 0x01, 0x7F, 0x02, 0x80, 0x01, 0x03];
        let mut decoder = PostingsDecoder::new(&bytes, 3);

        assert_eq!(decoder.next(), Some(Ok(posting(0, 1))));
        assert_eq!(decoder.next(), Some(Ok(posting(127, 2))));
        assert_eq!(decoder.next(), Some(Ok(posting(255, 3))));
        assert_eq!(decoder.next(), None);
    }

    #[test]
    fn posting_list_round_trips() {
        let postings = [
            posting(0, 1),
            posting(127, 2),
            posting(255, 3),
            posting(u32::MAX, 4),
        ];
        let mut bytes = Vec::new();
        encode(&postings, &mut bytes).unwrap();

        let decoder = PostingsDecoder::new(&bytes, postings.len());
        let mut decoded = Vec::new();
        for result in decoder {
            decoded.push(result.unwrap());
        }

        assert_eq!(decoded, postings);
    }

    #[test]
    fn decode_rejects_truncated_document_delta() {
        let mut decoder = PostingsDecoder::new(&[], 1);

        assert_eq!(
            decoder.next(),
            Some(Err(DecodeError::InvalidDocumentDelta(
                varint::DecodeError::Truncated
            )))
        );
        assert_eq!(decoder.next(), None);
    }

    #[test]
    fn decode_rejects_truncated_term_frequency() {
        let bytes = [0x01];
        let mut decoder = PostingsDecoder::new(&bytes, 1);

        assert_eq!(
            decoder.next(),
            Some(Err(DecodeError::InvalidTermFrequency(
                varint::DecodeError::Truncated
            )))
        );
        assert_eq!(decoder.next(), None);
    }

    #[test]
    fn decode_rejects_zero_delta_after_first_posting() {
        let bytes = [0x03, 0x01, 0x00, 0x01];
        let mut decoder = PostingsDecoder::new(&bytes, 2);

        assert_eq!(decoder.next(), Some(Ok(posting(3, 1))));
        assert_eq!(
            decoder.next(),
            Some(Err(DecodeError::ZeroDelta {
                previous_document_id: 3
            }))
        );
        assert_eq!(decoder.next(), None);
    }

    #[test]
    fn decode_allows_delta_equal_to_previous_document_id() {
        let bytes = [0x03, 0x01, 0x03, 0x01];
        let mut decoder = PostingsDecoder::new(&bytes, 2);

        assert_eq!(decoder.next(), Some(Ok(posting(3, 1))));
        assert_eq!(decoder.next(), Some(Ok(posting(6, 1))));
        assert_eq!(decoder.next(), None);
    }

    #[test]
    fn decode_rejects_document_id_overflow() {
        let mut bytes = Vec::new();
        varint::encode(u32::MAX, &mut bytes);
        varint::encode(1, &mut bytes);
        varint::encode(1, &mut bytes);
        varint::encode(1, &mut bytes);
        let mut decoder = PostingsDecoder::new(&bytes, 2);

        assert_eq!(decoder.next(), Some(Ok(posting(u32::MAX, 1))));
        assert_eq!(decoder.next(), Some(Err(DecodeError::DocumentIdOverflow)));
        assert_eq!(decoder.next(), None);
    }

    #[test]
    fn decode_rejects_zero_frequency() {
        let bytes = [0x04, 0x00];
        let mut decoder = PostingsDecoder::new(&bytes, 1);

        assert_eq!(
            decoder.next(),
            Some(Err(DecodeError::ZeroFrequency { document_id: 4 }))
        );
        assert_eq!(decoder.next(), None);
    }

    #[test]
    fn decode_rejects_trailing_bytes() {
        let bytes = [0x00, 0x01, 0x02];
        let mut decoder = PostingsDecoder::new(&bytes, 1);

        assert_eq!(decoder.next(), Some(Ok(posting(0, 1))));
        assert_eq!(decoder.next(), Some(Err(DecodeError::TrailingBytes)));
        assert_eq!(decoder.next(), None);
    }

    #[test]
    fn decode_rejects_fewer_postings_than_expected() {
        let bytes = [0x00, 0x01];
        let mut decoder = PostingsDecoder::new(&bytes, 2);

        assert_eq!(decoder.next(), Some(Ok(posting(0, 1))));
        assert_eq!(
            decoder.next(),
            Some(Err(DecodeError::InvalidDocumentDelta(
                varint::DecodeError::Truncated
            )))
        );
        assert_eq!(decoder.next(), None);
    }
}
