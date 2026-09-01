use std::cmp::Ordering;
use std::fmt::Display;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SegmentId(u64);

impl Display for SegmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentAddress {
    pub segment_id: SegmentId,
    pub local_document_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressedPosting {
    pub address: DocumentAddress,
    pub term_frequency: u32,
}

impl SegmentId {
    pub fn new(segment_id: u64) -> Result<SegmentId, String> {
        if segment_id <= 0 {
            return Err("Segment ID LTE to 0 not allowed".to_owned());
        }
        Ok(SegmentId(segment_id))
    }
}

impl Ord for DocumentAddress {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.segment_id.cmp(&other.segment_id) {
            Ordering::Equal => self.local_document_id.cmp(&other.local_document_id),
            segment_ordering => segment_ordering,
        }
    }
}

impl PartialOrd for DocumentAddress {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::{DocumentAddress, SegmentId};

    #[test]
    fn document_addresses_order_by_segment_then_local_document_id() {
        let segment_1 = SegmentId::new(1).unwrap();
        let segment_2 = SegmentId::new(2).unwrap();

        let segment_1_document_1 = DocumentAddress {
            segment_id: segment_1,
            local_document_id: 1,
        };
        let segment_1_document_2 = DocumentAddress {
            segment_id: segment_1,
            local_document_id: 2,
        };
        let segment_2_document_1 = DocumentAddress {
            segment_id: segment_2,
            local_document_id: 1,
        };

        assert!(segment_1_document_1 < segment_1_document_2);
        assert!(segment_1_document_2 < segment_2_document_1);
    }
}
