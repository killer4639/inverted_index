use std::io::{self, Write};
use std::process;
use std::time::{Duration, Instant};

use inverted_index::{
    IndexCreationStats, IndexSnapshot, IndexStorage, MultiSegmentDictionaryStats, create_index,
    encode_manifest,
};

const USAGE: &str = "usage:
  build <corpus>
  lookup <term>
  lookup-stats <term>";

fn main() {
    let stdin = io::stdin();
    let index_storage = match IndexStorage::new() {
        Ok(index_storage) => index_storage,
        Err(error) => {
            eprintln!("failed to initialize index storage: {error}");
            process::exit(1);
        }
    };
    let mut index_snapshot = match open_index_snapshot(&index_storage) {
        Ok(index_snapshot) => index_snapshot,
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    };

    loop {
        print!("> ");
        if io::stdout().flush().is_err() {
            eprintln!("failed to write prompt");
            process::exit(1);
        }

        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error) => {
                eprintln!("failed to read command: {error}");
                process::exit(1);
            }
        }

        let arguments: Vec<&str> = line.split_whitespace().collect();
        let result = match arguments.as_slice() {
            ["build", corpus_path] => {
                build_segment(corpus_path, &index_storage, &mut index_snapshot)
            }
            ["lookup", term] => lookup_term(term, &index_snapshot, true),
            ["lookup-stats", term] => lookup_term(term, &index_snapshot, false),
            [] => continue,
            _ => {
                eprintln!("{USAGE}");
                continue;
            }
        };

        if let Err(error) = result {
            eprintln!("{error}");
        }
    }
}

fn build_segment(
    corpus_path: &str,
    index_storage: &IndexStorage,
    index_snapshot: &mut IndexSnapshot,
) -> Result<(), String> {
    let segment_id = index_storage
        .reserve_segment_id()
        .map_err(|error| format!("failed to reserve segment ID: {error}"))?;
    let segment_path = index_storage.segment_file_path(segment_id);
    let stats = create_index(corpus_path, &segment_path)?;

    let segment_metadata = index_storage
        .load_segment_metadata(segment_id)
        .map_err(|error| format!("failed to load new segment metadata: {error}"))?;
    let manifest = index_snapshot
        .manifest_with_segment(segment_metadata)
        .map_err(|error| format!("failed to build next manifest: {error:?}"))?;
    let manifest_generation = index_storage
        .reserve_manifest_generation()
        .map_err(|error| format!("failed to reserve manifest generation: {error}"))?;
    let manifest_path = index_storage.manifest_file_path(manifest_generation);

    encode_manifest(&manifest_path, &manifest)
        .map_err(|error| format!("failed to publish manifest: {error}"))?;
    index_snapshot
        .refresh(manifest_generation, manifest, index_storage)
        .map_err(|error| format!("failed to refresh index snapshot: {error}"))?;

    println!(
        "built {} document(s), {} term(s), {} posting(s) into {}",
        stats.corpus.document_count,
        stats.index.unique_term_count,
        stats.index.posting_count,
        segment_path.display()
    );
    print_index_creation_stats(&stats);
    Ok(())
}

fn print_index_creation_stats(stats: &IndexCreationStats) {
    println!("index creation statistics:");
    println!("  input bytes: {}", stats.corpus.input_bytes);
    println!("  tokens: {}", stats.corpus.token_count);
    println!("  segment bytes: {}", stats.segment.segment_bytes);
    println!(
        "  validation: {:.3} ms",
        duration_milliseconds(stats.timings.validation_duration)
    );
    println!(
        "  indexing: {:.3} ms",
        duration_milliseconds(stats.timings.indexing_duration)
    );
    println!(
        "  finalization: {:.3} ms",
        duration_milliseconds(stats.timings.finalization_duration)
    );
    println!(
        "  segment write: {:.3} ms",
        duration_milliseconds(stats.timings.segment_write_duration)
    );
    println!(
        "  total: {:.3} ms",
        duration_milliseconds(stats.timings.total_duration)
    );
}

fn duration_milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn lookup_term(
    term: &str,
    index_snapshot: &IndexSnapshot,
    print_postings: bool,
) -> Result<(), String> {
    let total_started = Instant::now();
    let dictionary_lookup_started = Instant::now();
    let query_result = index_snapshot
        .query_term_with_stats(term)
        .map_err(|error| format!("failed to query index: {error}"))?;
    let dictionary_lookup_duration = dictionary_lookup_started.elapsed();
    let dictionary_stats = query_result.dictionary_stats;

    let postings_decode_started = Instant::now();
    let mut postings = Vec::new();
    for posting in query_result.postings {
        let posting = posting.map_err(|error| {
            format!(
                "failed to decode postings in segment {}: {:?}",
                error.segment_id, error.source
            )
        })?;
        postings.push(posting);
    }
    let postings_decode_duration = postings_decode_started.elapsed();
    let total_duration = total_started.elapsed();

    println!("{} matching document(s)", postings.len());
    if print_postings {
        for posting in &postings {
            println!(
                "segment {}, document {}: frequency {}",
                posting.address.segment_id,
                posting.address.local_document_id,
                posting.term_frequency
            );
        }
    }

    print_lookup_stats(
        index_snapshot,
        dictionary_stats,
        dictionary_lookup_duration,
        postings_decode_duration,
        total_duration,
        postings.len(),
    );
    Ok(())
}

fn print_lookup_stats(
    index_snapshot: &IndexSnapshot,
    dictionary_stats: MultiSegmentDictionaryStats,
    dictionary_lookup_duration: Duration,
    postings_decode_duration: Duration,
    total_duration: Duration,
    matched_document_count: usize,
) {
    println!("lookup statistics:");
    println!("  indexed segments: {}", index_snapshot.segment_count());
    println!("  segment bytes: {}", index_snapshot.total_segment_bytes());
    println!(
        "  indexed documents: {}",
        index_snapshot.total_document_count()
    );
    println!("  term entries: {}", index_snapshot.total_term_entries());
    println!(
        "  segments searched: {}",
        dictionary_stats.searched_segment_count
    );
    println!(
        "  matching segments: {}",
        dictionary_stats.matching_segment_count
    );
    println!(
        "  term found: {}",
        dictionary_stats.matching_segment_count > 0
    );
    println!(
        "  dictionary comparisons: {}",
        dictionary_stats.dictionary_comparisons
    );
    println!("  matched documents: {matched_document_count}");
    println!(
        "  postings bytes: {}",
        dictionary_stats.encoded_postings_bytes
    );
    println!(
        "  dictionary lookup: {:.3} ms",
        duration_milliseconds(dictionary_lookup_duration)
    );
    println!(
        "  postings decode: {:.3} ms",
        duration_milliseconds(postings_decode_duration)
    );
    println!("  total: {:.3} ms", duration_milliseconds(total_duration));
}

fn open_index_snapshot(index_storage: &IndexStorage) -> Result<IndexSnapshot, String> {
    IndexSnapshot::new(index_storage)
        .map_err(|error| format!("failed to open index storage: {error}"))
}
