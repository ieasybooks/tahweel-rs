use imprimatur::doc::{Document, Paragraph};
use imprimatur::style::{Align, TextDirection};
use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_script::{Script, UnicodeScript};

use super::normalize_newlines;

pub(super) fn render(pages: &[String]) -> imprimatur::Result<Vec<u8>> {
    let mut document = Document::new();
    // tahweel.rb writes 10 pt text; its 40-line page budget assumes that size.
    document.set_font_size(10.0)?;
    for (index, page) in pages.iter().enumerate() {
        let text = compact(page);
        let (align, direction) = if is_arabic(&text) {
            (Align::Right, TextDirection::RightToLeft)
        } else {
            (Align::Left, TextDirection::LeftToRight)
        };
        // Imprimatur emits a w:br for each newline and w:tab for each tab.
        document.push_paragraph(
            Paragraph::new()
                .text(text)
                .align(align)
                .direction(direction),
        )?;
        if index + 1 < pages.len() {
            document.page_break();
        }
    }
    document.to_docx()
}

fn compact(text: &str) -> String {
    let normalized = normalize_newlines(text);
    let mut previous = None;
    let collapsed: String = normalized
        .chars()
        .filter(|&ch| {
            let repeated = ruby_whitespace(ch) && previous == Some(ch);
            previous = Some(ch);
            !repeated
        })
        .collect();
    let mut lines: Vec<_> = collapsed
        .trim_matches(|ch| ruby_whitespace(ch) || ch == '\0')
        .split('\n')
        .map(str::to_owned)
        .collect();
    while lines.len()
        + lines
            .iter()
            .filter(|line| line.chars().count() > 80)
            .count()
        > 40
    {
        merge_shortest(&mut lines);
    }
    lines.join("\n")
}

fn merge_shortest(lines: &mut Vec<String>) {
    let Some(index) = lines
        .windows(2)
        .enumerate()
        .min_by_key(|(_, pair)| pair[0].chars().count() + pair[1].chars().count())
        .map(|(index, _)| index)
    else {
        return;
    };
    let next = lines.remove(index + 1);
    lines[index].push(' ');
    lines[index].push_str(&next);
}

fn is_arabic(text: &str) -> bool {
    let mut arabic = 0;
    let mut other = 0;
    for ch in text.chars() {
        if ch.script() == Script::Arabic {
            arabic += 1;
        } else if !ch.is_ascii_digit()
            && !ruby_whitespace(ch)
            && !matches!(
                get_general_category(ch),
                GeneralCategory::ConnectorPunctuation
                    | GeneralCategory::DashPunctuation
                    | GeneralCategory::ClosePunctuation
                    | GeneralCategory::FinalPunctuation
                    | GeneralCategory::InitialPunctuation
                    | GeneralCategory::OpenPunctuation
                    | GeneralCategory::OtherPunctuation
            )
        {
            other += 1;
        }
    }
    arabic >= other
}

fn ruby_whitespace(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{0b}' | '\u{0c}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruby_shortest_pair_and_single_line_cases() {
        let mut lines = ["aaaaa", "bbb", "cccccccccccccc", "d"]
            .map(str::to_owned)
            .to_vec();
        merge_shortest(&mut lines);
        assert_eq!(lines, ["aaaaa bbb", "cccccccccccccc", "d"]);
        let mut single = vec!["Just one line".to_owned()];
        merge_shortest(&mut single);
        assert_eq!(single, ["Just one line"]);
    }
}
