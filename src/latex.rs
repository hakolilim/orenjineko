use std::collections::{HashMap, HashSet};
use std::ffi::{CString, c_char, c_int, c_uint, c_void};

const MAX_FORMULAS: usize = 12;
const MAX_LENGTH: usize = 2000;
const PADDING: f64 = 24.0;

pub struct Preview {
    pub image: Option<Vec<u8>>,
    pub rendered: usize,
    pub skipped: usize,
}

#[derive(Debug, PartialEq)]
struct Formula<'a> {
    tex: &'a str,
    display: bool,
}

fn escaped(text: &[u8], index: usize) -> bool {
    text[..index]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
        % 2
        == 1
}

fn extract<'a>(text: &'a str) -> Vec<Formula<'a>> {
    let bytes = text.as_bytes();
    let mut seen = HashSet::new();
    let mut formulas = Vec::new();
    let mut add = |tex: &'a str, display| {
        let tex = tex.trim();
        if !tex.is_empty() && seen.insert(tex) {
            formulas.push(Formula { tex, display });
        }
    };
    let mut at = 0;
    while at < text.len() {
        if !text.is_char_boundary(at) || escaped(bytes, at) {
            at += 1;
            continue;
        }
        let rest = &text[at..];
        let marker = bytes[at];
        if matches!(marker, 96 | b'~') {
            let count = rest.bytes().take_while(|byte| *byte == marker).count();
            let fence = &rest[..count];
            if count >= 3 {
                let Some(line) = rest.find('\n') else {
                    break;
                };
                let Some(end) = rest[line + 1..].find(fence) else {
                    break;
                };
                let language = rest[count..line].trim().to_ascii_lowercase();
                if matches!(language.as_str(), "math" | "latex") {
                    add(&rest[line + 1..line + 1 + end], true);
                }
                at += line + 1 + end + count;
                continue;
            }
            if marker == 96 {
                let Some(end) = rest[count..].find(fence) else {
                    break;
                };
                at += count + end + count;
                continue;
            }
        }
        let delimiter = [
            ("$$", "$$", true),
            ("\\[", "\\]", true),
            ("\\(", "\\)", false),
            ("$", "$", false),
        ]
        .into_iter()
        .find(|(opening, _, _)| rest.starts_with(opening));
        let Some((open, close, display)) = delimiter else {
            at += 1;
            continue;
        };
        let start = at + open.len();
        let end = text[start..]
            .match_indices(close)
            .map(|(offset, _)| start + offset)
            .find(|offset| !escaped(bytes, *offset));
        if let Some(end) = end {
            let body = &text[start..end];
            let looks_like_price = open == "$"
                && (body.starts_with(char::is_whitespace)
                    || body.ends_with(char::is_whitespace)
                    || body.contains('\n'));
            if !looks_like_price {
                add(body, display);
                at = end + close.len();
                continue;
            }
        }
        at = start;
    }
    formulas
}

#[repr(C)]
#[derive(Default)]
struct TextExtents {
    x_bearing: f64,
    y_bearing: f64,
    width: f64,
    height: f64,
    x_advance: f64,
    y_advance: f64,
}

#[link(name = "libcairo.so.2", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn cairo_image_surface_create(format: c_int, width: c_int, height: c_int) -> *mut c_void;
    fn cairo_surface_status(surface: *mut c_void) -> c_int;
    fn cairo_surface_destroy(surface: *mut c_void);
    fn cairo_create(surface: *mut c_void) -> *mut c_void;
    fn cairo_status(cr: *mut c_void) -> c_int;
    fn cairo_destroy(cr: *mut c_void);
    fn cairo_select_font_face(cr: *mut c_void, family: *const c_char, slant: c_int, weight: c_int);
    fn cairo_set_font_size(cr: *mut c_void, size: f64);
    fn cairo_text_extents(cr: *mut c_void, text: *const c_char, extents: *mut TextExtents);
    fn cairo_set_source_rgb(cr: *mut c_void, r: f64, g: f64, b: f64);
    fn cairo_paint(cr: *mut c_void);
    fn cairo_move_to(cr: *mut c_void, x: f64, y: f64);
    fn cairo_line_to(cr: *mut c_void, x: f64, y: f64);
    fn cairo_set_line_width(cr: *mut c_void, width: f64);
    fn cairo_stroke(cr: *mut c_void);
    fn cairo_show_text(cr: *mut c_void, text: *const c_char);
    fn cairo_save(cr: *mut c_void);
    fn cairo_restore(cr: *mut c_void);
    fn cairo_translate(cr: *mut c_void, x: f64, y: f64);
    fn cairo_scale(cr: *mut c_void, x: f64, y: f64);
    fn cairo_surface_write_to_png_stream(
        surface: *mut c_void,
        write: unsafe extern "C" fn(*mut c_void, *const u8, c_uint) -> c_int,
        closure: *mut c_void,
    ) -> c_int;
}

struct Canvas {
    surface: *mut c_void,
    context: *mut c_void,
}

impl Canvas {
    fn new(width: i32, height: i32) -> Result<Self, String> {
        // Cairo returns error objects that can be checked and destroyed like normal objects.
        let surface = unsafe { cairo_image_surface_create(0, width, height) };
        let context = unsafe { cairo_create(surface) };
        let canvas = Self { surface, context };
        if unsafe { cairo_status(context) != 0 || cairo_surface_status(surface) != 0 } {
            return Err("Không thể tạo ảnh LaTeX".into());
        }
        unsafe {
            cairo_select_font_face(context, c"DejaVu Sans".as_ptr(), 0, 0);
        }
        Ok(canvas)
    }

    fn glyph(&self, text: &str, size: f64) -> Layout {
        let Ok(text) = CString::new(text) else {
            return Layout::default();
        };
        let mut bounds = TextExtents::default();
        unsafe {
            cairo_set_font_size(self.context, size);
            cairo_text_extents(self.context, text.as_ptr(), &mut bounds);
        }
        let margin = (-bounds.x_bearing).max(0.0);
        Layout {
            width: (bounds.x_advance.max(bounds.width + bounds.x_bearing) + margin).max(0.0),
            above: (-bounds.y_bearing).max(size * 0.64),
            below: (bounds.y_bearing + bounds.height).max(size * 0.18),
            marks: vec![Mark::Text {
                text,
                size,
                x: margin,
                y: 0.0,
            }],
        }
    }

    fn draw(&self, layout: &Layout, x: f64, baseline: f64, scale: f64) {
        let cr = self.context;
        unsafe {
            cairo_save(cr);
            cairo_translate(cr, x, baseline);
            cairo_scale(cr, scale, scale);
            for mark in &layout.marks {
                match mark {
                    Mark::Text { text, size, x, y } => {
                        cairo_set_font_size(cr, *size);
                        cairo_move_to(cr, *x, *y);
                        cairo_show_text(cr, text.as_ptr());
                    }
                    Mark::Line {
                        x,
                        y,
                        end_x,
                        end_y,
                        width,
                    } => {
                        cairo_set_line_width(cr, *width);
                        cairo_move_to(cr, *x, *y);
                        cairo_line_to(cr, *end_x, *end_y);
                        cairo_stroke(cr);
                    }
                }
            }
            cairo_restore(cr);
        }
    }

    fn png(&self) -> Result<Vec<u8>, String> {
        let mut png = Vec::new();
        let status = unsafe {
            cairo_surface_write_to_png_stream(
                self.surface,
                write_png,
                (&mut png as *mut Vec<u8>).cast(),
            )
        };
        if status == 0 {
            Ok(png)
        } else {
            Err("Không ghi được ảnh PNG".into())
        }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            cairo_destroy(self.context);
            cairo_surface_destroy(self.surface);
        }
    }
}

unsafe extern "C" fn write_png(closure: *mut c_void, bytes: *const u8, length: c_uint) -> c_int {
    if length == 0 {
        return 0;
    }
    // The synchronous Cairo call provides the output buffer and a valid chunk.
    let output = unsafe { &mut *closure.cast::<Vec<u8>>() };
    if output.len() + length as usize > 8 * 1024 * 1024 {
        return 11;
    }
    output.extend_from_slice(unsafe { std::slice::from_raw_parts(bytes, length as usize) });
    0
}

enum Mark {
    Text {
        text: CString,
        size: f64,
        x: f64,
        y: f64,
    },
    Line {
        x: f64,
        y: f64,
        end_x: f64,
        end_y: f64,
        width: f64,
    },
}

#[derive(Default)]
struct Layout {
    width: f64,
    above: f64,
    below: f64,
    marks: Vec<Mark>,
}

impl Layout {
    fn add(&mut self, item: Layout, dx: f64, dy: f64) {
        self.width = self.width.max(dx + item.width);
        self.above = self.above.max(item.above - dy);
        self.below = self.below.max(item.below + dy);
        self.marks
            .extend(item.marks.into_iter().map(|mark| match mark {
                Mark::Text { text, size, x, y } => Mark::Text {
                    text,
                    size,
                    x: x + dx,
                    y: y + dy,
                },
                Mark::Line {
                    x,
                    y,
                    end_x,
                    end_y,
                    width,
                } => Mark::Line {
                    x: x + dx,
                    y: y + dy,
                    end_x: end_x + dx,
                    end_y: end_y + dy,
                    width,
                },
            }));
    }

    fn append(&mut self, item: Layout) {
        self.add(item, self.width, 0.0);
    }

    fn line(&mut self, x: f64, y: f64, end_x: f64, end_y: f64, size: f64) {
        self.width = self.width.max(x).max(end_x);
        self.above = self.above.max(-y).max(-end_y);
        self.below = self.below.max(y).max(end_y);
        self.marks.push(Mark::Line {
            x,
            y,
            end_x,
            end_y,
            width: (size * 0.055).max(0.7),
        });
    }
}

#[derive(Clone)]
struct Macro {
    parameters: u8,
    replacement: String,
}

struct Parser<'a> {
    text: &'a str,
    canvas: &'a Canvas,
    macros: &'a mut HashMap<String, Macro>,
    budget: &'a mut usize,
}

type Math<T> = Result<T, ()>;

impl Parser<'_> {
    fn parse(canvas: &Canvas, text: &str, size: f64) -> Math<Layout> {
        let mut macros = HashMap::new();
        let mut budget = 4000;
        let mut parser = Parser {
            text,
            canvas,
            macros: &mut macros,
            budget: &mut budget,
        };
        parser.row(size, 0, None)
    }

    fn peek(&self) -> Option<char> {
        self.text.chars().next()
    }

    fn take(&mut self) -> Option<char> {
        let item = self.peek()?;
        self.text = &self.text[item.len_utf8()..];
        Some(item)
    }

    fn space(&mut self) {
        self.text = self.text.trim_start();
    }

    fn step(&mut self, depth: u8) -> Math<()> {
        if *self.budget == 0 || depth > 32 {
            return Err(());
        }
        *self.budget -= 1;
        Ok(())
    }

    fn row(&mut self, size: f64, depth: u8, end: Option<char>) -> Math<Layout> {
        self.step(depth)?;
        let mut row = Layout::default();
        loop {
            self.space();
            if self.peek() == end {
                self.take();
                return Ok(row);
            }
            let mut atom = self.atom(size, depth + 1)?;
            self.space();
            let mut upper = None;
            let mut lower = None;
            while let Some(mark @ ('^' | '_')) = self.peek() {
                self.take();
                let script = self.atom(size * 0.68, depth + 1)?;
                let slot = if mark == '^' { &mut upper } else { &mut lower };
                if slot.replace(script).is_some() {
                    return Err(());
                }
                self.space();
            }
            let x = atom.width + size * 0.04;
            if let Some(script) = upper {
                let y = -(atom.above * 0.7 + script.below * 0.5);
                atom.add(script, x, y);
            }
            if let Some(script) = lower {
                let y = atom.below + script.above * 0.68;
                atom.add(script, x, y);
            }
            row.append(atom);
        }
    }

    fn atom(&mut self, size: f64, depth: u8) -> Math<Layout> {
        self.step(depth)?;
        self.space();
        match self.take().ok_or(())? {
            '{' => self.row(size, depth + 1, Some('}')),
            '}' | '^' | '_' | '&' | '#' | '$' | '\0' => Err(()),
            '\\' => self.command(size, depth + 1),
            '-' => Ok(self.canvas.glyph("−", size)),
            '~' => Ok(Layout {
                width: size * 0.25,
                ..Default::default()
            }),
            item => Ok(self.canvas.glyph(item.encode_utf8(&mut [0; 4]), size)),
        }
    }

    fn command(&mut self, size: f64, depth: u8) -> Math<Layout> {
        let command = self.word_command()?;
        let result = match command.as_str() {
            "frac" | "dfrac" | "tfrac" | "binom" => {
                let scale = if command == "dfrac" { 0.94 } else { 0.82 };
                let numerator = self.atom(size * scale, depth + 1)?;
                let denominator = self.atom(size * scale, depth + 1)?;
                let width = numerator.width.max(denominator.width) + size * 0.25;
                let top = -size * 0.36 - numerator.below;
                let bottom = size * 0.04 + denominator.above;
                let offset_num = (width - numerator.width) / 2.0;
                let offset_den = (width - denominator.width) / 2.0;
                let mut result = Layout::default();
                result.add(numerator, offset_num, top);
                result.add(denominator, offset_den, bottom);
                if command == "binom" {
                    return Ok(self.wrap(result, '(', ')', size));
                }
                result.line(size * 0.05, -size * 0.18, width, -size * 0.18, size);
                result
            }
            "sqrt" => {
                self.space();
                let root_index = if self.peek() == Some('[') {
                    self.take();
                    Some(self.row(size * 0.55, depth + 1, Some(']'))?)
                } else {
                    None
                };
                let value = self.atom(size, depth + 1)?;
                let top = -value.above - size * 0.06;
                let bottom = value.below * 0.5;
                let mut result = Layout::default();
                if let Some(index) = root_index {
                    result.add(index, 0.0, top * 0.6);
                }
                let x = result.width;
                result.line(x, -size * 0.18, x + size * 0.17, bottom, size);
                result.line(x + size * 0.17, bottom, x + size * 0.43, top, size);
                result.line(
                    x + size * 0.43,
                    top,
                    x + size * 0.49 + value.width,
                    top,
                    size,
                );
                result.add(value, x + size * 0.48, 0.0);
                result
            }
            "text" | "textrm" | "textbf" | "textit" | "operatorname" => {
                let text = self
                    .raw_group()?
                    .replace("\\{", "{")
                    .replace("\\}", "}")
                    .replace("\\%", "%");
                self.canvas.glyph(&text, size)
            }
            "mathrm" | "mathbf" | "mathit" | "mathbb" | "mathcal" | "boldsymbol" | "mathsf"
            | "mathtt" | "mathscr" => self.atom(size, depth + 1)?,
            "overline" | "underline" | "bar" | "vec" | "hat" | "widehat" | "tilde"
            | "widetilde" | "dot" | "ddot" => {
                let mut body = self.atom(size, depth + 1)?;
                let x = body.width * 0.5;
                let top = -body.above - size * 0.1;
                match command.as_str() {
                    "overline" | "bar" => body.line(0.0, top, body.width, top, size),
                    "underline" => body.line(0.0, body.below, body.width, body.below, size),
                    "vec" => {
                        body.line(0.0, top, body.width, top, size);
                        body.line(
                            body.width - size * 0.12,
                            top - size * 0.08,
                            body.width,
                            top,
                            size,
                        );
                    }
                    "dot" | "ddot" => body.add(
                        self.canvas
                            .glyph(if command == "dot" { "·" } else { "¨" }, size * 0.7),
                        x - size * 0.12,
                        top,
                    ),
                    "tilde" | "widetilde" => {
                        body.add(self.canvas.glyph("~", size * 0.7), x - size * 0.15, top)
                    }
                    _ => {
                        body.line(x - size * 0.13, top, x, top - size * 0.12, size);
                        body.line(x, top - size * 0.12, x + size * 0.13, top, size);
                    }
                }
                body
            }
            "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr" | "Bigl"
            | "Bigr" | "middle" => {
                self.space();
                if self.peek() == Some('.') {
                    self.take();
                    Layout::default()
                } else {
                    self.atom(size * 1.15, depth + 1)?
                }
            }
            "begin" => {
                let environment = self.raw_group()?.to_owned();
                self.environment(&environment, size, depth + 1)?
            }
            "newcommand" | "renewcommand" | "providecommand" => {
                self.define_macro()?;
                Layout::default()
            }
            "displaystyle" | "textstyle" | "scriptstyle" | "scriptscriptstyle" | "limits"
            | "nolimits" | "!" => Layout::default(),
            "quad" | "qquad" | "," | ":" | ";" | " " => Layout {
                width: size
                    * match command.as_str() {
                        "quad" => 1.0,
                        "qquad" => 2.0,
                        _ => 0.2,
                    },
                ..Default::default()
            },
            _ => {
                if let Some(macro_def) = self.macros.get(&command).cloned() {
                    return self.expand(macro_def, size, depth + 1);
                }
                let symbol = symbol(&command).ok_or(())?;
                self.canvas.glyph(
                    symbol,
                    size * if matches!(command.as_str(), "sum" | "prod" | "int" | "iint" | "oint") {
                        1.2
                    } else {
                        1.0
                    },
                )
            }
        };
        Ok(result)
    }

    fn word_command(&mut self) -> Math<String> {
        let next = self.take().ok_or(())?;
        let mut command = String::from(next);
        if next.is_ascii_alphabetic() {
            while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                command.push(self.take().unwrap());
            }
        }
        Ok(command)
    }

    fn raw_group(&mut self) -> Math<&str> {
        self.space();
        if self.take() != Some('{') {
            return Err(());
        }
        let rest = self.text;
        let mut level = 1;
        for (index, marker) in rest.char_indices() {
            if !matches!(marker, '{' | '}') || escaped(rest.as_bytes(), index) {
                continue;
            }
            if marker == '{' {
                level += 1;
            } else {
                level -= 1;
            }
            if level == 0 {
                self.text = &rest[index + 1..];
                return Ok(&rest[..index]);
            }
        }
        Err(())
    }

    fn define_macro(&mut self) -> Math<()> {
        let name = self.raw_group()?.strip_prefix('\\').ok_or(())?.to_owned();
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphabetic()) {
            return Err(());
        }
        self.space();
        let mut parameters = 0;
        if self.peek() == Some('[') {
            self.take();
            parameters = self.take().and_then(|c| c.to_digit(10)).ok_or(())? as u8;
            if self.take() != Some(']') {
                return Err(());
            }
        }
        let replacement = self.raw_group()?.to_owned();
        if self.macros.len() >= 32 {
            return Err(());
        }
        self.macros.insert(
            name,
            Macro {
                parameters,
                replacement,
            },
        );
        Ok(())
    }

    fn sub(&mut self, text: &str, size: f64, depth: u8) -> Math<Layout> {
        let mut parser = Parser {
            text,
            canvas: self.canvas,
            macros: self.macros,
            budget: self.budget,
        };
        parser.row(size, depth + 1, None)
    }

    fn expand(&mut self, macro_def: Macro, size: f64, depth: u8) -> Math<Layout> {
        let mut args = Vec::new();
        for _ in 0..macro_def.parameters {
            args.push(self.raw_group()?.to_owned());
        }
        let mut output = String::new();
        let mut replacement = macro_def.replacement.chars();
        while let Some(marker) = replacement.next() {
            if marker == '#' {
                let number = replacement
                    .next()
                    .and_then(|c| c.to_digit(10))
                    .and_then(|n| n.checked_sub(1))
                    .ok_or(())? as usize;
                output.push_str(args.get(number).ok_or(())?);
            } else {
                output.push(marker);
            }
            if output.len() > 8192 {
                return Err(());
            }
        }
        self.sub(&output, size, depth + 1)
    }

    fn environment(&mut self, environment: &str, size: f64, depth: u8) -> Math<Layout> {
        let delimiters = match environment {
            "matrix" | "smallmatrix" | "aligned" | "alignedat" | "gathered" | "split" | "align"
            | "align*" | "array" => (' ', ' '),
            "pmatrix" => ('(', ')'),
            "bmatrix" => ('[', ']'),
            "Bmatrix" => ('{', '}'),
            "vmatrix" => ('|', '|'),
            "Vmatrix" => ('‖', '‖'),
            "cases" => ('{', ' '),
            _ => return Err(()),
        };
        if matches!(environment, "array" | "alignedat") {
            self.raw_group()?;
        }
        let closing = format!("\\end{{{environment}}}");
        let end = self.text.find(&closing).ok_or(())?;
        let content = &self.text[..end];
        self.text = &self.text[end + closing.len()..];
        let cells = matrix_cells(content)?;
        let mut rows = Vec::new();
        let mut columns: Vec<f64> = Vec::new();
        let is_aligned = matches!(
            environment,
            "aligned" | "alignedat" | "split" | "align" | "align*"
        );
        for row in cells {
            let mut layouts = Vec::new();
            for (index, cell) in row.into_iter().enumerate() {
                let layout = self.sub(cell, size * 0.88, depth + 1)?;
                if index >= columns.len() {
                    columns.push(0.0);
                }
                columns[index] = columns[index].max(layout.width);
                layouts.push(layout);
            }
            rows.push(layouts);
        }
        let mut result = Layout::default();
        let mut top = 0.0;
        for row in rows {
            let above = row.iter().map(|item| item.above).fold(size * 0.5, f64::max);
            let below = row
                .iter()
                .map(|item| item.below)
                .fold(size * 0.15, f64::max);
            let mut x = 0.0;
            for (index, cell) in row.into_iter().enumerate() {
                let room = columns[index] - cell.width;
                let offset = if is_aligned {
                    if index % 2 == 0 { room } else { 0.0 }
                } else {
                    room / 2.0
                };
                result.add(cell, x + offset, top + above);
                x += columns[index] + size * 0.52;
            }
            top += above + below + size * 0.22;
        }
        let shift = -(result.below + size * 0.28) / 2.0;
        let mut centered = Layout::default();
        centered.add(result, 0.0, shift);
        Ok(self.wrap(centered, delimiters.0, delimiters.1, size))
    }

    fn wrap(&self, item: Layout, left: char, right: char, size: f64) -> Layout {
        let mut result = Layout::default();
        let bracket_size = (item.above + item.below).max(size);
        let baseline = (item.below - item.above) / 2.0 + bracket_size * 0.34;
        if left != ' ' {
            result.add(
                self.canvas
                    .glyph(left.encode_utf8(&mut [0; 4]), bracket_size),
                0.0,
                baseline,
            );
        }
        result.append(item);
        if right != ' ' {
            result.add(
                self.canvas
                    .glyph(right.encode_utf8(&mut [0; 4]), bracket_size),
                result.width,
                baseline,
            );
        }
        result
    }
}

fn matrix_cells(text: &str) -> Math<Vec<Vec<&str>>> {
    let mut rows = vec![Vec::new()];
    let mut begin = 0;
    let mut level: i32 = 0;
    let mut tokens = text.char_indices();
    while let Some((index, marker)) = tokens.next() {
        match marker {
            '\\' => {
                if let Some((_, '\\')) = tokens.next()
                    && level == 0
                {
                    rows.last_mut().ok_or(())?.push(&text[begin..index]);
                    rows.push(Vec::new());
                    begin = index + 2;
                }
            }
            '{' => level += 1,
            '}' => level -= 1,
            '&' if level == 0 => {
                rows.last_mut().ok_or(())?.push(&text[begin..index]);
                begin = index + 1;
            }
            _ => {}
        }
    }
    rows.last_mut().ok_or(())?.push(&text[begin..]);
    if level != 0 || rows.len() > 24 || rows.iter().any(|row| row.len() > 24) {
        return Err(());
    }
    Ok(rows)
}

fn symbol(command: &str) -> Option<&'static str> {
    Some(match command {
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" => "ϵ",
        "varepsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" => "θ",
        "vartheta" => "ϑ",
        "iota" => "ι",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "varpi" => "ϖ",
        "rho" => "ρ",
        "varrho" => "ϱ",
        "sigma" => "σ",
        "varsigma" => "ς",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" => "ϕ",
        "varphi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Upsilon" => "Υ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        "sum" => "∑",
        "prod" => "∏",
        "int" => "∫",
        "iint" => "∬",
        "oint" => "∮",
        "infty" => "∞",
        "times" => " × ",
        "cdot" => " · ",
        "div" => " ÷ ",
        "pm" => " ± ",
        "mp" => " ∓ ",
        "le" | "leq" | "leqslant" => " ≤ ",
        "ge" | "geq" | "geqslant" => " ≥ ",
        "ne" | "neq" => " ≠ ",
        "equiv" => " ≡ ",
        "approx" => " ≈ ",
        "sim" => " ∼ ",
        "simeq" => " ≃ ",
        "cong" => " ≅ ",
        "propto" => " ∝ ",
        "ll" => " ≪ ",
        "gg" => " ≫ ",
        "partial" => "∂",
        "nabla" => "∇",
        "forall" => "∀",
        "exists" => "∃",
        "in" => " ∈ ",
        "notin" => " ∉ ",
        "ni" => " ∋ ",
        "emptyset" | "varnothing" => "∅",
        "cup" => " ∪ ",
        "cap" => " ∩ ",
        "subset" => " ⊂ ",
        "supset" => " ⊃ ",
        "subseteq" => " ⊆ ",
        "supseteq" => " ⊇ ",
        "setminus" => " ∖ ",
        "land" | "wedge" => " ∧ ",
        "lor" | "vee" => " ∨ ",
        "neg" => "¬",
        "oplus" => " ⊕ ",
        "otimes" => " ⊗ ",
        "to" | "rightarrow" | "longrightarrow" => " → ",
        "leftarrow" | "gets" | "longleftarrow" => " ← ",
        "Rightarrow" | "Longrightarrow" | "implies" => " ⇒ ",
        "Leftarrow" | "Longleftarrow" => " ⇐ ",
        "Leftrightarrow" | "Longleftrightarrow" | "iff" => " ⇔ ",
        "leftrightarrow" => " ↔ ",
        "mapsto" => " ↦ ",
        "uparrow" => "↑",
        "downarrow" => "↓",
        "perp" => " ⊥ ",
        "parallel" => " ∥ ",
        "angle" => "∠",
        "ldots" | "dots" => "…",
        "cdots" => "⋯",
        "vdots" => "⋮",
        "ddots" => "⋱",
        "therefore" => "∴",
        "langle" => "⟨",
        "rangle" => "⟩",
        "lceil" => "⌈",
        "rceil" => "⌉",
        "lfloor" => "⌊",
        "rfloor" => "⌋",
        "vert" | "mid" | "lvert" | "rvert" => "|",
        "Vert" | "lVert" | "rVert" | "|" => "‖",
        "lbrace" | "{" => "{",
        "rbrace" | "}" => "}",
        "$" => "$",
        "%" => "%",
        "#" => "#",
        "&" => "&",
        "_" => "_",
        "prime" => "′",
        "circ" => "∘",
        "degree" => "°",
        "hbar" => "ℏ",
        "ell" => "ℓ",
        "Re" => "ℜ",
        "Im" => "ℑ",
        "sin" => "sin",
        "cos" => "cos",
        "tan" => "tan",
        "cot" => "cot",
        "sec" => "sec",
        "csc" => "csc",
        "sinh" => "sinh",
        "cosh" => "cosh",
        "tanh" => "tanh",
        "arcsin" => "arcsin",
        "arccos" => "arccos",
        "arctan" => "arctan",
        "log" => "log",
        "ln" => "ln",
        "exp" => "exp",
        "lim" => "lim",
        "min" => "min",
        "max" => "max",
        "det" => "det",
        "gcd" => "gcd",
        "sup" => "sup",
        "inf" => "inf",
        "mod" | "bmod" | "pmod" => " mod ",
        _ => return None,
    })
}

pub fn preview(text: &str) -> Result<Preview, String> {
    let formulas = extract(text);
    let mut preview = Preview {
        image: None,
        rendered: 0,
        skipped: formulas.len().saturating_sub(MAX_FORMULAS),
    };
    if formulas.is_empty() {
        return Ok(preview);
    }
    let measurement = Canvas::new(1, 1)?;
    let mut rows = Vec::new();
    let mut width: f64 = 320.0;
    let mut height = PADDING;
    for formula in formulas.into_iter().take(MAX_FORMULAS) {
        let layout = (formula.tex.encode_utf16().count() <= MAX_LENGTH)
            .then(|| {
                Parser::parse(
                    &measurement,
                    formula.tex,
                    if formula.display { 26.0 } else { 24.0 },
                )
            })
            .and_then(Result::ok)
            .filter(|layout| !layout.marks.is_empty());
        let Some(layout) = layout else {
            preview.skipped += 1;
            continue;
        };
        let scale = (1312.0 / layout.width.max(1.0))
            .min(300.0 / (layout.above + layout.below).max(1.0))
            .min(1.0);
        let row_height = ((layout.above + layout.below) * scale).clamp(36.0, 300.0);
        width = width.max(layout.width * scale + PADDING * 2.0 + 40.0);
        rows.push((height, scale, layout));
        height += row_height + PADDING;
    }
    preview.rendered = rows.len();
    if rows.is_empty() {
        return Ok(preview);
    }
    let canvas = Canvas::new(width.ceil() as i32, height.ceil() as i32)?;
    unsafe {
        cairo_set_source_rgb(canvas.context, 1.0, 1.0, 1.0);
        cairo_paint(canvas.context);
        cairo_set_source_rgb(canvas.context, 0.08, 0.08, 0.08);
    }
    for (index, (top, scale, layout)) in rows.into_iter().enumerate() {
        canvas.draw(
            &canvas.glyph(&format!("{}.", index + 1), 16.0),
            PADDING,
            top + 24.0,
            1.0,
        );
        canvas.draw(&layout, PADDING + 40.0, top + layout.above * scale, scale);
    }
    preview.image = Some(canvas.png()?);
    Ok(preview)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_all_delimiters_and_skips_code_and_prices() {
        let text = "Inline $x^2$ and \\(y^2\\).\n$$\\frac{a}{b}$$\n\\[\\sum_{i=1}^n i\\]\n\
            \x60\x60\x60latex\na+b=c\n\x60\x60\x60\n~~~math\nx+y\n~~~\n$x^2$";
        let found = extract(text);
        assert_eq!(found.len(), 6);
        assert!(!found[0].display);
        assert!(found[2].display);
        let ignored = "\x60$code$\x60\n\x60\x60\x60js\nconst a = \"$notMath$\"\n\x60\x60\x60\nGiá \\$5 hoặc $10 và $20. \\[x^2";
        assert!(extract(ignored).is_empty());
        assert!(extract("$$unfinished").is_empty());
        assert_eq!(extract("$x+\\$y$")[0].tex, "x+\\$y");
    }

    #[test]
    fn renders_fractions_matrices_vietnamese_and_local_macros() {
        let text = r"$$\frac{-b\pm\sqrt{b^2-4ac}}{2a}$$
            \[\begin{pmatrix}1&2\\3&4\end{pmatrix}\]
            $$\text{Diện tích}=\pi r^2$$ $\newcommand{\myvar}{x}\myvar$ $\myvar$";
        let preview = preview(text).unwrap();
        assert_eq!((preview.rendered, preview.skipped), (4, 1));
        let png = preview.image.unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        assert!((320..=1400).contains(&width));
        assert!(height <= 3912);
    }

    #[test]
    fn bad_large_and_excess_formulas_do_not_block_others() {
        let text = format!(
            "$\\notARealCommand{{x}}$ \x24{}\x24 $x^2$",
            "x".repeat(2001)
        );
        let invalid = preview(&text).unwrap();
        assert_eq!((invalid.rendered, invalid.skipped), (1, 2));
        let many = (0..14)
            .map(|index| format!("$x_{{{index}}}$"))
            .collect::<Vec<_>>()
            .join(" ");
        let capped = preview(&many).unwrap();
        assert_eq!((capped.rendered, capped.skipped), (12, 2));
        let invalid = preview(r"$\frac{$").unwrap();
        assert_eq!((invalid.rendered, invalid.skipped), (0, 1));
        assert!(invalid.image.is_none());
    }
}
