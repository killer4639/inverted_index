mod doc_validator;
mod index_builder;
mod index_codec;
mod index_creation_stats;
mod index_creator;
mod inverted_index;
mod lookup_executor;
mod lookup_stats;
mod postings_codec;
mod query_engine;
mod segment_reader;
mod varint;

use std::io::{self, Write};
use std::process;
use std::time::Duration;

use crate::index_creation_stats::IndexCreationStats;
use crate::index_creator::create_index;
use crate::lookup_executor::LookupExecutor;
use crate::lookup_stats::LookupStats;

const USAGE: &str = "usage:
  build <corpus> <segment>
  lookup <segment> <term>
  lookup-stats <segment> <term>";

fn main() {
    let stdin = io::stdin();
    let mut lookup_executor = LookupExecutor::new();

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
            ["build", corpus_path, segment_path] => build_segment(corpus_path, segment_path),
            ["lookup", segment_path, term] => {
                lookup_term(segment_path, term, &mut lookup_executor, true)
            }
            ["lookup-stats", segment_path, term] => {
                lookup_term(segment_path, term, &mut lookup_executor, false)
            }
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

fn build_segment(corpus_path: &str, segment_path: &str) -> Result<(), String> {
    let stats = create_index(corpus_path, segment_path)?;

    println!(
        "built {} document(s), {} term(s), {} posting(s) into {segment_path}",
        stats.corpus.document_count, stats.index.unique_term_count, stats.index.posting_count
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
    segment_path: &str,
    term: &str,
    lookup_executor: &mut LookupExecutor,
    print_postings: bool,
) -> Result<(), String> {
    let result = lookup_executor.lookup(segment_path, term)?;

    println!("{} matching document(s)", result.postings.len());
    if print_postings {
        for posting in result.postings {
            println!(
                "document {}: frequency {}",
                posting.document_id, posting.term_frequency
            );
        }
    }

    print_lookup_stats(&result.stats);
    Ok(())
}

fn print_lookup_stats(stats: &LookupStats) {
    let segment_access = if stats.segment.opened_for_lookup {
        "opened"
    } else {
        "reused"
    };

    println!("lookup statistics:");
    println!("  segment access: {segment_access}");
    println!("  segment bytes: {}", stats.segment.segment_bytes);
    println!("  indexed documents: {}", stats.segment.document_count);
    println!("  indexed terms: {}", stats.segment.term_count);
    println!("  term found: {}", stats.query.term_found);
    println!(
        "  dictionary comparisons: {}",
        stats.query.dictionary_comparisons
    );
    println!(
        "  matched documents: {}",
        stats.query.matched_document_count
    );
    println!("  postings bytes: {}", stats.query.postings_bytes);
    println!(
        "  segment open: {:.3} ms",
        duration_milliseconds(stats.timings.segment_open_duration)
    );
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
