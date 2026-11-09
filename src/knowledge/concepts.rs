use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::path::Path;

#[derive(Default, Serialize, Deserialize)]
pub struct ConceptRules {
    #[serde(default)]
    rules: Vec<ConceptRule>,
}

#[derive(Serialize, Deserialize)]
struct ConceptRule {
    any: Vec<String>,
    #[serde(default)]
    all: Vec<Vec<String>>,
    #[serde(default)]
    exclude: Vec<String>,
    expansions: Vec<String>,
}

impl ConceptRules {
    pub fn load_optional(directory: &Path) -> Result<Self, Box<dyn Error>> {
        let path = directory.join("concepts.json");

        if !path.exists() {
            return Ok(Self::default());
        }

        Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
    }

    pub fn expansions_for(&self, question: &str) -> Vec<String> {
        let normalized = normalize(question);
        let mut expansions = Vec::new();
        let mut seen = HashSet::new();

        for rule in &self.rules {
            let trigger_matches = rule
                .any
                .iter()
                .any(|phrase| contains_phrase(&normalized, phrase));

            let context_matches = rule.all.iter().all(|group| {
                group
                    .iter()
                    .any(|phrase| contains_phrase(&normalized, phrase))
            });

            let excluded = rule
                .exclude
                .iter()
                .any(|phrase| contains_phrase(&normalized, phrase));

            if trigger_matches && context_matches && !excluded {
                for expansion in &rule.expansions {
                    if !expansion.trim().is_empty() && seen.insert(expansion.as_str()) {
                        expansions.push(expansion.clone());
                    }
                }
            }
        }

        expansions
    }
}

// Normalize punctuation and case while preserving word boundaries.
fn normalize(text: &str) -> String {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_phrase(normalized_question: &str, phrase: &str) -> bool {
    let phrase = normalize(phrase);

    if phrase.is_empty() {
        return false;
    }

    format!(" {normalized_question} ").contains(&format!(" {phrase} "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_only_when_trigger_and_context_match() {
        let rules: ConceptRules = serde_json::from_str(
            r#"{
                "rules": [{
                    "any": ["defective brakes", "safety discrepancy"],
                    "all": [
                        ["vehicle", "driver", "operator"],
                        ["inspection", "operating"]
                    ],
                    "exclude": ["bicycle"],
                    "expansions": ["discontinue vehicle use"]
                }]
            }"#,
        )
        .unwrap();

        let relevant = rules.expansions_for(
            "A vehicle operator discovers defective brakes \
             during inspection. Can the vehicle continue operating?",
        );

        assert_eq!(relevant, vec!["discontinue vehicle use"]);

        // A mention of brakes alone should not activate the rule.
        assert!(
            rules
                .expansions_for("What do brake repairs cost?")
                .is_empty()
        );

        // Excluded contexts should not activate the rule.
        assert!(
            rules
                .expansions_for("A bicycle operator finds defective brakes during inspection")
                .is_empty()
        );
    }
}
