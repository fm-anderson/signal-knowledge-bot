use std::error::Error;
use std::path::{Path, PathBuf};

mod retrieval;

pub use retrieval::{
    build_bounded_page_context, build_page_context, search_documents, search_pages,
};

pub struct PdfPage {
    pub number: usize,
    pub text: String,
}

pub struct PdfDocument {
    pub path: PathBuf,
    pub pages: Vec<PdfPage>,
}

pub fn load_pdf(path: &Path) -> Result<PdfDocument, Box<dyn Error>> {
    let extracted_pages = pdf_extract::extract_text_by_pages(path)?;

    let pages = extracted_pages
        .into_iter()
        .enumerate()
        .map(|(index, text)| PdfPage {
            number: index + 1,
            text,
        })
        .collect();

    Ok(PdfDocument {
        path: path.to_path_buf(),
        pages,
    })
}

pub fn load_pdf_directory(directory: &Path) -> Result<Vec<PdfDocument>, Box<dyn Error>> {
    let mut paths = std::fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;

    paths.retain(|path| {
        path.is_file()
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
    });

    paths.sort();

    if paths.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No PDF files found in directory",
        )
        .into());
    }

    paths.into_iter().map(|path| load_pdf(&path)).collect()
}
