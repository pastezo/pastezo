//! ICU MessageFormat — the subset translators use and formatjs accepts:
//! `{arg}`, `{n, plural, =0 {…} one {# item} other {# items}}`,
//! `{x, select, a {…} other {…}}`, `{n, selectordinal, …}`, `#`,
//! rich-text tags (`<b>…</b>`) and apostrophe quoting (`''`, `'{literal}'`).
//! Messages are parsed once, when a language is loaded.

use std::collections::HashMap;

use icu_plurals::{PluralCategory, PluralRules};

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Text(String),
    Arg(String),
    /// `#` inside a plural branch: the number
    Pound,
    Tag(String, Vec<Node>),
    Plural { arg: String, ordinal: bool, offset: i64, branches: Vec<(String, Vec<Node>)> },
    Select { arg: String, branches: Vec<(String, Vec<Node>)> },
}

pub type Message = Vec<Node>;

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError(pub String);

pub fn parse(src: &str) -> Result<Message, ParseError> {
    let mut p = Parser { s: src.chars().collect(), i: 0 };
    let nodes = p.nodes(false, None)?;
    if p.i < p.s.len() {
        return Err(p.err("unexpected '}' or closing tag"));
    }
    Ok(nodes)
}

struct Parser {
    s: Vec<char>,
    i: usize,
}

impl Parser {
    fn err(&self, what: &str) -> ParseError {
        ParseError(format!("{what} at {}", self.i))
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn starts_with(&self, t: &str) -> bool {
        t.chars().enumerate().all(|(k, c)| self.s.get(self.i + k) == Some(&c))
    }

    /// Nodes until `}` (inside a branch), `</tag>` or the end.
    fn nodes(&mut self, in_plural: bool, in_tag: Option<&str>) -> Result<Vec<Node>, ParseError> {
        let mut out = Vec::new();
        let mut text = String::new();
        let flush = |text: &mut String, out: &mut Vec<Node>| {
            if !text.is_empty() {
                out.push(Node::Text(std::mem::take(text)));
            }
        };
        while let Some(c) = self.peek() {
            match c {
                '}' => break,
                '<' if self.starts_with("</") => {
                    if in_tag.is_some() {
                        break;
                    }
                    return Err(self.err("closing tag without an opening one"));
                }
                '<' if self.s.get(self.i + 1).is_some_and(|c| c.is_ascii_alphabetic()) => {
                    flush(&mut text, &mut out);
                    out.push(self.tag(in_plural)?);
                }
                '{' => {
                    flush(&mut text, &mut out);
                    out.push(self.argument()?);
                }
                '#' if in_plural => {
                    flush(&mut text, &mut out);
                    self.i += 1;
                    out.push(Node::Pound);
                }
                '\'' => {
                    self.i += 1;
                    match self.peek() {
                        Some('\'') => {
                            text.push('\'');
                            self.i += 1;
                        }
                        // '{…}', '<…', '#…' are literal up to the next single quote
                        Some('{' | '}' | '<' | '#') => {
                            while let Some(c) = self.peek() {
                                self.i += 1;
                                if c == '\'' {
                                    if self.peek() == Some('\'') {
                                        text.push('\'');
                                        self.i += 1;
                                        continue;
                                    }
                                    break;
                                }
                                text.push(c);
                            }
                        }
                        _ => text.push('\''),
                    }
                }
                _ => {
                    text.push(c);
                    self.i += 1;
                }
            }
        }
        flush(&mut text, &mut out);
        Ok(out)
    }

    fn tag(&mut self, in_plural: bool) -> Result<Node, ParseError> {
        self.i += 1; // <
        let name = self.word();
        if name.is_empty() || self.peek() != Some('>') {
            return Err(self.err("bad tag"));
        }
        self.i += 1;
        let children = self.nodes(in_plural, Some(&name))?;
        let close = format!("</{name}>");
        if !self.starts_with(&close) {
            return Err(self.err(&format!("missing {close}")));
        }
        self.i += close.chars().count();
        Ok(Node::Tag(name, children))
    }

    fn ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.i += 1;
        }
    }

    fn word(&mut self) -> String {
        let mut w = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' || c == '-' || c == '=' || c == ':' || c == '.' {
                w.push(c);
                self.i += 1;
            } else {
                break;
            }
        }
        w
    }

    fn expect(&mut self, c: char) -> Result<(), ParseError> {
        self.ws();
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected '{c}'")))
        }
    }

    fn argument(&mut self) -> Result<Node, ParseError> {
        self.i += 1; // {
        self.ws();
        let arg = self.word();
        if arg.is_empty() {
            return Err(self.err("empty argument"));
        }
        self.ws();
        if self.peek() == Some('}') {
            self.i += 1;
            return Ok(Node::Arg(arg));
        }
        self.expect(',')?;
        self.ws();
        let kind = self.word();
        self.ws();
        match kind.as_str() {
            "plural" | "selectordinal" | "select" => {
                self.expect(',')?;
                let mut offset = 0;
                let mut branches = Vec::new();
                loop {
                    self.ws();
                    if self.peek() == Some('}') {
                        self.i += 1;
                        break;
                    }
                    let key = self.word();
                    if key.is_empty() {
                        return Err(self.err("expected a branch key"));
                    }
                    if let Some(n) = key.strip_prefix("offset:") {
                        offset = n.parse().map_err(|_| self.err("bad offset"))?;
                        continue;
                    }
                    self.expect('{')?;
                    let body = self.nodes(kind != "select", None)?;
                    self.expect('}')?;
                    branches.push((key, body));
                }
                if !branches.iter().any(|(k, _)| k == "other") {
                    return Err(self.err("an 'other' branch is required"));
                }
                Ok(if kind == "select" {
                    Node::Select { arg, branches }
                } else {
                    Node::Plural { arg, ordinal: kind == "selectordinal", offset, branches }
                })
            }
            // number/date/time and friends: shown as the plain value
            _ => {
                while self.peek().is_some_and(|c| c != '}') {
                    self.i += 1;
                }
                self.expect('}')?;
                Ok(Node::Arg(arg))
            }
        }
    }
}

/// A run of formatted text; `bold` marks text inside `<b>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
}

pub enum Value<'a> {
    Str(&'a str),
    /// for `{n, plural, …}` messages (none in the UI yet; the tests cover them)
    #[allow(dead_code)]
    Num(i64),
}

pub struct Rules {
    pub cardinal: Option<PluralRules>,
    pub ordinal: Option<PluralRules>,
}

pub fn format(msg: &Message, args: &HashMap<&str, Value>, rules: &Rules) -> Vec<Run> {
    let mut runs = Vec::new();
    emit(msg, args, rules, false, None, &mut runs);
    runs
}

fn push(runs: &mut Vec<Run>, text: &str, bold: bool) {
    if text.is_empty() {
        return;
    }
    match runs.last_mut() {
        Some(last) if last.bold == bold => last.text.push_str(text),
        _ => runs.push(Run { text: text.to_string(), bold }),
    }
}

fn category_key(c: PluralCategory) -> &'static str {
    match c {
        PluralCategory::Zero => "zero",
        PluralCategory::One => "one",
        PluralCategory::Two => "two",
        PluralCategory::Few => "few",
        PluralCategory::Many => "many",
        PluralCategory::Other => "other",
    }
}

fn emit(nodes: &[Node], args: &HashMap<&str, Value>, rules: &Rules, bold: bool, pound: Option<i64>, runs: &mut Vec<Run>) {
    for n in nodes {
        match n {
            Node::Text(t) => push(runs, t, bold),
            Node::Arg(a) => match args.get(a.as_str()) {
                Some(Value::Str(s)) => push(runs, s, bold),
                Some(Value::Num(v)) => push(runs, &v.to_string(), bold),
                None => push(runs, &format!("{{{a}}}"), bold),
            },
            Node::Pound => {
                if let Some(v) = pound {
                    push(runs, &v.to_string(), bold)
                }
            }
            Node::Tag(name, children) => emit(children, args, rules, bold || name == "b", pound, runs),
            Node::Select { arg, branches } => {
                let key = match args.get(arg.as_str()) {
                    Some(Value::Str(s)) => s.to_string(),
                    Some(Value::Num(v)) => v.to_string(),
                    None => String::new(),
                };
                let body = branches
                    .iter()
                    .find(|(k, _)| *k == key)
                    .or_else(|| branches.iter().find(|(k, _)| k == "other"));
                if let Some((_, b)) = body {
                    emit(b, args, rules, bold, pound, runs);
                }
            }
            Node::Plural { arg, ordinal, offset, branches } => {
                let v = match args.get(arg.as_str()) {
                    Some(Value::Num(v)) => *v,
                    Some(Value::Str(s)) => s.parse().unwrap_or(0),
                    None => 0,
                };
                let exact = format!("={v}");
                let n = v - offset;
                let rule = if *ordinal { &rules.ordinal } else { &rules.cardinal };
                let cat = rule.as_ref().map_or("other", |r| category_key(r.category_for(n.unsigned_abs() as usize)));
                let body = branches
                    .iter()
                    .find(|(k, _)| *k == exact)
                    .or_else(|| branches.iter().find(|(k, _)| k == cat))
                    .or_else(|| branches.iter().find(|(k, _)| k == "other"));
                if let Some((_, b)) = body {
                    emit(b, args, rules, bold, Some(n), runs);
                }
            }
        }
    }
}

/// Argument and tag names a message uses, for checking translations.
#[cfg(test)]
pub fn signature(msg: &Message) -> (Vec<String>, Vec<String>) {
    fn walk(nodes: &[Node], args: &mut Vec<String>, tags: &mut Vec<String>) {
        for n in nodes {
            match n {
                Node::Arg(a) => args.push(a.clone()),
                Node::Tag(t, c) => {
                    tags.push(t.clone());
                    walk(c, args, tags);
                }
                Node::Plural { arg, branches, .. } | Node::Select { arg, branches } => {
                    args.push(arg.clone());
                    for (_, b) in branches {
                        walk(b, args, tags);
                    }
                }
                Node::Text(_) | Node::Pound => {}
            }
        }
    }
    let (mut args, mut tags) = (Vec::new(), Vec::new());
    walk(msg, &mut args, &mut tags);
    args.sort();
    args.dedup();
    tags.sort();
    tags.dedup();
    (args, tags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use icu_locale::locale;

    fn run(src: &str, args: &[(&str, Value)], lang: icu_locale::Locale) -> Vec<(String, bool)> {
        let rules = Rules {
            cardinal: PluralRules::try_new_cardinal((&lang).into()).ok(),
            ordinal: PluralRules::try_new_ordinal((&lang).into()).ok(),
        };
        let map: HashMap<&str, Value> = args
            .iter()
            .map(|(k, v)| (*k, match v { Value::Str(s) => Value::Str(s), Value::Num(n) => Value::Num(*n) }))
            .collect();
        format(&parse(src).unwrap(), &map, &rules).into_iter().map(|r| (r.text, r.bold)).collect()
    }

    #[test]
    fn args_and_bold() {
        assert_eq!(
            run("Copied from <b>{app}</b>", &[("app", Value::Str("Safari"))], locale!("en")),
            vec![("Copied from ".into(), false), ("Safari".into(), true)]
        );
        assert_eq!(
            run("<b>{day}</b> в <b>{time}</b>", &[("day", Value::Str("Сегодня")), ("time", Value::Str("21:00"))], locale!("ru")),
            vec![("Сегодня".into(), true), (" в ".into(), false), ("21:00".into(), true)]
        );
    }

    #[test]
    fn plurals_follow_the_language() {
        let msg = "{n, plural, =0 {нет записей} one {# запись} few {# записи} many {# записей} other {# записи}}";
        let f = |n| run(msg, &[("n", Value::Num(n))], locale!("ru")).into_iter().map(|r| r.0).collect::<String>();
        assert_eq!(f(0), "нет записей");
        assert_eq!(f(1), "1 запись");
        assert_eq!(f(3), "3 записи");
        assert_eq!(f(5), "5 записей");
        assert_eq!(f(21), "21 запись");
    }

    #[test]
    fn select_and_quotes() {
        let msg = "{os, select, macos {⌘''F} other {Ctrl+F}} '{literal}' it''s";
        assert_eq!(
            run(msg, &[("os", Value::Str("macos"))], locale!("en")).into_iter().map(|r| r.0).collect::<String>(),
            "⌘'F {literal} it's"
        );
    }

    #[test]
    fn errors() {
        assert!(parse("{bad").is_err());
        assert!(parse("<b>x").is_err());
        assert!(parse("{n, plural, one {x}}").is_err()); // no "other"
        assert!(parse("x</b>").is_err());
    }
}
