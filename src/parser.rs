use unicode_segmentation::UnicodeSegmentation;

pub fn prev_grapheme_boundary(text: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }
    let mut prev = 0;
    for (idx, _) in text.grapheme_indices(true) {
        if idx >= cursor {
            break;
        }
        prev = idx;
    }
    prev
}

pub fn next_grapheme_boundary(text: &str, cursor: usize) -> usize {
    if cursor >= text.len() {
        return text.len();
    }
    for (idx, _) in text.grapheme_indices(true) {
        if idx > cursor {
            return idx;
        }
    }
    text.len()
}
