use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HeaderTone {
    Plain,
    Muted,
    Label,
    Key,
    Cyan,
    Green,
    Amber,
}

impl HeaderTone {
    fn ansi(self) -> &'static str {
        match self {
            Self::Plain => "\x1b[0m",
            Self::Muted => "\x1b[2;37m",
            Self::Label => "\x1b[1;90m",
            Self::Key | Self::Cyan => "\x1b[1;36m",
            Self::Green => "\x1b[1;32m",
            Self::Amber => "\x1b[1;33m",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HeaderSpan {
    text: String,
    tone: HeaderTone,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct HeaderLine {
    spans: Vec<HeaderSpan>,
}

impl HeaderLine {
    fn push(&mut self, text: impl Into<String>, tone: HeaderTone) {
        self.spans.push(HeaderSpan {
            text: text.into(),
            tone,
        });
    }

    fn append(&mut self, mut other: Self) {
        self.spans.append(&mut other.spans);
    }

    #[cfg(test)]
    fn plain(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }

    fn width(&self) -> usize {
        self.spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.text.as_str()))
            .sum()
    }

    fn render(&self, width: usize, pad: bool) -> String {
        let mut rendered = String::new();
        let mut used = 0;
        for span in &self.spans {
            let mut visible = String::new();
            for character in span.text.chars() {
                let cells = UnicodeWidthChar::width(character).unwrap_or(0);
                if used + cells > width {
                    break;
                }
                visible.push(character);
                used += cells;
            }
            if !visible.is_empty() {
                rendered.push_str(span.tone.ansi());
                rendered.push_str(&visible);
            }
            if used >= width {
                break;
            }
        }
        rendered.push_str("\x1b[0m");
        if pad {
            rendered.extend(std::iter::repeat_n(' ', width.saturating_sub(used)));
        }
        rendered
    }
}

#[derive(Clone, Debug)]
struct HeaderLayout {
    lines: Vec<HeaderLine>,
    too_small: bool,
}
