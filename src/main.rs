use std::io::{self, Write};
use std::process;
use std::time::Duration;

use inverted_index::{
    IndexCreationStats, IndexSnapshot, IndexStorage, LookupExecutor, LookupStats, create_index,
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
    let lookup_executor = LookupExecutor::new();

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
            ["lookup", term] => lookup_term(term, &index_snapshot, &lookup_executor, true),
            ["lookup-stats", term] => lookup_term(term, &index_snapshot, &lookup_executor, false),
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
    lookup_executor: &LookupExecutor,
    print_postings: bool,
) -> Result<(), String> {
    let lookup_result = lookup_executor.lookup(index_snapshot, term)?;

    println!("{} matching document(s)", lookup_result.postings.len());
    if print_postings {
        for posting in &lookup_result.postings {
            println!(
                "segment {}, document {}: frequency {}",
                posting.address.segment_id,
                posting.address.local_document_id,
                posting.term_frequency
            );
        }
    }

    print_lookup_stats(&lookup_result.stats);
    Ok(())
}

fn print_lookup_stats(stats: &LookupStats) {
    println!("lookup statistics:");
    match stats.snapshot.manifest_generation {
        Some(generation) => println!("  manifest generation: {generation}"),
        None => println!("  manifest generation: none"),
    }
    println!("  indexed segments: {}", stats.snapshot.segment_count);
    println!("  segment bytes: {}", stats.snapshot.total_segment_bytes);
    println!(
        "  indexed documents: {}",
        stats.snapshot.total_document_count
    );
    println!("  term entries: {}", stats.snapshot.total_term_entries);
    println!(
        "  segments searched: {}",
        stats.query.searched_segment_count
    );
    println!(
        "  matching segments: {}",
        stats.query.matching_segment_count
    );
    println!("  term found: {}", stats.query.term_found());
    println!(
        "  dictionary comparisons: {}",
        stats.query.dictionary_comparisons
    );
    println!(
        "  matched documents: {}",
        stats.query.matched_document_count
    );
    println!("  postings bytes: {}", stats.query.encoded_postings_bytes);
    println!(
        "  dictionary lookup: {:.3} ms",
        duration_milliseconds(stats.timings.dictionary_lookup_duration)
    );
    println!(
        "  postings decode: {:.3} ms",
        duration_milliseconds(stats.timings.postings_decode_duration)
    );
    println!(
        "  total: {:.3} ms",
        duration_milliseconds(stats.timings.total_duration)
    );
}

fn open_index_snapshot(index_storage: &IndexStorage) -> Result<IndexSnapshot, String> {
    IndexSnapshot::new(index_storage)
        .map_err(|error| format!("failed to open index storage: {error}"))
}
