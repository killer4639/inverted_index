use std::io::{self, Write};
use std::process;
use std::time::{Duration, Instant};

use inverted_index::{
    IndexCreationStats, IndexStorage, MultiSegmentDictionaryStats, MultiSegmentQueryEngine,
    create_index,
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
    let mut query_engine = match open_query_engine(&index_storage) {
        Ok(query_engine) => query_engine,
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
            ["build", corpus_path] => build_segment(corpus_path, &index_storage, &mut query_engine),
            ["lookup", term] => lookup_term(term, &query_engine, true),
            ["lookup-stats", term] => lookup_term(term, &query_engine, false),
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
    query_engine: &mut MultiSegmentQueryEngine,
) -> Result<(), String> {
    let segment_path = index_storage
        .reserve_segment_file_path()
        .map_err(|error| format!("failed to reserve segment path: {error}"))?;
    let stats = create_index(corpus_path, &segment_path)?;
    let refreshed_query_engine = open_query_engine(index_storage)?;
    *query_engine = refreshed_query_engine;

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
    query_engine: &MultiSegmentQueryEngine,
    print_postings: bool,
) -> Result<(), String> {
    let total_started = Instant::now();
    let dictionary_lookup_started = Instant::now();
    let query_result = query_engine
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
        query_engine,
        dictionary_stats,
        dictionary_lookup_duration,
        postings_decode_duration,
        total_duration,
        postings.len(),
    );
    Ok(())
}

fn print_lookup_stats(
    query_engine: &MultiSegmentQueryEngine,
    dictionary_stats: MultiSegmentDictionaryStats,
    dictionary_lookup_duration: Duration,
    postings_decode_duration: Duration,
    total_duration: Duration,
    matched_document_count: usize,
) {
    println!("lookup statistics:");
    println!("  indexed segments: {}", query_engine.segment_count());
    println!("  segment bytes: {}", query_engine.total_segment_bytes());
    println!(
        "  indexed documents: {}",
        query_engine.total_document_count()
    );
    println!("  term entries: {}", query_engine.total_term_entries());
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

fn open_query_engine(index_storage: &IndexStorage) -> Result<MultiSegmentQueryEngine, String> {
    let manifest = index_storage
        .manifest_from_published_segments()
        .map_err(|error| format!("failed to discover published segments: {error}"))?;
    MultiSegmentQueryEngine::new(manifest, index_storage.segment_directory())
        .map_err(|error| format!("failed to open published segments: {error}"))
}
