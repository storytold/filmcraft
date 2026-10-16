//! Shared search for title, caption, template and replacement font pickers.

pub(crate) fn options(ui: &mut egui::Ui, query: &mut String) -> (Vec<String>, egui::Response) {
    let response = ui.add(egui::TextEdit::singleline(query).hint_text(tl!("Search")).desired_width(ui.available_width()));
    ui.separator();
    if !filmcraft_text::fonts::system_scanned() {
        filmcraft_text::fonts::scan_system();
    }
    let families = filmcraft_text::families().into_iter().map(|(name, _)| name);
    (matching(families, query), response)
}

fn matching(families: impl IntoIterator<Item = String>, query: &str) -> Vec<String> {
    let query = query.to_lowercase();
    let terms: Vec<_> = query.split_whitespace().collect();
    families.into_iter().filter(|name| !name.starts_with('.') && terms.iter().all(|term| name.to_lowercase().contains(term))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_all_words_case_insensitively_and_hides_private_faces() {
        let names = ["Noto Sans", "Noto Serif", "Noto Sans CJK", ".System", "Écriture"];
        let find = |query| matching(names.map(str::to_owned), query);
        assert_eq!(find("  SANS noto "), ["Noto Sans", "Noto Sans CJK"]);
        assert_eq!(find("ÉCR"), ["Écriture"]);
        assert!(find("missing").is_empty());
        assert_eq!(find(" "), ["Noto Sans", "Noto Serif", "Noto Sans CJK", "Écriture"]);
    }
}
