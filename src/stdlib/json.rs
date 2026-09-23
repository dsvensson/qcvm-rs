// SPDX-License-Identifier: MIT OR Apache-2.0

//! JSON parsing into VM memory (docs/spec/builtins.md).
//!
//! `json_parse` builds the whole tree in one heap block (freed with `json_free`, which is
//! `memfree`), so QuakeC can walk it with pointers. Each node is 16 bytes:
//!
//! ```text
//! +0  int    type    0 string, 1 number, 2 object, 3 array, 4 true, 5 false, 6 null
//! +4  string name    the key in the parent (array elements are named "0", "1", …), else null
//! +8  union          object/array: int first-child pointer, int count
//!                    number/true/false/null: double (1 for true, 0 for false and null)
//!                    string: string (a temp string, which outlives the tree)
//! ```
//!
//! The root comes first; the children of each object or array are contiguous, laid out in
//! pre-order; the names follow the nodes. The parser is FTE's lenient one: `//` and `/* */`
//! comments, trailing commas, a UTF-8 byte-order mark, unquoted words as numbers, and
//! case-insensitive `true`/`false`/`null` are accepted; keys are used verbatim (escapes are not
//! decoded). `\uXXXX` escapes in strings are decoded to UTF-8 (a NUL as the overlong `C0 80`).

use crate::builtins::Builtins;
use crate::bytes::usize_from;
use crate::error::VmError;
use crate::host::Host;
use crate::vm::Vm;
use crate::vm::core::Core;

use super::introspect::soft_error;
use super::memory::heap_alloc;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("json_parse", json_parse::<H>);
    b.set("json_free", super::memory::memfree::<H>);
    b.set("json_get_value_type", json_get_value_type::<H>);
    b.set("json_get_name", json_get_name::<H>);
    b.set("json_get_integer", json_get_integer::<H>);
    b.set("json_get_float", json_get_float::<H>);
    b.set("json_get_string", json_get_string::<H>);
    b.set("json_find_object_child", json_find_object_child::<H>);
    b.set("json_get_length", json_get_length::<H>);
    b.set("json_get_child_at_index", json_get_child_at_index::<H>);
}

const TYPE_STRING: u32 = 0;
const TYPE_NUMBER: u32 = 1;
const TYPE_OBJECT: u32 = 2;
const TYPE_ARRAY: u32 = 3;
const TYPE_TRUE: u32 = 4;
const TYPE_FALSE: u32 = 5;
const TYPE_NULL: u32 = 6;

/// Bytes per node.
const NODE: usize = 16;
/// Nesting deeper than this fails the parse (instead of exhausting the stack).
const MAX_DEPTH: u32 = 256;

// ---- parsing ----------------------------------------------------------------------------------

/// A byte range of the input.
#[derive(Clone, Copy, Debug)]
struct Span {
    start: u32,
    end: u32,
}

impl Span {
    fn len(self) -> usize {
        usize_from(self.end.saturating_sub(self.start))
    }

    fn of(self, data: &[u8]) -> &[u8] {
        data.get(usize_from(self.start)..usize_from(self.end)).unwrap_or_default()
    }
}

/// A node's name in its parent.
#[derive(Clone, Copy, Debug)]
enum Name {
    Root,
    /// An object key, verbatim.
    Key(Span),
    /// An array element, named by its index.
    Index(u32),
}

impl Name {
    /// Bytes the name takes in the VM layout (with its NUL; empty names are null).
    fn layout_len(self) -> usize {
        match self {
            Self::Root => 0,
            Self::Key(s) if s.len() == 0 => 0,
            Self::Key(s) => s.len().saturating_add(1),
            Self::Index(i) => {
                let digits = i.checked_ilog10().map_or(1, |d| d.saturating_add(1));
                usize_from(digits).saturating_add(1)
            }
        }
    }
}

#[derive(Debug)]
enum Kind {
    /// A string, with its escapes still encoded.
    Str(Span),
    /// A number (or any other unquoted word), as written.
    Num(Span),
    Object,
    Array,
    True,
    False,
    Null,
}

#[derive(Debug)]
struct Node {
    name: Name,
    kind: Kind,
    children: Vec<Node>,
}

/// Why a document was not parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    /// Not (FTE-flavoured) JSON.
    Invalid,
    /// Its VM layout would exceed the limit.
    TooLarge,
}

/// A parsed document: the root (childless unless built) and the VM layout's node count and name
/// bytes.
#[derive(Debug)]
struct Parsed {
    root: Node,
    nodes: usize,
    names: usize,
}

struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
    /// Keep the tree; otherwise only validate and measure the document.
    build: bool,
    nodes: usize,
    names: usize,
    /// Most bytes the VM layout (nodes and names) may take.
    limit: usize,
    too_large: bool,
}

impl Parser<'_> {
    fn at(&self, i: usize) -> u8 {
        self.data.get(i).copied().unwrap_or(0)
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn bump(&mut self) {
        self.pos = self.pos.saturating_add(1);
    }

    fn span(&self, start: usize) -> Span {
        let to = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
        Span { start: to(start), end: to(self.pos) }
    }

    /// Skips whitespace and C/C++ comments.
    fn skip_white(&mut self) {
        while let Some(c) = self.peek() {
            match c {
                b' ' | b'\t' | b'\r' | b'\n' => self.bump(),
                b'/' if self.pos.saturating_add(1) < self.data.len() => {
                    match self.at(self.pos.saturating_add(1)) {
                        b'/' => {
                            self.pos = self.pos.saturating_add(2);
                            while self.peek().is_some_and(|c| c != b'\r' && c != b'\n') {
                                self.bump();
                            }
                        }
                        b'*' => {
                            self.pos = self.pos.saturating_add(2);
                            while self.pos.saturating_add(1) < self.data.len() {
                                if self.at(self.pos) == b'*'
                                    && self.at(self.pos.saturating_add(1)) == b'/'
                                {
                                    self.pos = self.pos.saturating_add(2);
                                    break;
                                }
                                self.bump();
                            }
                        }
                        _ => return,
                    }
                }
                _ => return,
            }
        }
    }

    /// A quoted string (the body, escapes undecoded) or an unquoted word.
    fn token(&mut self) -> Option<(Span, bool)> {
        if self.peek() == Some(b'"') {
            self.bump();
            let start = self.pos;
            while let Some(c) = self.peek() {
                match c {
                    b'"' => break,
                    b'\\' => match self.at(self.pos.saturating_add(1)) {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' | b'u' => {
                            self.pos = self.pos.saturating_add(2);
                        }
                        _ => self.bump(),
                    },
                    _ => self.bump(),
                }
            }
            if self.peek() != Some(b'"') {
                return None;
            }
            let body = self.span(start);
            self.bump();
            Some((body, true))
        } else {
            let start = self.pos;
            while self.peek().is_some_and(|c| !b" \t\r\n:,{}[]".contains(&c)) {
                self.bump();
            }
            (self.pos > start).then(|| (self.span(start), false))
        }
    }

    /// Counts a node against the layout limit.
    fn measure(&mut self, name: Name) -> bool {
        self.nodes = self.nodes.saturating_add(1);
        self.names = self.names.saturating_add(name.layout_len());
        let size = self.nodes.saturating_mul(NODE).saturating_add(self.names);
        self.too_large = size > self.limit;
        !self.too_large
    }

    fn node(&mut self, name: Name, depth: u32) -> Option<Node> {
        if depth > MAX_DEPTH || !self.measure(name) {
            return None;
        }
        self.skip_white();
        let mut node = Node { name, kind: Kind::Null, children: Vec::new() };
        match self.peek()? {
            b'{' => {
                self.bump();
                self.skip_white();
                node.kind = Kind::Object;
                while self.peek() == Some(b'"') {
                    let Some((key, _)) = self.token() else { break };
                    self.skip_white();
                    if self.peek() == Some(b':') {
                        self.bump();
                        match self.node(Name::Key(key), depth.saturating_add(1)) {
                            Some(child) if self.build => node.children.push(child),
                            Some(_) => {}
                            None => break,
                        }
                    }
                    self.skip_white();
                    if self.peek() == Some(b',') {
                        self.bump();
                        self.skip_white();
                        continue;
                    }
                    break;
                }
                (self.peek() == Some(b'}') && !self.too_large).then(|| {
                    self.bump();
                    node
                })
            }
            b'[' => {
                self.bump();
                self.skip_white();
                node.kind = Kind::Array;
                let mut index = 0u32;
                loop {
                    let name = Name::Index(index);
                    index = index.saturating_add(1);
                    match self.node(name, depth.saturating_add(1)) {
                        Some(child) if self.build => node.children.push(child),
                        Some(_) => {}
                        None => break,
                    }
                    if self.peek() == Some(b',') {
                        self.bump();
                        self.skip_white();
                        continue;
                    }
                    break;
                }
                self.skip_white();
                (self.peek() == Some(b']') && !self.too_large).then(|| {
                    self.bump();
                    node
                })
            }
            _ => {
                let (text, quoted) = self.token()?;
                let word = text.of(self.data);
                node.kind = if quoted {
                    Kind::Str(text)
                } else if word.eq_ignore_ascii_case(b"true") {
                    Kind::True
                } else if word.eq_ignore_ascii_case(b"false") {
                    Kind::False
                } else if word.eq_ignore_ascii_case(b"null") {
                    Kind::Null
                } else {
                    Kind::Num(text)
                };
                Some(node)
            }
        }
    }
}

/// Parses a whole document (trailing garbage fails it) whose VM layout takes at most `limit`
/// bytes. With `build` false nothing is allocated: the document is only validated and measured.
fn parse(data: &[u8], build: bool, limit: usize) -> Result<Parsed, Failure> {
    if u32::try_from(data.len()).is_err() {
        return Err(Failure::TooLarge);
    }
    let pos = if data.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 };
    let mut p = Parser { data, pos, build, nodes: 0, names: 0, limit, too_large: false };
    let root = p.node(Name::Root, 0);
    p.skip_white();
    match root {
        _ if p.too_large => Err(Failure::TooLarge),
        Some(root) if p.pos == data.len() => Ok(Parsed { root, nodes: p.nodes, names: p.names }),
        _ => Err(Failure::Invalid),
    }
}

fn hex(c: u8) -> Option<u32> {
    char::from(c).to_digit(16)
}

/// Appends code point `c` as (modified) UTF-8: NUL becomes `C0 80`, surrogates are encoded like
/// any other value.
fn push_utf8(out: &mut Vec<u8>, c: u32) {
    let byte = |v: u32| u8::try_from(v & 0xFF).unwrap_or(0);
    match c {
        0 => out.extend_from_slice(&[0xC0, 0x80]),
        1..=0x7F => out.push(byte(c)),
        0x80..=0x7FF => out.extend_from_slice(&[byte(0xC0 | (c >> 6)), byte(0x80 | (c & 0x3F))]),
        0x800..=0xFFFF => out.extend_from_slice(&[
            byte(0xE0 | (c >> 12)),
            byte(0x80 | ((c >> 6) & 0x3F)),
            byte(0x80 | (c & 0x3F)),
        ]),
        _ => out.extend_from_slice(&[
            byte(0xF0 | (c >> 18)),
            byte(0x80 | ((c >> 12) & 0x3F)),
            byte(0x80 | ((c >> 6) & 0x3F)),
            byte(0x80 | (c & 0x3F)),
        ]),
    }
}

/// Four hex digits at `s[i..]`.
fn hex4(s: &[u8], i: usize) -> Option<u32> {
    let d = s.get(i..i.checked_add(4)?)?;
    d.iter().try_fold(0u32, |acc, &c| Some((acc << 4) | hex(c)?))
}

/// Decodes a string body's escapes. Unknown escapes are kept verbatim (backslash included).
fn unescape(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0usize;
    while let Some(&c) = body.get(i) {
        i = i.saturating_add(1);
        if c != b'\\' {
            out.push(c);
            continue;
        }
        let Some(&e) = body.get(i) else {
            out.push(b'\\');
            break;
        };
        i = i.saturating_add(1);
        let plain = match e {
            b'"' => Some(b'"'),
            b'\\' => Some(b'\\'),
            b'/' => Some(b'/'),
            b'b' => Some(0x08),
            b'f' => Some(0x0C),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            _ => None,
        };
        if let Some(p) = plain {
            out.push(p);
            continue;
        }
        if e == b'u'
            && let Some(mut code) = hex4(body, i)
        {
            i = i.saturating_add(4);
            if (0xD800..0xDC00).contains(&code)
                && body.get(i) == Some(&b'\\')
                && body.get(i.saturating_add(1)) == Some(&b'u')
                && let Some(low) = hex4(body, i.saturating_add(2))
                && (0xDC00..0xE000).contains(&low)
            {
                i = i.saturating_add(6);
                let high = code.wrapping_sub(0xD800).wrapping_shl(10);
                code = 0x10000u32.wrapping_add(high).wrapping_add(low.wrapping_sub(0xDC00));
            }
            push_utf8(&mut out, code);
            continue;
        }
        out.push(b'\\');
        out.push(e);
    }
    out
}

/// C's `atof` on a byte string (leading whitespace, sign, decimal or hexadecimal digits,
/// `inf`/`infinity`/`nan`); 0 if nothing parses.
pub(crate) fn c_atof(s: &[u8]) -> f64 {
    let s = s.trim_ascii_start();
    let (neg, body) = match s.split_first() {
        Some((b'-', rest)) => (true, rest),
        Some((b'+', rest)) => (false, rest),
        _ => (false, s),
    };
    let lower: Vec<u8> = body.iter().take(8).map(u8::to_ascii_lowercase).collect();
    let v = if lower.starts_with(b"inf") {
        f64::INFINITY
    } else if lower.starts_with(b"nan") {
        f64::NAN
    } else if lower.starts_with(b"0x") {
        hex_float(body.get(2..).unwrap_or_default())
    } else {
        decimal_float(body)
    };
    if neg { -v } else { v }
}

/// The longest decimal floating-point prefix of `s`.
fn decimal_float(s: &[u8]) -> f64 {
    let digits = |from: usize| {
        s.get(from..).map_or(0, |t| t.iter().take_while(|c| c.is_ascii_digit()).count())
    };
    let mut end = digits(0);
    let mut mantissa_digits = end;
    if s.get(end) == Some(&b'.') {
        let frac = digits(end.saturating_add(1));
        mantissa_digits = mantissa_digits.saturating_add(frac);
        end = end.saturating_add(1).saturating_add(frac);
    }
    if mantissa_digits == 0 {
        return 0.0;
    }
    if matches!(s.get(end), Some(b'e' | b'E')) {
        let mut e = end.saturating_add(1);
        if matches!(s.get(e), Some(b'+' | b'-')) {
            e = e.saturating_add(1);
        }
        let n = digits(e);
        if n > 0 {
            end = e.saturating_add(n);
        }
    }
    std::str::from_utf8(s.get(..end).unwrap_or_default())
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or(0.0)
}

/// A hexadecimal floating-point number (after `0x`), like `strtod`.
fn hex_float(s: &[u8]) -> f64 {
    let mut value = 0.0f64;
    let mut scale = 0i32;
    let mut i = 0usize;
    let mut seen = false;
    while let Some(d) = s.get(i).copied().and_then(hex) {
        value = value * 16.0 + f64::from(d);
        seen = true;
        i = i.saturating_add(1);
    }
    if s.get(i) == Some(&b'.') {
        i = i.saturating_add(1);
        while let Some(d) = s.get(i).copied().and_then(hex) {
            value = value * 16.0 + f64::from(d);
            scale = scale.saturating_sub(4);
            seen = true;
            i = i.saturating_add(1);
        }
    }
    if !seen {
        return 0.0;
    }
    if matches!(s.get(i), Some(b'p' | b'P')) {
        let rest = s.get(i.saturating_add(1)..).unwrap_or_default();
        let (neg, digits) = match rest.split_first() {
            Some((b'-', r)) => (true, r),
            Some((b'+', r)) => (false, r),
            _ => (false, rest),
        };
        let mut exp = 0i32;
        for &c in digits.iter().take_while(|c| c.is_ascii_digit()) {
            exp = exp.saturating_mul(10).saturating_add(i32::from(c.wrapping_sub(b'0')));
        }
        scale = scale.saturating_add(if neg { exp.saturating_neg() } else { exp });
    }
    value * libm::exp2(f64::from(scale))
}

/// C's `atoi`: `strtol` in base 10, truncated to `int`.
pub(crate) fn c_atoi(s: &[u8]) -> i32 {
    let s = s.trim_ascii_start();
    let (neg, digits) = match s.split_first() {
        Some((b'-', rest)) => (true, rest),
        Some((b'+', rest)) => (false, rest),
        _ => (false, s),
    };
    let mut v: i64 = 0;
    for &c in digits.iter().take_while(|c| c.is_ascii_digit()) {
        v = v.saturating_mul(10).saturating_add(i64::from(c.wrapping_sub(b'0')));
    }
    let v = if neg { v.saturating_neg() } else { v };
    // `long` → `int` keeps the low 32 bits.
    i32::from_ne_bytes(v.to_ne_bytes().get(..4).and_then(|b| b.try_into().ok()).unwrap_or([0; 4]))
}

// ---- layout -----------------------------------------------------------------------------------

struct Layout<'a, H: Host> {
    vm: &'a mut Vm<H>,
    /// The document text the tree's spans refer to.
    data: &'a [u8],
    /// The block being built; `base` is its VM address.
    out: Vec<u8>,
    base: u32,
    next_node: usize,
    next_name: usize,
}

impl<H: Host> Layout<'_, H> {
    fn addr(&self, offset: usize) -> u32 {
        u32::try_from(offset).ok().and_then(|o| self.base.checked_add(o)).unwrap_or(0)
    }

    fn put(&mut self, at: usize, bytes: &[u8]) {
        if let Some(dst) = at.checked_add(bytes.len()).and_then(|end| self.out.get_mut(at..end)) {
            dst.copy_from_slice(bytes);
        }
    }

    fn node(&mut self, node: &Node, slot: usize) -> Result<(), VmError> {
        let at = slot.saturating_mul(NODE);
        let index;
        let name = match node.name {
            Name::Root => &[][..],
            Name::Key(key) => key.of(self.data),
            Name::Index(i) => {
                index = i.to_string();
                index.as_bytes()
            }
        };
        if !name.is_empty() {
            let name_at = self.next_name;
            self.put(name_at, name);
            self.next_name = name_at.saturating_add(name.len()).saturating_add(1);
            let r = self.addr(name_at);
            self.put(at.saturating_add(4), &r.to_le_bytes());
        }
        let number = |v: f64| v.to_le_bytes();
        let ty = match &node.kind {
            Kind::Str(body) => {
                let s = self.vm.temp(&unescape(body.of(self.data)))?;
                self.put(at.saturating_add(8), &s.0.to_le_bytes());
                TYPE_STRING
            }
            Kind::Num(text) => {
                let text = text.of(self.data);
                // FTE reads at most 63 characters.
                let v = c_atof(text.get(..text.len().min(63)).unwrap_or_default());
                self.put(at.saturating_add(8), &number(v));
                TYPE_NUMBER
            }
            Kind::True => {
                self.put(at.saturating_add(8), &number(1.0));
                TYPE_TRUE
            }
            Kind::False => TYPE_FALSE,
            Kind::Null => TYPE_NULL,
            Kind::Object | Kind::Array => {
                let first = self.next_node;
                self.next_node = first.saturating_add(node.children.len());
                let child_ptr = self.addr(first.saturating_mul(NODE));
                let n = u32::try_from(node.children.len()).unwrap_or(u32::MAX);
                self.put(at.saturating_add(8), &child_ptr.to_le_bytes());
                self.put(at.saturating_add(12), &n.to_le_bytes());
                for (k, child) in node.children.iter().enumerate() {
                    self.node(child, first.saturating_add(k))?;
                }
                if matches!(node.kind, Kind::Object) { TYPE_OBJECT } else { TYPE_ARRAY }
            }
        };
        self.put(at, &ty.to_le_bytes());
        Ok(())
    }
}

// ---- nodes ------------------------------------------------------------------------------------

/// A node as read from VM memory.
#[derive(Clone, Copy, Debug)]
struct NodeView {
    ty: u32,
    name: u32,
    a: u32,
    b: u32,
}

impl NodeView {
    /// The node FTE substitutes for null and bad pointers.
    const NULL: Self = Self { ty: TYPE_NULL, name: 0, a: 0, b: 0 };

    fn num(self) -> f64 {
        f64::from_bits(u64::from(self.a) | (u64::from(self.b) << 32))
    }

    fn is_container(self) -> bool {
        self.ty == TYPE_OBJECT || self.ty == TYPE_ARRAY
    }
}

fn read_node(core: &Core, p: u32) -> Option<NodeView> {
    let w = |k: u32| core.mem.read_u32(p.checked_add(k)?);
    Some(NodeView { ty: w(0)?, name: w(4)?, a: w(8)?, b: w(12)? })
}

/// The node argument `i` points at: the null node for a null pointer; a builtin error (then the
/// null node, as FTE) for a pointer outside VM memory.
fn node_arg<H: Host>(vm: &mut Vm<H>, i: usize) -> Result<NodeView, VmError> {
    let p = vm.arg_u32(i);
    if p == 0 {
        return Ok(NodeView::NULL);
    }
    match read_node(&vm.core, p) {
        Some(n) => Ok(n),
        None => {
            soft_error(vm, format!("json: bad node pointer {p:#x}"))?;
            Ok(NodeView::NULL)
        }
    }
}

// ---- builtins ---------------------------------------------------------------------------------

/// `jsonnode json_parse(string json)`: parses a document into a new heap block and returns its
/// root node, or null if the text is not valid (FTE-flavoured) JSON.
pub fn json_parse<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let text = vm.arg_str(0).to_vec();
    // Validate and measure first, allocating nothing, so a document whose layout cannot fit the
    // heap costs no memory; only then build the tree.
    let limit = usize_from(vm.core.config.limits.heap_bytes);
    let parsed = match parse(&text, false, limit).and_then(|_| parse(&text, true, limit)) {
        Ok(parsed) => parsed,
        Err(Failure::Invalid) => {
            vm.ret_raw([0; 3]);
            return Ok(());
        }
        Err(Failure::TooLarge) => {
            vm.ret_raw([0; 3]);
            return soft_error(vm, "json_parse: out of memory");
        }
    };
    let Parsed { root, nodes, names } = parsed;
    let size = nodes.saturating_mul(NODE).saturating_add(names);
    let Some(base) = u32::try_from(size).ok().and_then(|n| heap_alloc(&mut vm.core, n)) else {
        vm.ret_raw([0; 3]);
        return soft_error(vm, "json_parse: out of memory");
    };
    let mut out = Vec::new();
    if out.try_reserve_exact(size).is_err() {
        super::memory::heap_free(&mut vm.core, base);
        vm.ret_raw([0; 3]);
        return soft_error(vm, "json_parse: out of memory");
    }
    out.resize(size, 0);
    let next_name = nodes.saturating_mul(NODE);
    let mut layout = Layout { vm, data: &text, out, base, next_node: 1, next_name };
    let laid_out = layout.node(&root, 0);
    let Layout { vm, out, .. } = layout;
    if let Err(e) = laid_out {
        // Out of temp strings: do not leak the block.
        super::memory::heap_free(&mut vm.core, base);
        return Err(e);
    }
    vm.write_mem(crate::value::Ptr(base), &out);
    vm.ret_raw([base, 0, 0]);
    Ok(())
}

/// `json_type_e json_get_value_type(jsonnode node)`: 0 string, 1 number, 2 object, 3 array,
/// 4 true, 5 false, 6 null (also for a null node).
pub fn json_get_value_type<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = node_arg(vm, 0)?;
    vm.ret_raw([n.ty, 0, 0]);
    Ok(())
}

/// `string json_get_name(jsonnode node)`: the node's key in its parent (array elements are named
/// by their index), or null for the root.
pub fn json_get_name<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = node_arg(vm, 0)?;
    vm.ret_raw([n.name, 0, 0]);
    Ok(())
}

/// `int json_get_integer(jsonnode node)`: numbers truncated (1/0 for true/false), strings parsed
/// with `atoi`, 0 otherwise.
pub fn json_get_integer<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = node_arg(vm, 0)?;
    let v = match n.ty {
        TYPE_NUMBER | TYPE_TRUE | TYPE_FALSE => super::math::d2i(n.num()),
        TYPE_STRING => c_atoi(vm.core.str_bytes(n.a).unwrap_or_default()),
        _ => 0,
    };
    vm.ret_i32(v);
    Ok(())
}

/// `float json_get_float(jsonnode node)`: numbers (1/0 for true/false), strings parsed with
/// `atof`, 0 otherwise.
pub fn json_get_float<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = node_arg(vm, 0)?;
    let v = match n.ty {
        TYPE_NUMBER | TYPE_TRUE | TYPE_FALSE => n.num() as f32,
        TYPE_STRING => c_atof(vm.core.str_bytes(n.a).unwrap_or_default()) as f32,
        _ => 0.0,
    };
    vm.ret_f32(v);
    Ok(())
}

/// `string json_get_string(jsonnode node)`: the text of a string node, else null.
pub fn json_get_string<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = node_arg(vm, 0)?;
    let s = if n.ty == TYPE_STRING { n.a } else { 0 };
    vm.ret_raw([s, 0, 0]);
    Ok(())
}

/// `jsonnode json_find_object_child(jsonnode node, string key)`: the first child of an object
/// (or array, whose elements are named by index) with that exact name, or null.
pub fn json_find_object_child<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = node_arg(vm, 0)?;
    let key = vm.arg_str(1).to_vec();
    let mut found = 0;
    if n.is_container() {
        for k in 0..n.b {
            let Some(p) = k.checked_mul(16).and_then(|o| n.a.checked_add(o)) else { break };
            let Some(child) = read_node(&vm.core, p) else {
                soft_error(vm, format!("json: bad node pointer {p:#x}"))?;
                break;
            };
            if vm.core.str_bytes(child.name).unwrap_or_default() == key.as_slice() {
                found = p;
                break;
            }
        }
    }
    vm.ret_raw([found, 0, 0]);
    Ok(())
}

/// `int json_get_length(jsonnode node)`: the number of children of an object or array, else 0.
pub fn json_get_length<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = node_arg(vm, 0)?;
    vm.ret_raw([if n.is_container() { n.b } else { 0 }, 0, 0]);
    Ok(())
}

/// `jsonnode json_get_child_at_index(jsonnode node, int index)`: the `index`th child of an
/// object or array, or null if out of range.
pub fn json_get_child_at_index<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = node_arg(vm, 0)?;
    let index = vm.arg_u32(1);
    let p =
        if n.is_container() && index < n.b { n.a.wrapping_add(index.wrapping_mul(16)) } else { 0 };
    vm.ret_raw([p, 0, 0]);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(c_atof(b"  -1.5e2x"), -150.0);
        assert_eq!(c_atof(b"0x10"), 16.0);
        assert_eq!(c_atof(b"0x1p-1"), 0.5);
        assert!(c_atof(b"Infinity").is_infinite());
        assert!(c_atof(b"nan").is_nan());
        assert_eq!(c_atof(b".5"), 0.5);
        assert_eq!(c_atof(b"abc"), 0.0);
        assert_eq!(c_atof(b"1e"), 1.0);
        assert_eq!(c_atoi(b" 42abc"), 42);
        assert_eq!(c_atoi(b"-7"), -7);
        assert_eq!(c_atoi(b"4294967297"), 1);
        assert_eq!(c_atoi(b"99999999999999999999"), -1);
    }

    #[test]
    fn escapes() {
        assert_eq!(unescape(br#"a\"b\\c\/d\n\t"#), b"a\"b\\c/d\n\t");
        assert_eq!(unescape(b"\x5cu00e9"), "\u{e9}".as_bytes());
        assert_eq!(unescape(b"\x5cud83d\x5cude00"), "\u{1F600}".as_bytes());
        assert_eq!(unescape(br"\u0000"), [0xC0, 0x80]);
        assert_eq!(unescape(br"\q\uZZ"), br"\q\uZZ");
        assert_eq!(unescape(b"end\\"), b"end\\");
    }

    fn valid(data: &[u8]) -> bool {
        let checked = parse(data, false, usize::MAX).is_ok();
        assert_eq!(parse(data, true, usize::MAX).is_ok(), checked);
        checked
    }

    #[test]
    fn grammar() {
        assert!(!valid(b""));
        assert!(valid(b"{}"));
        assert!(valid(b"[1,]"));
        assert!(valid(b"{\"a\":1,}"));
        assert!(!valid(b"[1 2]"));
        assert!(!valid(b"{} x"));
        assert!(valid(b"\xEF\xBB\xBF // hi\n /* c */ [true, NULL, False]"));
        assert!(!valid(b"\"unterminated"));
        let deep = [b'['; 1000];
        assert!(!valid(&deep));
    }

    /// The layout size is known without building anything, and exceeding the limit stops the
    /// parse early.
    #[test]
    fn layout_size_and_limit() {
        let doc = br#"{"ab": [1, "x"], "": null}"#;
        let p = parse(doc, false, usize::MAX).unwrap();
        assert!(p.root.children.is_empty(), "nothing is built when measuring");
        // Root, "ab", its two elements and the empty-named null: 5 nodes; names "ab", "0", "1".
        assert_eq!((p.nodes, p.names), (5, 3 + 2 + 2));
        let built = parse(doc, true, usize::MAX).unwrap();
        assert_eq!(built.root.children.len(), 2);
        let size = 5 * NODE + 7;
        assert!(parse(doc, false, size).is_ok());
        assert_eq!(parse(doc, false, size - 1).unwrap_err(), Failure::TooLarge);
        let wide = format!("[{}]", "0,".repeat(100_000));
        assert_eq!(parse(wide.as_bytes(), true, 64 * 1024).unwrap_err(), Failure::TooLarge);
    }
}
