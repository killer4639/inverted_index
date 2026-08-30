use std::fs::File;
use std::io::{BufRead, BufReader};
use std::str::SplitWhitespace;

pub struct DocumentTokenizer<'a> {
    tokens: SplitWhitespace<'a>,
}

impl<'a> DocumentTokenizer<'a> {
    pub fn new(document: &'a str) -> Self {
        Self {
            tokens: document.split_whitespace(),
        }
    }
}

impl<'a> Iterator for DocumentTokenizer<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        self.tokens.next()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    EmptyDocument { line: usize },
    InvalidTokenCharacter { line: usize, token: String },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyDocument { line } => write!(f, "document on line {line} is empty"),
            Self::InvalidTokenCharacter { line, token } => {
                write!(
                    f,
                    "token '{token}' on line {line} is not ASCII alphanumeric"
                )
            }
        }
    }
}

pub fn validate_doc(path: &str) -> Result<(), String> {
    let file =
        File::open(path).map_err(|error| format!("failed to open document file: {error}"))?;
    let reader = BufReader::new(file);

    for (index, line) in reader.lines().enumerate() {
        let line_number = index + 1;
        let line = line.map_err(|error| format!("failed to read line {line_number}: {error}"))?;
        let mut tokens = DocumentTokenizer::new(&line).peekable();

        if tokens.peek().is_none() {
            return Err(ValidationError::EmptyDocument { line: line_number }.to_string());
        }

        for token in tokens {
            if !token
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
            {
                return Err(ValidationError::InvalidTokenCharacter {
                    line: line_number,
                    token: token.to_owned(),
                }
                .to_string());
            }
        }
    }

    Ok(())
}
