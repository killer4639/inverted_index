mod doc_validator;
mod inverted_index;

use std::io::{self, Write};
use std::process;

use crate::inverted_index::InvertedIndex;
use doc_validator::validate_doc;

fn main() {
    let stdin = io::stdin();
    let mut inverted_index = inverted_index::InvertedIndex::new();

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

        let mut parts = line.split_whitespace();
        let result = match (parts.next(), parts.next()) {
            (Some("index"), Some(path)) => index_document(path, &mut inverted_index),
            (Some("query"), Some(word)) => query_word(word, &inverted_index),
            (None, _) => continue,
            _ => {
                eprintln!("usage:\n  index <path>\n  query <word>");
                process::exit(2);
            }
        };

        if let Err(error) = result {
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

fn index_document(path: &str, inverted_index: &mut InvertedIndex) -> Result<(), String> {
    validate_doc(path)?;
    inverted_index.create_index(path)?;
    Ok(())
}

fn query_word(word: &str, inverted_index: &InvertedIndex) -> Result<(), String> {
    if !word
        .chars()
        .all(|character| character.is_ascii_alphanumeric())
    {
        return Err("query must be one ASCII-alphanumeric word".to_owned());
    }

    let postings = inverted_index.query(word);
    println!("{} matching document(s)", postings.len());

    for posting in postings {
        println!(
            "document {}: frequency {}",
            posting.document_id, posting.term_frequency
        );
    }

    Ok(())
}
