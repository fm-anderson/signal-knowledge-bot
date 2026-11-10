use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
pub struct Terminology {
    aliases: BTreeMap<String, Vec<String>>,
}

impl Terminology {
    pub(super) fn from_json(content: &str) -> Result<Self, serde_json::Error> {
        let aliases = serde_json::from_str(content)?;

        Ok(Self { aliases })
    }

    pub fn load_optional(directory: &Path) -> Result<Self, Box<dyn Error>> {
        let path = directory.join("terminology.json");

        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error.into()),
        };

        Ok(Self::from_json(&content)?)
    }

    pub fn expand(&self, question: &str) -> String {
        let question_words = words(question);
        let mut additions = Vec::new();
        let mut seen = HashSet::new();

        for (alias, expansions) in &self.aliases {
            let alias_words = words(alias);

            // only expand if present in the question.
            if !contains_phrase(&question_words, &alias_words) {
                continue;
            }

            for expansion in expansions {
                let expansion = expansion.trim();
                let expansion_words = words(expansion);

                if expansion_words.is_empty() {
                    continue;
                }

                if contains_phrase(&question_words, &expansion_words) {
                    continue;
                }

                if seen.insert(expansion_words) {
                    additions.push(expansion);
                }
            }
        }

        if additions.is_empty() {
            question.to_string()
        } else {
            format!("{question} {}", additions.join(" "))
        }
    }
}

// normalize text into lowercase words.
// hyphens and punctuation are word boundaries.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

fn contains_phrase(haystack: &[String], needle: &[String]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
