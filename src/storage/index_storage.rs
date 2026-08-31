use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterError {
    ManifestGenerationExhausted,
    SegmentIdExhausted,
}

pub struct IndexStorage {
    index_directory: PathBuf,
    manifest_generation_next: AtomicU64,
    segment_id_next: AtomicU64,
}

impl IndexStorage {
    pub fn new(
        index_directory: impl Into<PathBuf>,
        manifest_generation_highest: u64,
        segment_id_highest: u64,
    ) -> Result<Self, CounterError> {
        let manifest_generation_next = manifest_generation_highest
            .checked_add(1)
            .ok_or(CounterError::ManifestGenerationExhausted)?;
        let segment_id_next = segment_id_highest
            .checked_add(1)
            .ok_or(CounterError::SegmentIdExhausted)?;

        Ok(Self {
            index_directory: index_directory.into(),
            manifest_generation_next: AtomicU64::new(manifest_generation_next),
            segment_id_next: AtomicU64::new(segment_id_next),
        })
    }

    pub fn index_directory(&self) -> &Path {
        &self.index_directory
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

fn reserve_counter(counter: &AtomicU64, exhausted: CounterError) -> Result<u64, CounterError> {
    // The counter provides uniqueness only; it does not publish other memory between threads.
    match counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
        next.checked_add(1)
    }) {
        Ok(reserved) => Ok(reserved),
        Err(_) => Err(exhausted),
    }
}
