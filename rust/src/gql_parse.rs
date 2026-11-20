//! GraphQL document lexer and parser: operations, fragments, variables with defaults, aliases,
//! arguments (variables, scalars, enums, lists, input objects), directives, inline fragments.

use crate::json::Json;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub enum Value {
    Var(String),
    Int(f64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    Enum(String),
    List(Vec<Value>),
    Object(Vec<(String, Value)>),
}

#[derive(Clone, Debug)]
pub struct Directive {
    pub name: String,
    pub args: Vec<(String, Value)>,
}

#[derive(Clone, Debug)]
pub enum Selection {
    Field { alias: Option<String>, name: String, args: Vec<(String, Value)>, directives: Vec<Directive>, selections: Option<Vec<Selection>> },
    Spread { name: String, directives: Vec<Directive> },
    Inline { type_condition: Option<String>, directives: Vec<Directive>, selections: Vec<Selection> },
}

#[derive(Clone, Debug)]
pub struct VarDef {
    pub name: String,
    pub type_ref: String,
    pub default: Option<Value>,
}

#[derive(Clone, Debug)]
pub struct Operation {
    pub kind: String, // query | mutation | subscription
    pub name: Option<String>,
    pub vars: Vec<VarDef>,
    pub selections: Vec<Selection>,
}

#[derive(Clone, Debug)]
pub struct Fragment {
    pub type_condition: String,
    pub selections: Vec<Selection>,
}

pub struct Document {
    pub operations: Vec<Operation>,
    pub fragments: HashMap<String, Fragment>,
}

#[derive(Clone, PartialEq, Debug)]
enum Tok {
    Punct(String),
    Name(String),
    Int(String),
    Float(String),
    Str(String),
    Eof,
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    let n = chars.len();
    while i < n {
        let c = chars[i];
        if c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == ',' || c == '\u{feff}' {
            i += 1;
            continue;
        }
        if c == '#' {
            while i < n && chars[i] != '\n' && chars[i] != '\r' {
                i += 1;
            }
            continue;
        }
        if c == '.' && i + 2 < n && chars[i + 1] == '.' && chars[i + 2] == '.' {
            toks.push(Tok::Punct("...".into()));
            i += 3;
            continue;
        }
        if "!$&()[]{}:=@|".contains(c) {
            toks.push(Tok::Punct(c.to_string()));
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let s = i;
            while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            toks.push(Tok::Name(chars[s..i].iter().collect()));
            continue;
        }
        if c == '-' || c.is_ascii_digit() {
            let s = i;
            if c == '-' {
                i += 1;
            }
            while i < n && chars[i].is_ascii_digit() {
                i += 1;
            }
            let mut float = false;
            if i < n && chars[i] == '.' {
                float = true;
                i += 1;
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if i < n && (chars[i] == 'e' || chars[i] == 'E') {
                float = true;
                i += 1;
                if i < n && (chars[i] == '+' || chars[i] == '-') {
                    i += 1;
                }
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let text: String = chars[s..i].iter().collect();
            if text == "-" {
                return Err(format!("Syntax Error: unexpected character \"-\" at {}", s));
            }
            toks.push(if float { Tok::Float(text) } else { Tok::Int(text) });
            continue;
        }
        if c == '"' {
            if i + 2 < n && chars[i + 1] == '"' && chars[i + 2] == '"' {
                let start = i + 3;
                let mut j = start;
                while j + 2 < n && !(chars[j] == '"' && chars[j + 1] == '"' && chars[j + 2] == '"') {
                    j += 1;
                }
                if j + 2 >= n {
                    return Err("Syntax Error: unterminated block string".into());
                }
                let raw: String = chars[start..j].iter().collect();
                toks.push(Tok::Str(block_string(&raw)));
                i = j + 3;
                continue;
            }
            let mut j = i + 1;
            let mut out = String::new();
            while j < n && chars[j] != '"' {
                if chars[j] == '\n' {
                    return Err("Syntax Error: unterminated string".into());
                }
                if chars[j] == '\\' && j + 1 < n {
                    let e = chars[j + 1];
                    if e == 'u' && j + 5 < n {
                        let hex: String = chars[j + 2..j + 6].iter().collect();
                        out.push(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('\u{fffd}'));
                        j += 6;
                        continue;
                    }
                    out.push(match e {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        'b' => '\u{08}',
                        'f' => '\u{0c}',
                        other => other,
                    });
                    j += 2;
                    continue;
                }
                out.push(chars[j]);
                j += 1;
            }
            if j >= n {
                return Err("Syntax Error: unterminated string".into());
            }
            toks.push(Tok::Str(out));
            i = j + 1;
            continue;
        }
        return Err(format!("Syntax Error: unexpected character {:?} at {}", c, i));
    }
    toks.push(Tok::Eof);
    Ok(toks)
}

fn block_string(raw: &str) -> String {
    let lines: Vec<&str> = raw.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let mut common = usize::MAX;
    for l in lines.iter().skip(1) {
        let indent = l.len() - l.trim_start().len();
        if indent < l.len() {
            common = common.min(indent);
        }
    }
    let mut out: Vec<String> = lines.iter().enumerate().map(|(k, l)| if k == 0 || common == usize::MAX { l.to_string() } else { l.get(common..).unwrap_or("").to_string() }).collect();
    while out.first().map(|l| l.trim().is_empty()).unwrap_or(false) {
        out.remove(0);
    }
    while out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        out.pop();
    }
    out.join("\n")
}

struct Parser {
    toks: Vec<Tok>,
    i: usize,
}

fn desc(t: &Tok) -> String {
    match t {
        Tok::Eof => "<EOF>".into(),
        Tok::Punct(s) | Tok::Name(s) | Tok::Int(s) | Tok::Float(s) => format!("\"{}\"", s),
        Tok::Str(s) => format!("\"{}\"", s),
    }
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.i]
    }
    fn next(&mut self) -> Tok {
        let t = self.toks[self.i].clone();
        if self.i < self.toks.len() - 1 {
            self.i += 1;
        }
        t
    }
    fn is(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::Punct(s) if s == p)
    }
    fn expect(&mut self, p: &str) -> Result<(), String> {
        let t = self.next();
        match t {
            Tok::Punct(ref s) if s == p => Ok(()),
            other => Err(format!("Syntax Error: expected \"{}\", found {}", p, desc(&other))),
        }
    }
    fn name(&mut self) -> Result<String, String> {
        match self.next() {
            Tok::Name(s) => Ok(s),
            other => Err(format!("Syntax Error: expected a name, found {}", desc(&other))),
        }
    }

    fn document(&mut self) -> Result<Document, String> {
        let mut operations = Vec::new();
        let mut fragments = HashMap::new();
        while *self.peek() != Tok::Eof {
            if self.is("{") {
                operations.push(Operation { kind: "query".into(), name: None, vars: vec![], selections: self.selection_set()? });
                continue;
            }
            let word = self.name()?;
            if word == "fragment" {
                let name = self.name()?;
                if self.name()? != "on" {
                    return Err("Syntax Error: expected \"on\"".into());
                }
                let tc = self.name()?;
                self.directives()?;
                fragments.insert(name, Fragment { type_condition: tc, selections: self.selection_set()? });
                continue;
            }
            if word != "query" && word != "mutation" && word != "subscription" {
                return Err(format!("Syntax Error: unexpected \"{}\"", word));
            }
            let name = if matches!(self.peek(), Tok::Name(_)) { Some(self.name()?) } else { None };
            let mut vars = Vec::new();
            if self.is("(") {
                self.next();
                while !self.is(")") {
                    if *self.peek() == Tok::Eof {
                        return Err("Syntax Error: unexpected <EOF>".into());
                    }
                    self.expect("$")?;
                    let vname = self.name()?;
                    self.expect(":")?;
                    let type_ref = self.type_ref()?;
                    let default = if self.is("=") {
                        self.next();
                        Some(self.value(true)?)
                    } else {
                        None
                    };
                    self.directives()?;
                    vars.push(VarDef { name: vname, type_ref, default });
                }
                self.next();
            }
            self.directives()?;
            operations.push(Operation { kind: word, name, vars, selections: self.selection_set()? });
        }
        if operations.is_empty() {
            return Err("Syntax Error: the document has no operation".into());
        }
        Ok(Document { operations, fragments })
    }

    fn type_ref(&mut self) -> Result<String, String> {
        let mut out = if self.is("[") {
            self.next();
            let inner = self.type_ref()?;
            self.expect("]")?;
            format!("[{}]", inner)
        } else {
            self.name()?
        };
        if self.is("!") {
            self.next();
            out.push('!');
        }
        Ok(out)
    }

    fn directives(&mut self) -> Result<Vec<Directive>, String> {
        let mut out = Vec::new();
        while self.is("@") {
            self.next();
            let name = self.name()?;
            let args = if self.is("(") { self.arguments()? } else { vec![] };
            out.push(Directive { name, args });
        }
        Ok(out)
    }

    fn arguments(&mut self) -> Result<Vec<(String, Value)>, String> {
        self.expect("(")?;
        let mut args = Vec::new();
        while !self.is(")") {
            if *self.peek() == Tok::Eof {
                return Err("Syntax Error: unexpected <EOF>".into());
            }
            let n = self.name()?;
            self.expect(":")?;
            args.push((n, self.value(false)?));
        }
        self.next();
        Ok(args)
    }

    fn selection_set(&mut self) -> Result<Vec<Selection>, String> {
        self.expect("{")?;
        let mut out = Vec::new();
        while !self.is("}") {
            if *self.peek() == Tok::Eof {
                return Err("Syntax Error: expected a name, found <EOF>".into());
            }
            if self.is("...") {
                self.next();
                if let Tok::Name(n) = self.peek().clone() {
                    if n != "on" {
                        self.next();
                        let directives = self.directives()?;
                        out.push(Selection::Spread { name: n, directives });
                        continue;
                    }
                }
                let mut tc = None;
                if matches!(self.peek(), Tok::Name(s) if s == "on") {
                    self.next();
                    tc = Some(self.name()?);
                }
                let directives = self.directives()?;
                out.push(Selection::Inline { type_condition: tc, directives, selections: self.selection_set()? });
                continue;
            }
            let mut alias = None;
            let mut name = self.name()?;
            if self.is(":") {
                self.next();
                alias = Some(name);
                name = self.name()?;
            }
            let args = if self.is("(") { self.arguments()? } else { vec![] };
            let directives = self.directives()?;
            let selections = if self.is("{") { Some(self.selection_set()?) } else { None };
            out.push(Selection::Field { alias, name, args, directives, selections });
        }
        self.next();
        if out.is_empty() {
            return Err("Syntax Error: empty selection set".into());
        }
        Ok(out)
    }

    fn value(&mut self, constant: bool) -> Result<Value, String> {
        match self.next() {
            Tok::Punct(p) if p == "$" => {
                if constant {
                    return Err("Syntax Error: a variable is not allowed here".into());
                }
                Ok(Value::Var(self.name()?))
            }
            Tok::Int(s) => Ok(Value::Int(s.parse().unwrap_or(0.0))),
            Tok::Float(s) => Ok(Value::Float(s.parse().unwrap_or(0.0))),
            Tok::Str(s) => Ok(Value::Str(s)),
            Tok::Name(n) => Ok(match n.as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                "null" => Value::Null,
                _ => Value::Enum(n),
            }),
            Tok::Punct(p) if p == "[" => {
                let mut items = Vec::new();
                while !self.is("]") {
                    if *self.peek() == Tok::Eof {
                        return Err("Syntax Error: unexpected <EOF>".into());
                    }
                    items.push(self.value(constant)?);
                }
                self.next();
                Ok(Value::List(items))
            }
            Tok::Punct(p) if p == "{" => {
                let mut fields = Vec::new();
                while !self.is("}") {
                    if *self.peek() == Tok::Eof {
                        return Err("Syntax Error: unexpected <EOF>".into());
                    }
                    let n = self.name()?;
                    self.expect(":")?;
                    fields.push((n, self.value(constant)?));
                }
                self.next();
                Ok(Value::Object(fields))
            }
            other => Err(format!("Syntax Error: unexpected {}", desc(&other))),
        }
    }
}

pub fn parse_document(src: &str) -> Result<Document, String> {
    let toks = lex(src)?;
    Parser { toks, i: 0 }.document()
}

/// A value node resolved against the request's variables.
pub fn value_of(v: &Value, vars: &Json) -> Json {
    match v {
        Value::Var(n) => vars.at(n).clone(),
        Value::Int(x) | Value::Float(x) => Json::Num(*x),
        Value::Str(s) | Value::Enum(s) => Json::Str(s.clone()),
        Value::Bool(b) => Json::Bool(*b),
        Value::Null => Json::Null,
        Value::List(items) => Json::Arr(items.iter().map(|x| value_of(x, vars)).collect()),
        Value::Object(fields) => Json::Obj(fields.iter().map(|(k, x)| (k.clone(), value_of(x, vars))).collect()),
    }
}
