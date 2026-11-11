use super::{LanguageModel, agy::Agy, groq::Groq};
use std::collections::HashMap;
use std::io;

pub fn load_model() -> io::Result<Box<dyn LanguageModel>> {
    let values = read_dotenv()?;

    let provider = setting(&values, "LLM_PROVIDER")?.unwrap_or_else(|| "agy".to_string());

    match provider.as_str() {
        "agy" => {
            eprintln!("Provider: agy");
            Ok(Box::new(Agy))
        }
        "groq" => {
            let key = setting(&values, "GROQ_KEY")?
                .ok_or_else(|| invalid("GROQ_KEY is required for Groq"))?;

            let model = setting(&values, "GROQ_MODEL")?
                .unwrap_or_else(|| "openai/gpt-oss-120b".to_string());

            let groq = Groq::new(&key, model.clone())?;
            eprintln!("Provider: groq | Model: {model}");
            Ok(Box::new(groq))
        }
        _ => Err(invalid("LLM_PROVIDER must be agy or groq")),
    }
}

fn read_dotenv() -> io::Result<HashMap<String, String>> {
    let entries = match dotenvy::from_path_iter(".env") {
        Ok(entries) => entries,
        Err(dotenvy::Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(HashMap::new());
        }
        Err(_) => return Err(invalid("Could not read .env")),
    };

    entries
        .map(|entry| entry.map_err(|_| invalid("Invalid entry in .env")))
        .collect()
}

fn setting(values: &HashMap<String, String>, name: &str) -> io::Result<Option<String>> {
    let value = match std::env::var(name) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => values.get(name).cloned(),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(invalid(&format!("{name} is not valid Unicode")));
        }
    };

    match value {
        Some(value) if value.trim().is_empty() => Err(invalid(&format!("{name} cannot be empty"))),
        Some(value) => Ok(Some(value.trim().to_string())),
        None => Ok(None),
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
