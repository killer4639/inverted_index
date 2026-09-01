use crate::storage::postings::{DecodeError, PostingsDecoder};
use crate::{AddressedPosting, DocumentAddress, SegmentId};

#[derive(Debug, PartialEq, Eq)]
pub struct SegmentDecodeError {
    pub segment_id: SegmentId,
    pub source: DecodeError,
}

pub struct MultiSegmentPostings<'a> {
    segment_decoders: Vec<(SegmentId, PostingsDecoder<'a>)>,
    current_segment_index: usize,
}

impl<'a> MultiSegmentPostings<'a> {
    pub(crate) fn new(segment_decoders: Vec<(SegmentId, PostingsDecoder<'a>)>) -> Self {
        Self {
            segment_decoders,
            current_segment_index: 0,
        }
    }
}

impl Iterator for MultiSegmentPostings<'_> {
    type Item = Result<AddressedPosting, SegmentDecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (segment_id, decoder) =
                self.segment_decoders.get_mut(self.current_segment_index)?;
            let segment_id = *segment_id;

            match decoder.next() {
                Some(Ok(posting)) => {
                    return Some(Ok(AddressedPosting {
                        address: DocumentAddress {
                            segment_id,
                            local_document_id: posting.document_id,
                        },
                        term_frequency: posting.term_frequency,
                    }));
                }
                Some(Err(source)) => {
                    self.current_segment_index += 1;
                    return Some(Err(SegmentDecodeError { segment_id, source }));
                }
                None => self.current_segment_index += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MultiSegmentPostings, SegmentDecodeError};
    use crate::storage::postings::{DecodeError, PostingsDecoder};
    use crate::{AddressedPosting, DocumentAddress, SegmentId};

    #[test]
    fn iterates_segments_in_order_and_preserves_local_document_ids() {
        let segment_1_bytes = [0, 2];
        let segment_2_bytes = [0, 3];
        let decoders = vec![
            (
                SegmentId::new(1).unwrap(),
                PostingsDecoder::new(&segment_1_bytes, 1),
            ),
            (
                SegmentId::new(2).unwrap(),
                PostingsDecoder::new(&segment_2_bytes, 1),
            ),
        ];

        let postings = MultiSegmentPostings::new(decoders)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(
            postings,
            vec![
                AddressedPosting {
                    address: DocumentAddress {
                        segment_id: SegmentId::new(1).unwrap(),
                        local_document_id: 0,
                    },
                    term_frequency: 2,
                },
                AddressedPosting {
                    address: DocumentAddress {
                        segment_id: SegmentId::new(2).unwrap(),
                        local_document_id: 0,
                    },
                    term_frequency: 3,
                },
            ]
        );
    }

    #[test]
    fn decode_error_contains_the_segment_id() {
        let invalid_postings = [0];
        let segment_id = SegmentId::new(7).unwrap();
        let decoders = vec![(segment_id, PostingsDecoder::new(&invalid_postings, 1))];

        let error = MultiSegmentPostings::new(decoders)
            .next()
            .unwrap()
            .unwrap_err();

        assert_eq!(
            error,
            SegmentDecodeError {
                segment_id,
                source: DecodeError::InvalidTermFrequency(
                    crate::storage::varint::DecodeError::Truncated,
                ),
            }
        );
    }
}
