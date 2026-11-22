//! The input box: multi-line text, cursor editing and history.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;

#[derive(Debug, Default, Clone)]
pub struct Composer {
    text: String,
    /// Byte index into `text`, always on a character boundary.
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    draft: String,
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    #[cfg(test)]
    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.history_index = None;
        self.draft.clear();
    }

    /// Take the submitted text and remember it in the history.
    pub fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.history_index = None;
        self.draft.clear();
        if !text.trim().is_empty() {
            self.history.push(text.clone());
        }
        text
    }

    pub fn insert_char(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.edited();
    }

    pub fn insert_str(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.edited();
    }

    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub fn backspace(&mut self) {
        if let Some(previous) = self.prev_boundary() {
            self.text.remove(previous);
            self.cursor = previous;
            self.edited();
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
            self.edited();
        }
    }

    pub fn left(&mut self) {
        if let Some(previous) = self.prev_boundary() {
            self.cursor = previous;
        }
    }

    pub fn right(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.cursor = next;
        }
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }

    /// Up: move a line when there is one above, else recall older history.
    pub fn up(&mut self) {
        let before = &self.text[..self.cursor];
        if let Some(newline) = before.rfind('\n') {
            let line_start = before[..newline].rfind('\n').map_or(0, |at| at + 1);
            let column = before[newline + 1..].chars().count();
            let line = &self.text[line_start..newline];
            self.cursor = line_start + byte_at_column(line, column);
            self.edited();
        } else {
            self.history_previous();
        }
    }

    /// Down: move a line when there is one below, else recall newer history.
    pub fn down(&mut self) {
        let after = &self.text[self.cursor..];
        if let Some(newline) = after.find('\n') {
            let line_start = self.cursor + newline + 1;
            let line_end = self.text[line_start..]
                .find('\n')
                .map_or(self.text.len(), |at| line_start + at);
            let column = after[..newline].chars().count();
            let line = &self.text[line_start..line_end];
            self.cursor = line_start + byte_at_column(line, column);
            self.edited();
        } else {
            self.history_next();
        }
    }

    /// Ctrl-U: delete from the cursor back to the start of the line.
    pub fn kill_to_start(&mut self) {
        let start = self.line_start();
        if start < self.cursor {
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
            self.edited();
        }
    }

    /// Ctrl-W: delete the previous word.
    pub fn delete_word(&mut self) {
        let mut start = self.cursor;
        while start > 0 {
            let ch = self.text[..start].chars().next_back().unwrap();
            if !ch.is_whitespace() {
                break;
            }
            start -= ch.len_utf8();
        }
        while start > 0 {
            let ch = self.text[..start].chars().next_back().unwrap();
            if ch.is_whitespace() {
                break;
            }
            start -= ch.len_utf8();
        }
        if start < self.cursor {
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
            self.edited();
        }
    }

    pub fn lines(&self) -> Vec<&str> {
        self.text.split('\n').collect()
    }

    /// Zero-based (row, column), both in characters.
    pub fn cursor_row_col(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let row = before.matches('\n').count();
        let column = before
            .rsplit('\n')
            .next()
            .map_or(0, |line| line.chars().count());
        (row, column)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                " input ",
                Style::default().add_modifier(Modifier::DIM),
            ));
        frame.render_widget(block.clone(), area);
        let inner = block.inner(area);
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let (row, column) = self.cursor_row_col();
        let rows = inner.height as usize;
        let start = row.saturating_sub(rows.saturating_sub(1));
        let width = inner.width as usize;
        let offset = column.saturating_sub(width.saturating_sub(1));

        let lines = self.lines();
        let visible: Vec<ratatui::text::Line> = lines
            .iter()
            .skip(start)
            .take(rows)
            .map(|line| ratatui::text::Line::from(line.chars().skip(offset).collect::<String>()))
            .collect();
        frame.render_widget(ratatui::widgets::Paragraph::new(visible), inner);

        let x = inner.x + (column - offset) as u16;
        let y = inner.y + (row - start) as u16;
        frame.set_cursor_position((x.min(inner.right().saturating_sub(1)), y));
    }

    fn edited(&mut self) {
        self.history_index = None;
        self.draft.clear();
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|ch| self.cursor + ch.len_utf8())
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |at| at + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |at| self.cursor + at)
    }

    fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            None => {
                self.draft = self.text.clone();
                self.history.len() - 1
            }
            Some(0) => return,
            Some(index) => index - 1,
        };
        self.history_index = Some(index);
        self.text = self.history[index].clone();
        self.cursor = self.text.len();
    }

    fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            self.text = self.history[index + 1].clone();
        } else {
            self.history_index = None;
            self.text = std::mem::take(&mut self.draft);
        }
        self.cursor = self.text.len();
    }
}

fn byte_at_column(line: &str, column: usize) -> usize {
    line.char_indices()
        .nth(column)
        .map_or(line.len(), |(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_and_newlines_keep_the_cursor_after_the_text() {
        let mut composer = Composer::new();
        composer.insert_str("hello");
        composer.insert_newline();
        composer.insert_str("world");
        assert_eq!(composer.text(), "hello\nworld");
        assert_eq!(composer.cursor_row_col(), (1, 5));
        assert_eq!(composer.lines(), ["hello", "world"]);
    }

    #[test]
    fn cursor_editing_works_across_lines() {
        let mut composer = Composer::new();
        composer.insert_str("ab\ncd");
        composer.home();
        assert_eq!(composer.cursor_row_col(), (1, 0));
        composer.insert_char('X');
        composer.end();
        composer.insert_char('Y');
        composer.left();
        composer.delete();
        assert_eq!(composer.text(), "ab\nXcd");

        composer.up();
        assert_eq!(composer.cursor_row_col(), (0, 2));
        composer.delete_word();
        assert_eq!(composer.text(), "\nXcd");
        composer.delete();
        assert_eq!(composer.text(), "Xcd");
        composer.home();
        composer.kill_to_start();
        assert_eq!(composer.text(), "Xcd");
    }

    #[test]
    fn history_round_trips_and_keeps_the_draft() {
        let mut composer = Composer::new();
        composer.insert_str("first");
        assert_eq!(composer.take(), "first");
        composer.insert_str("second");
        assert_eq!(composer.take(), "second");

        composer.insert_str("draft");
        composer.up();
        assert_eq!(composer.text(), "second");
        composer.up();
        assert_eq!(composer.text(), "first");
        composer.up();
        assert_eq!(composer.text(), "first");
        composer.down();
        assert_eq!(composer.text(), "second");
        composer.down();
        assert_eq!(composer.text(), "draft");

        composer.set_text("changed");
        composer.up();
        assert_eq!(composer.text(), "second");
        composer.insert_char('!');
        composer.up();
        assert_eq!(composer.text(), "second");
        composer.down();
        assert_eq!(composer.text(), "second!");
        composer.down();
        assert_eq!(composer.text(), "second!");
    }

    #[test]
    fn take_ignores_blank_history_entries() {
        let mut composer = Composer::new();
        composer.insert_str("   ");
        assert_eq!(composer.take(), "   ");
        assert!(composer.history().is_empty());
        composer.insert_str("real");
        composer.take();
        assert_eq!(composer.history(), ["real"]);
    }
}
