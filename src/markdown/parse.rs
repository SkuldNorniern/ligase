const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_BLOCKS: usize = 20_000;
const MAX_INLINE_DEPTH: usize = 16;
const MAX_QUOTE_DEPTH: usize = 8;
const MAX_ITEM_DEPTH: usize = 8;
const MAX_COLS: usize = 64;
const MAX_ROWS: usize = 2_000;

/// a run of inline text with its style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
    /// the link target when this span is part of a link.
    pub link: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub strong: bool,
    pub emph: bool,
    pub code: bool,
    pub strike: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// level 1-6.
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph(Vec<Span>),
    /// fenced or indented code; `lang` is the info string's first word.
    Code {
        lang: String,
        lines: Vec<String>,
    },
    /// `>` lines; inner blocks.
    Quote(Vec<Block>),
    /// one list item. `depth` from 0 by indent (2+ spaces or a tab per level, cap 8);
    /// `marker` is None for bullets, Some(n) for ordered; `task` is Some(checked) for `- [ ]`/`- [x]`.
    Item {
        depth: u8,
        marker: Option<u32>,
        task: Option<bool>,
        spans: Vec<Span>,
    },
    Rule,
    /// a GFM pipe table: header cells, then rows; alignment ignored.
    Table {
        head: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
    /// `![alt](src)` standing alone on a line.
    Image {
        alt: String,
        src: String,
    },
}

/// each block and the 0-based source line it starts on.
pub fn parse(text: &str) -> Vec<(usize, Block)> {
    let text = cut(text, MAX_BYTES);
    let lines: Vec<(usize, &str)> = text.lines().enumerate().collect();
    let mut budget = MAX_BLOCKS;
    blocks(&lines, 0, &mut budget)
}

/// inline markdown to spans.
pub fn inline(text: &str) -> Vec<Span> {
    let src = cut(text, MAX_BYTES);
    let mut cx = Cx {
        src,
        budget: src.len() * 8 + 1024,
    };
    let mut out = Vec::new();
    cx.run(0, src.len(), Style::default(), None, 0, &mut out);
    out
}

fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

// block level

/// parses one level of lines. `budget` counts every block made, nested ones too.
fn blocks<L: AsRef<str>>(
    lines: &[(usize, L)],
    depth: usize,
    budget: &mut usize,
) -> Vec<(usize, Block)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let raw: &str = lines[i].1.as_ref();
        let t = raw.trim_start();
        if t.is_empty() {
            i += 1;
            continue;
        }
        if *budget == 0 {
            break;
        }
        *budget -= 1;

        let no = lines[i].0;
        let (block, next) = if lead_cols(raw, 4) >= 4 && list_marker(t).is_none() {
            indented_code(lines, i)
        } else if let Some((ch, n, info)) = fence_open(t) {
            fence(lines, i, ch, n, info)
        } else if let Some((level, content)) = atx(t) {
            (
                Block::Heading {
                    level,
                    spans: inline(content),
                },
                i + 1,
            )
        } else if is_rule(t) {
            (Block::Rule, i + 1)
        } else if depth < MAX_QUOTE_DEPTH && t.starts_with('>') {
            quote(lines, i, depth, budget)
        } else if let Some(cols) = table_cols(lines, i) {
            table(lines, i, cols)
        } else if list_marker(t).is_some() {
            item(lines, i)
        } else if let Some((alt, src)) = image_line(t) {
            (
                Block::Image {
                    alt: plain(&inline(alt)),
                    src: src.to_string(),
                },
                i + 1,
            )
        } else {
            paragraph(lines, i, depth)
        };
        out.push((no, block));
        i = next;
    }
    out
}

fn indented_code<L: AsRef<str>>(lines: &[(usize, L)], i: usize) -> (Block, usize) {
    let mut end = i;
    let mut j = i;
    while j < lines.len() {
        let raw: &str = lines[j].1.as_ref();
        j += 1;
        if raw.trim().is_empty() {
            continue;
        }
        if lead_cols(raw, 4) < 4 {
            break;
        }
        end = j;
    }
    let code: Vec<String> = lines[i..end]
        .iter()
        .map(|(_, l)| {
            let l: &str = l.as_ref();
            if l.trim().is_empty() {
                String::new()
            } else {
                strip_code_indent(l)
            }
        })
        .collect();
    (
        Block::Code {
            lang: String::new(),
            lines: code,
        },
        end,
    )
}

fn strip_code_indent(line: &str) -> String {
    let rest = match line.strip_prefix('\t') {
        Some(r) => r,
        None => {
            let n = line.bytes().take(4).take_while(|&b| b == b' ').count();
            &line[n..]
        }
    };
    rest.replace('\t', "    ")
}

fn fence<L: AsRef<str>>(
    lines: &[(usize, L)],
    i: usize,
    c: u8,
    n: usize,
    info: &str,
) -> (Block, usize) {
    let lang = info.split_whitespace().next().unwrap_or("").to_string();
    let mut code = Vec::new();
    let mut j = i + 1;
    while j < lines.len() {
        let raw: &str = lines[j].1.as_ref();
        j += 1;
        if is_fence_close(raw.trim(), c, n) {
            break;
        }
        code.push(raw.replace('\t', "    "));
    }
    (Block::Code { lang, lines: code }, j)
}

fn fence_open(t: &str) -> Option<(u8, usize, &str)> {
    let c = *t.as_bytes().first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let n = t.bytes().take_while(|&b| b == c).count();
    if n < 3 {
        return None;
    }
    let info = t[n..].trim();
    if c == b'`' && info.contains('`') {
        return None;
    }
    Some((c, n, info))
}

fn is_fence_close(t: &str, c: u8, n: usize) -> bool {
    let k = t.bytes().take_while(|&b| b == c).count();
    k >= n && t[k..].trim().is_empty()
}

fn atx(t: &str) -> Option<(u8, &str)> {
    let n = t.bytes().take_while(|&b| b == b'#').count();
    if n == 0 || n > 6 {
        return None;
    }
    let rest = &t[n..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let content = rest.trim();
    let open = content.trim_end_matches('#');
    let content = if open.len() < content.len() && (open.is_empty() || open.ends_with([' ', '\t']))
    {
        open.trim_end()
    } else {
        content
    };
    Some((n as u8, content))
}

fn is_rule(t: &str) -> bool {
    let mut marker = None;
    let mut count = 0;
    for c in t.chars() {
        match c {
            ' ' | '\t' => {}
            '-' | '*' | '_' if marker.is_none() || marker == Some(c) => {
                marker = Some(c);
                count += 1;
            }
            _ => return false,
        }
    }
    count >= 3
}

/// setext underline level, if `t` is one.
fn setext_level(t: &str) -> Option<u8> {
    let t = t.trim();
    if t.is_empty() {
        None
    } else if t.bytes().all(|b| b == b'=') {
        Some(1)
    } else if t.bytes().all(|b| b == b'-') {
        Some(2)
    } else {
        None
    }
}

/// width of the leading whitespace; a tab counts as `tab` columns.
fn lead_cols(s: &str, tab: usize) -> usize {
    s.bytes()
        .take_while(|&b| b == b' ' || b == b'\t')
        .map(|b| if b == b'\t' { tab } else { 1 })
        .sum()
}

fn quote<L: AsRef<str>>(
    lines: &[(usize, L)],
    i: usize,
    depth: usize,
    budget: &mut usize,
) -> (Block, usize) {
    let mut inner: Vec<(usize, String)> = Vec::new();
    let mut j = i;
    while j < lines.len() {
        let raw: &str = lines[j].1.as_ref();
        let Some(rest) = raw.trim_start().strip_prefix('>') else {
            break;
        };
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        inner.push((lines[j].0, rest.to_string()));
        j += 1;
    }
    let body: Vec<Block> = blocks(&inner, depth + 1, budget)
        .into_iter()
        .map(|(_, b)| b)
        .collect();
    (Block::Quote(body), j)
}

/// a list marker: (ordered number, text after the marker).
fn list_marker(t: &str) -> Option<(Option<u32>, &str)> {
    let b = t.as_bytes();
    let first = *b.first()?;
    if matches!(first, b'-' | b'*' | b'+') && (b.len() == 1 || matches!(b[1], b' ' | b'\t')) {
        return Some((None, &t[1..]));
    }
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if !(1..=9).contains(&digits) || !matches!(b.get(digits), Some(b'.' | b')')) {
        return None;
    }
    let after = digits + 1;
    if b.len() == after || matches!(b[after], b' ' | b'\t') {
        let n: u32 = t[..digits].parse().ok()?;
        return Some((Some(n), &t[after..]));
    }
    None
}

/// `[ ]`, `[x]` or `[X]` at the start of an item body.
fn strip_task(s: &str) -> Option<(bool, &str)> {
    let b = s.as_bytes();
    if b.len() < 3 || b[0] != b'[' || b[2] != b']' {
        return None;
    }
    let checked = match b[1] {
        b' ' => false,
        b'x' | b'X' => true,
        _ => return None,
    };
    let rest = &s[3..];
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some((checked, rest.trim()))
}

fn item<L: AsRef<str>>(lines: &[(usize, L)], i: usize) -> (Block, usize) {
    let raw: &str = lines[i].1.as_ref();
    let depth = (lead_cols(raw, 2) / 2).min(MAX_ITEM_DEPTH) as u8;
    let (marker, rest) = list_marker(raw.trim_start()).unwrap_or((None, ""));
    let (task, body) = match strip_task(rest.trim()) {
        Some((checked, body)) => (Some(checked), body),
        None => (None, rest.trim()),
    };
    let mut text = body.to_string();
    let mut j = i + 1;
    while j < lines.len() {
        let next: &str = lines[j].1.as_ref();
        if next.trim().is_empty()
            || lead_cols(next, 2) < 2
            || list_marker(next.trim_start()).is_some()
        {
            break;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(next.trim());
        j += 1;
    }
    (
        Block::Item {
            depth,
            marker,
            task,
            spans: inline(&text),
        },
        j,
    )
}

/// splits a row on unescaped pipes. outer pipes are dropped.
fn split_cells(line: &str) -> Vec<&str> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = match t.strip_suffix('|') {
        Some(s) if !s.ends_with('\\') => s,
        _ => t,
    };
    let mut cells = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (idx, ch) in t.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '|' => {
                cells.push(t[start..idx].trim());
                start = idx + 1;
            }
            _ => {}
        }
    }
    cells.push(t[start..].trim());
    cells
}

/// column count of a delimiter row like `|---|:--:|`.
fn delim_cols(line: &str) -> Option<usize> {
    if !line.contains('|') {
        return None;
    }
    let cells = split_cells(line);
    let ok = cells.iter().all(|c| {
        let core = c.trim_matches(':');
        !core.is_empty() && core.bytes().all(|b| b == b'-')
    });
    ok.then_some(cells.len())
}

/// column count when line `i` is a table header with a matching delimiter row.
fn table_cols<L: AsRef<str>>(lines: &[(usize, L)], i: usize) -> Option<usize> {
    let head: &str = lines.get(i)?.1.as_ref();
    let next: &str = lines.get(i + 1)?.1.as_ref();
    let head = head.trim();
    if !head.contains('|') {
        return None;
    }
    let n = delim_cols(next.trim())?;
    (n <= MAX_COLS && split_cells(head).len() == n).then_some(n)
}

fn table<L: AsRef<str>>(lines: &[(usize, L)], i: usize, cols: usize) -> (Block, usize) {
    let head_raw: &str = lines[i].1.as_ref();
    let head: Vec<Vec<Span>> = split_cells(head_raw).into_iter().map(inline).collect();
    let mut rows: Vec<Vec<Vec<Span>>> = Vec::new();
    let mut j = i + 2;
    while j < lines.len() && rows.len() < MAX_ROWS {
        let raw: &str = lines[j].1.as_ref();
        let raw = raw.trim();
        if raw.is_empty() || !raw.contains('|') {
            break;
        }
        let mut row: Vec<Vec<Span>> = split_cells(raw)
            .into_iter()
            .take(cols)
            .map(inline)
            .collect();
        row.resize_with(cols, Vec::new);
        rows.push(row);
        j += 1;
    }
    (Block::Table { head, rows }, j)
}

/// `![alt](src)` alone on a line. the title is dropped.
fn image_line(t: &str) -> Option<(&str, &str)> {
    let rest = t.trim().strip_prefix("![")?;
    let close = rest.find(']')?;
    let inner = rest[close + 1..].strip_prefix('(')?.strip_suffix(')')?;
    Some((&rest[..close], destination(inner)?))
}

/// the text of a line with its hard break flag. a trailing `\` or two spaces is a break.
fn split_break(line: &str) -> (&str, bool) {
    let t = line.trim_start();
    if let Some(s) = t.strip_suffix('\\') {
        return (s.trim_end(), true);
    }
    if t.ends_with("  ") {
        return (t.trim_end(), true);
    }
    (t.trim_end(), false)
}

fn paragraph<L: AsRef<str>>(lines: &[(usize, L)], i: usize, depth: usize) -> (Block, usize) {
    let mut text = String::new();
    let mut level = None;
    let mut hard = false;
    let mut j = i;
    loop {
        if j > i {
            text.push(if hard { '\n' } else { ' ' });
        }
        let raw: &str = lines[j].1.as_ref();
        let (content, h) = split_break(raw);
        text.push_str(content);
        hard = h;
        j += 1;
        if j >= lines.len() {
            break;
        }
        let next: &str = lines[j].1.as_ref();
        let next = next.trim_start();
        if next.is_empty() {
            break;
        }
        if let Some(l) = setext_level(next) {
            level = Some(l);
            j += 1;
            break;
        }
        if interrupts(lines, j, depth) {
            break;
        }
    }
    let spans = inline(&text);
    match level {
        Some(level) => (Block::Heading { level, spans }, j),
        None => (Block::Paragraph(spans), j),
    }
}

/// lines that end a paragraph without being part of it.
fn interrupts<L: AsRef<str>>(lines: &[(usize, L)], j: usize, depth: usize) -> bool {
    let raw: &str = lines[j].1.as_ref();
    let t = raw.trim_start();
    atx(t).is_some()
        || fence_open(t).is_some()
        || is_rule(t)
        || (depth < MAX_QUOTE_DEPTH && t.starts_with('>'))
        || table_cols(lines, j).is_some()
        || matches!(list_marker(t), Some((None, _)) | Some((Some(1), _)))
}

// inline level

struct Cx<'a> {
    src: &'a str,
    budget: usize,
}

impl<'a> Cx<'a> {
    fn spend(&mut self, n: usize) {
        self.budget = self.budget.saturating_sub(n);
    }

    /// run of `c` from `i`, not past `end`.
    fn run_len(&mut self, i: usize, end: usize, c: u8) -> usize {
        let src = self.src;
        let n = src.as_bytes()[i..end]
            .iter()
            .take_while(|&&b| b == c)
            .count();
        self.spend(n + 1);
        n
    }

    fn prev_char(&self, i: usize) -> Option<char> {
        self.src[..i].chars().next_back()
    }

    fn next_char(&self, i: usize) -> Option<char> {
        self.src[i..].chars().next()
    }

    fn can_open(&self, i: usize, m: usize, c: u8) -> bool {
        let next_ok = self.next_char(i + m).is_some_and(|ch| !ch.is_whitespace());
        let prev_ok = c != b'_' || !self.prev_char(i).is_some_and(char::is_alphanumeric);
        next_ok && prev_ok
    }

    fn can_close(&self, j: usize, m: usize, c: u8) -> bool {
        let prev_ok = self.prev_char(j).is_some_and(|ch| !ch.is_whitespace());
        let next_ok = c != b'_' || !self.next_char(j + m).is_some_and(char::is_alphanumeric);
        prev_ok && next_ok
    }

    /// start of the next backtick run of exactly `n`.
    fn code_end(&mut self, from: usize, end: usize, n: usize) -> Option<usize> {
        let src = self.src;
        let b = src.as_bytes();
        let mut j = from;
        while j < end && self.budget > 0 {
            if b[j] == b'`' {
                let m = self.run_len(j, end, b'`');
                if m == n {
                    return Some(j);
                }
                j += m;
            } else {
                self.spend(1);
                j += 1;
            }
        }
        None
    }

    /// start of the next run of `c` that can close an opener of length `m`.
    fn find_closer(&mut self, from: usize, end: usize, c: u8, m: usize) -> Option<usize> {
        let src = self.src;
        let b = src.as_bytes();
        let mut j = from;
        while j < end && self.budget > 0 {
            let x = b[j];
            if x == b'\\' && j + 1 < end && b[j + 1].is_ascii_punctuation() {
                self.spend(1);
                j += 2;
            } else if x == b'`' {
                let n = self.run_len(j, end, b'`');
                j = match self.code_end(j + n, end, n) {
                    Some(k) => k + n,
                    None => j + n,
                };
            } else if x == c {
                let r = self.run_len(j, end, c);
                if r == m && self.can_close(j, r, c) {
                    return Some(j);
                }
                j += r;
            } else {
                self.spend(1);
                j += 1;
            }
        }
        None
    }

    /// matching close for an open at `from - 1`. escapes are skipped.
    fn match_close(&mut self, from: usize, end: usize, open: u8, close: u8) -> Option<usize> {
        let src = self.src;
        let b = src.as_bytes();
        let mut depth = 1usize;
        let mut j = from;
        while j < end && self.budget > 0 {
            self.spend(1);
            let x = b[j];
            if x == b'\\' && j + 1 < end && b[j + 1].is_ascii_punctuation() {
                j += 2;
                continue;
            }
            if x == open {
                depth += 1;
            } else if x == close {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            j += 1;
        }
        None
    }

    /// `[text](dest)` with the `[` at `open`. gives the `]`, the `)` and the dest.
    fn bracketed(&mut self, open: usize, end: usize) -> Option<(usize, usize, &'a str)> {
        let src = self.src;
        let b = src.as_bytes();
        let close = self.match_close(open + 1, end, b'[', b']')?;
        if close + 1 >= end || b[close + 1] != b'(' {
            return None;
        }
        let p = self.match_close(close + 2, end, b'(', b')')?;
        let dest = destination(&src[close + 2..p])?;
        Some((close, p, dest))
    }

    fn bare_start(&self, i: usize) -> bool {
        let rest = &self.src[i..];
        (rest.starts_with("http://") || rest.starts_with("https://"))
            && !self.prev_char(i).is_some_and(char::is_alphanumeric)
    }

    fn run(
        &mut self,
        start: usize,
        end: usize,
        style: Style,
        link: Option<&'a str>,
        depth: usize,
        out: &mut Vec<Span>,
    ) {
        let src = self.src;
        let b = src.as_bytes();
        let mut lit = start;
        let mut i = start;
        while i < end && self.budget > 0 {
            self.spend(1);
            match b[i] {
                b'\\' if i + 1 < end && b[i + 1].is_ascii_punctuation() => {
                    push(out, &src[lit..i], style, link);
                    push(out, &src[i + 1..i + 2], style, link);
                    i += 2;
                    lit = i;
                }
                b'`' => {
                    let n = self.run_len(i, end, b'`');
                    match self.code_end(i + n, end, n) {
                        Some(k) => {
                            push(out, &src[lit..i], style, link);
                            let code = code_content(&src[i + n..k]);
                            push(
                                out,
                                code,
                                Style {
                                    code: true,
                                    ..style
                                },
                                link,
                            );
                            i = k + n;
                            lit = i;
                        }
                        None => i += n,
                    }
                }
                c @ (b'*' | b'_' | b'~') => {
                    let m = self.run_len(i, end, c);
                    let st = if depth < MAX_INLINE_DEPTH && self.can_open(i, m, c) {
                        emph_style(c, m, style)
                    } else {
                        None
                    };
                    let closer = match st {
                        Some(_) => self.find_closer(i + m, end, c, m),
                        None => None,
                    };
                    match (st, closer) {
                        (Some(st), Some(j)) => {
                            push(out, &src[lit..i], style, link);
                            self.run(i + m, j, st, link, depth + 1, out);
                            i = j + m;
                            lit = i;
                        }
                        _ => i += m,
                    }
                }
                b'[' if link.is_none() && depth < MAX_INLINE_DEPTH => {
                    match self.bracketed(i, end) {
                        Some((close, p, dest)) => {
                            push(out, &src[lit..i], style, link);
                            self.run(i + 1, close, style, Some(dest), depth + 1, out);
                            i = p + 1;
                            lit = i;
                        }
                        None => i += 1,
                    }
                }
                b'!' if i + 1 < end && b[i + 1] == b'[' && depth < MAX_INLINE_DEPTH => {
                    match self.bracketed(i + 1, end) {
                        Some((close, p, dest)) => {
                            push(out, &src[lit..i], style, link);
                            let mut alt = Vec::new();
                            self.run(i + 2, close, Style::default(), None, depth + 1, &mut alt);
                            let text = plain(&alt);
                            push(
                                out,
                                &text,
                                Style {
                                    emph: true,
                                    ..style
                                },
                                Some(dest),
                            );
                            i = p + 1;
                            lit = i;
                        }
                        None => i += 1,
                    }
                }
                b'<' => {
                    let mut j = i + 1;
                    while j < end && !matches!(b[j], b'<' | b'>') && !b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    self.spend(j - i);
                    if j < end && b[j] == b'>' && is_web(&src[i + 1..j]) {
                        push(out, &src[lit..i], style, link);
                        let url = &src[i + 1..j];
                        push(out, url, style, Some(url));
                        i = j + 1;
                        lit = i;
                    } else {
                        i += 1;
                    }
                }
                b'h' if link.is_none() && self.bare_start(i) => {
                    let mut j = i;
                    while j < end && !b[j].is_ascii_whitespace() && b[j] != b'<' {
                        j += 1;
                    }
                    self.spend(j - i);
                    let mut e = j;
                    while e > i && matches!(b[e - 1], b'.' | b',' | b')') {
                        e -= 1;
                    }
                    if is_web(&src[i..e]) {
                        push(out, &src[lit..i], style, link);
                        push(out, &src[i..e], style, Some(&src[i..e]));
                        i = e;
                        lit = i;
                    } else {
                        i += 1;
                    }
                }
                _ => i += 1,
            }
        }
        push(out, &src[lit..end], style, link);
    }
}

/// appends text, merging into the last span when style and link match.
fn push(out: &mut Vec<Span>, text: &str, style: Style, link: Option<&str>) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = out
        .last_mut()
        .filter(|l| l.style == style && l.link.as_deref() == link)
    {
        last.text.push_str(text);
        return;
    }
    out.push(Span {
        text: text.to_string(),
        style,
        link: link.map(str::to_string),
    });
}

fn emph_style(c: u8, m: usize, style: Style) -> Option<Style> {
    match (c, m) {
        (b'~', 2) => Some(Style {
            strike: true,
            ..style
        }),
        (b'*' | b'_', 1) => Some(Style {
            emph: true,
            ..style
        }),
        (b'*' | b'_', 2) => Some(Style {
            strong: true,
            ..style
        }),
        (b'*' | b'_', 3) => Some(Style {
            strong: true,
            emph: true,
            ..style
        }),
        _ => None,
    }
}

/// one space off each side when both are there.
fn code_content(s: &str) -> &str {
    if s.len() >= 2 && s.starts_with(' ') && s.ends_with(' ') && !s.bytes().all(|b| b == b' ') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn is_web(s: &str) -> bool {
    (s.len() > 8 && s.starts_with("https://")) || (s.len() > 7 && s.starts_with("http://"))
}

/// link or image destination. `<...>` form allowed, title dropped.
fn destination(raw: &str) -> Option<&str> {
    let t = raw.trim();
    let d = match t.strip_prefix('<') {
        Some(rest) => &rest[..rest.find('>')?],
        None => t.split_whitespace().next()?,
    };
    (!d.is_empty()).then_some(d)
}

fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Style = Style {
        strong: false,
        emph: false,
        code: false,
        strike: false,
    };
    const STRONG: Style = Style {
        strong: true,
        ..PLAIN
    };
    const EMPH: Style = Style {
        emph: true,
        ..PLAIN
    };
    const CODE: Style = Style {
        code: true,
        ..PLAIN
    };
    const STRIKE: Style = Style {
        strike: true,
        ..PLAIN
    };

    fn sp(text: &str, style: Style) -> Span {
        Span {
            text: text.to_string(),
            style,
            link: None,
        }
    }

    fn lk(text: &str, style: Style, url: &str) -> Span {
        Span {
            text: text.to_string(),
            style,
            link: Some(url.to_string()),
        }
    }

    fn p(text: &str) -> Block {
        Block::Paragraph(vec![sp(text, PLAIN)])
    }

    fn h(level: u8, text: &str) -> Block {
        Block::Heading {
            level,
            spans: vec![sp(text, PLAIN)],
        }
    }

    fn code_b(lang: &str, lines: &[&str]) -> Block {
        Block::Code {
            lang: lang.to_string(),
            lines: lines.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn li(depth: u8, marker: Option<u32>, task: Option<bool>, text: &str) -> Block {
        Block::Item {
            depth,
            marker,
            task,
            spans: vec![sp(text, PLAIN)],
        }
    }

    #[test]
    fn headings() {
        let got = parse("# Title\n\n## Sub ##\n#tag\n\n####### seven");
        assert_eq!(
            got,
            vec![
                (0, h(1, "Title")),
                (2, h(2, "Sub")),
                (3, p("#tag")),
                (5, p("####### seven")),
            ]
        );
    }

    #[test]
    fn setext_and_rule() {
        let got = parse("Title\n===\n\nSub\n---\n\n---\n\n* * *\ntext\n***");
        assert_eq!(
            got,
            vec![
                (0, h(1, "Title")),
                (3, h(2, "Sub")),
                (6, Block::Rule),
                (8, Block::Rule),
                (9, p("text")),
                (10, Block::Rule),
            ]
        );
    }

    #[test]
    fn fences_close() {
        let got = parse("```rust\nlet a = 1;\n```\n\n~~~\n  x\n~~~~\n\n```\n\tx\n```");
        assert_eq!(
            got,
            vec![
                (0, code_b("rust", &["let a = 1;"])),
                (4, code_b("", &["  x"])),
                (8, code_b("", &["    x"])),
            ]
        );
    }

    #[test]
    fn fence_unclosed() {
        let got = parse("text\n\n~~~ go\nline1\n\nline2");
        assert_eq!(
            got,
            vec![(0, p("text")), (2, code_b("go", &["line1", "", "line2"]))]
        );
    }

    #[test]
    fn indented_code_block() {
        let got = parse("para\n\n    code\n    more\n\nafter");
        assert_eq!(
            got,
            vec![
                (0, p("para")),
                (2, code_b("", &["code", "more"])),
                (5, p("after")),
            ]
        );
    }

    #[test]
    fn nested_quotes() {
        let got = parse("> a\n> > b\n>\n> c");
        assert_eq!(
            got,
            vec![(
                0,
                Block::Quote(vec![p("a"), Block::Quote(vec![p("b")]), p("c"),])
            )]
        );
    }

    #[test]
    fn lists() {
        let got = parse("- a\n* b\n  cont\n    - deep\n3. three\n1) one\n- [ ] todo\n- [x] done");
        assert_eq!(
            got,
            vec![
                (0, li(0, None, None, "a")),
                (1, li(0, None, None, "b cont")),
                (3, li(2, None, None, "deep")),
                (4, li(0, Some(3), None, "three")),
                (5, li(0, Some(1), None, "one")),
                (6, li(0, None, Some(false), "todo")),
                (7, li(0, None, Some(true), "done")),
            ]
        );
    }

    #[test]
    fn tables() {
        let got =
            parse("| a | b |\n|---|:--:|\n| 1 | 2 |\n| x \\| y | z | extra |\n| only |\n\nafter");
        assert_eq!(
            got,
            vec![
                (
                    0,
                    Block::Table {
                        head: vec![vec![sp("a", PLAIN)], vec![sp("b", PLAIN)]],
                        rows: vec![
                            vec![vec![sp("1", PLAIN)], vec![sp("2", PLAIN)]],
                            vec![vec![sp("x | y", PLAIN)], vec![sp("z", PLAIN)]],
                            vec![vec![sp("only", PLAIN)], vec![]],
                        ],
                    }
                ),
                (6, p("after")),
            ]
        );
    }

    #[test]
    fn paragraph_joins_and_breaks() {
        let got = parse("one\ntwo  \nthree\\\nfour\n\na");
        assert_eq!(got, vec![(0, p("one two\nthree\nfour")), (5, p("a"))]);
    }

    #[test]
    fn image_block_and_html_text() {
        let got = parse("![logo](a/b.png)\n<div>\nhi</div>");
        assert_eq!(
            got,
            vec![
                (
                    0,
                    Block::Image {
                        alt: "logo".to_string(),
                        src: "a/b.png".to_string(),
                    }
                ),
                (1, p("<div> hi</div>")),
            ]
        );
    }

    #[test]
    fn inline_styles() {
        assert_eq!(
            inline("**bold** and *em* and ***both***"),
            vec![
                sp("bold", STRONG),
                sp(" and ", PLAIN),
                sp("em", EMPH),
                sp(" and ", PLAIN),
                sp(
                    "both",
                    Style {
                        strong: true,
                        emph: true,
                        ..PLAIN
                    }
                ),
            ]
        );
        assert_eq!(
            inline("_em_ and __strong__"),
            vec![sp("em", EMPH), sp(" and ", PLAIN), sp("strong", STRONG),]
        );
        assert_eq!(
            inline("snake_case_name"),
            vec![sp("snake_case_name", PLAIN)]
        );
        assert_eq!(
            inline("~~gone~~ ok"),
            vec![sp("gone", STRIKE), sp(" ok", PLAIN)]
        );
    }

    #[test]
    fn inline_code_spans() {
        assert_eq!(
            inline("use ``a`b`` here"),
            vec![sp("use ", PLAIN), sp("a`b", CODE), sp(" here", PLAIN)]
        );
        assert_eq!(inline("`*not em*`"), vec![sp("*not em*", CODE)]);
    }

    #[test]
    fn inline_links() {
        assert_eq!(
            inline("[**bold** link](http://x.y)"),
            vec![
                lk("bold", STRONG, "http://x.y"),
                lk(" link", PLAIN, "http://x.y"),
            ]
        );
        assert_eq!(
            inline("[t](http://x \"title\")"),
            vec![lk("t", PLAIN, "http://x")]
        );
        assert_eq!(
            inline("<https://example.com>"),
            vec![lk("https://example.com", PLAIN, "https://example.com")]
        );
        assert_eq!(
            inline("see https://a.b/c."),
            vec![
                sp("see ", PLAIN),
                lk("https://a.b/c", PLAIN, "https://a.b/c"),
                sp(".", PLAIN),
            ]
        );
        assert_eq!(
            inline("![alt text](img.png)"),
            vec![lk("alt text", EMPH, "img.png")]
        );
    }

    #[test]
    fn inline_escapes_and_literals() {
        assert_eq!(inline("\\*not em\\*"), vec![sp("*not em*", PLAIN)]);
        assert_eq!(inline("a * b and _ c"), vec![sp("a * b and _ c", PLAIN)]);
        assert_eq!(inline("[open only"), vec![sp("[open only", PLAIN)]);
    }

    #[test]
    fn line_numbers_follow_source() {
        let got = parse("\n\n# x\n\n- y");
        assert_eq!(got, vec![(2, h(1, "x")), (4, li(0, None, None, "y"))]);
    }

    #[test]
    fn block_cap() {
        let got = parse(&"- x\n".repeat(20_005));
        assert_eq!(got.len(), MAX_BLOCKS);
    }

    #[test]
    fn cut_on_char_boundary() {
        let text = "가".repeat(900_000);
        let got = parse(&text);
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn fuzz_no_panic() {
        const CHARS: &[char] = &[
            '*', '_', '~', '`', '[', ']', '(', ')', '!', '#', '>', '-', '|', '1', '.', 'a', '가',
            '\n', ' ', '\\', '<', ':', '=', '\t',
        ];
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..3000 {
            let len = (next() % 200) as usize;
            let s: String = (0..len)
                .map(|_| CHARS[(next() % CHARS.len() as u64) as usize])
                .collect();
            let _ = parse(&s);
            let _ = inline(&s);
        }
    }

    #[test]
    fn pathological_inputs_finish() {
        let s = "*[`".repeat(200_000 / 3);
        let _ = parse(&s);
        let _ = inline(&s);
        let _ = inline(&"[".repeat(200_000));
        let _ = inline(&"*".repeat(200_000));
        let _ = parse(&"> ".repeat(100_000));
        let _ = parse(&"|a".repeat(100_000));
    }
}
