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
//! pre-order; the names follow the nodes. Objects keep their members in document order,
//! duplicate keys included.
//!
//! Documents must be strict JSON (RFC 8259, parsed by `serde_json`; nesting up to 128 levels);
//! a leading UTF-8 byte-order mark is skipped. FTE's parser is more lenient (comments, trailing
//! commas, unquoted words, keys used verbatim). Keys and strings are unescaped; a NUL is stored
//! as the overlong `C0 80`, since it would end a QuakeC string. A document is measured before
//! anything is built, so one whose tree cannot fit the heap costs no memory.

use std::borrow::Cow;
use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};

use crate::builtins::Builtins;
use crate::bytes::usize_from;
use crate::error::VmError;
use crate::host::Host;
use crate::vm::Vm;
use crate::vm::core::Core;

use super::convert::{atof, strtol};
use super::introspect::soft_error;
use super::memory::heap_alloc;

/// C's `atoi`: `strtol` in base 10, truncated to `int`.
fn atoi(s: &[u8]) -> i32 {
    strtol(s, 10) as i32
}

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

// ---- parsing ----------------------------------------------------------------------------------

/// Why a document was not parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    /// Not valid JSON.
    Invalid,
    /// Its VM layout would exceed the limit.
    TooLarge,
}

/// Text as stored in VM memory: a NUL (which would end a QuakeC string) becomes the overlong
/// `C0 80`.
fn qc_text(s: &str) -> Cow<'_, [u8]> {
    if !s.contains('\0') {
        return Cow::Borrowed(s.as_bytes());
    }
    let mut out = Vec::with_capacity(s.len().saturating_add(8));
    for &b in s.as_bytes() {
        if b == 0 {
            out.extend_from_slice(&[0xC0, 0x80]);
        } else {
            out.push(b);
        }
    }
    Cow::Owned(out)
}

/// Length of [`qc_text`]`(s)` without building it.
fn qc_len(s: &str) -> usize {
    s.len().saturating_add(s.bytes().filter(|&b| b == 0).count())
}

/// Two words as the 8 value bytes of a node.
fn words(a: u32, b: u32) -> [u8; 8] {
    (u64::from(a) | (u64::from(b) << 32)).to_le_bytes()
}

/// Bytes a name takes in the names area (with its NUL; an empty name is null and takes none).
fn name_bytes(len: usize) -> usize {
    if len == 0 { 0 } else { len.saturating_add(1) }
}

/// Decimal digits of an array index.
fn index_digits(i: u32) -> usize {
    usize_from(i.checked_ilog10().map_or(1, |d| d.saturating_add(1)))
}

/// A string (object keys), as its length in the names area.
struct KeyLen;

impl<'de> DeserializeSeed<'de> for KeyLen {
    type Value = usize;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<usize, D::Error> {
        d.deserialize_str(self)
    }
}

impl Visitor<'_> for KeyLen {
    type Value = usize;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a key")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<usize, E> {
        Ok(qc_len(v))
    }
}

/// Pass 1: validates and measures a document without building anything. Records the VM
/// layout's node count and name bytes, and the child count of every object and array in
/// pre-order; stops once the layout would exceed `limit`.
#[derive(Debug)]
struct Measure {
    nodes: usize,
    names: usize,
    limit: usize,
    counts: Vec<u32>,
    too_large: bool,
}

/// One node to measure, with the length of its name.
struct MeasureNode<'m> {
    m: &'m mut Measure,
    name: usize,
}

impl<'de> DeserializeSeed<'de> for MeasureNode<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        let m = self.m;
        m.nodes = m.nodes.saturating_add(1);
        m.names = m.names.saturating_add(name_bytes(self.name));
        if m.nodes.saturating_mul(NODE).saturating_add(m.names) > m.limit {
            m.too_large = true;
            return Err(de::Error::custom("the document does not fit the heap"));
        }
        d.deserialize_any(MeasureValue(m))
    }
}

struct MeasureValue<'m>(&'m mut Measure);

impl MeasureValue<'_> {
    /// Reserves a count slot in pre-order, for a container about to be measured.
    fn open(&mut self) -> usize {
        self.0.counts.push(0);
        self.0.counts.len().saturating_sub(1)
    }

    fn close(&mut self, slot: usize, n: u32) {
        if let Some(c) = self.0.counts.get_mut(slot) {
            *c = n;
        }
    }
}

impl<'de> Visitor<'de> for MeasureValue<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        let slot = self.open();
        let mut n = 0u32;
        while seq.next_element_seed(MeasureNode { m: self.0, name: index_digits(n) })?.is_some() {
            n = n.saturating_add(1);
        }
        self.close(slot, n);
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(mut self, mut map: A) -> Result<(), A::Error> {
        let slot = self.open();
        let mut n = 0u32;
        while let Some(key) = map.next_key_seed(KeyLen)? {
            map.next_value_seed(MeasureNode { m: self.0, name: key })?;
            n = n.saturating_add(1);
        }
        self.close(slot, n);
        Ok(())
    }
}

/// Measures `text` (see [`Measure`]).
fn measure(text: &[u8], limit: usize) -> Result<Measure, Failure> {
    let mut m = Measure { nodes: 0, names: 0, limit, counts: Vec::new(), too_large: false };
    let mut d = serde_json::Deserializer::from_slice(text);
    let parsed = MeasureNode { m: &mut m, name: 0 }.deserialize(&mut d).and_then(|()| d.end());
    match parsed {
        Ok(()) => Ok(m),
        Err(_) if m.too_large => Err(Failure::TooLarge),
        Err(_) => Err(Failure::Invalid),
    }
}

// ---- layout -----------------------------------------------------------------------------------

/// Pass 2: writes the document into the block being built, in the layout [`Measure`] sized.
struct Writer<'a, H: Host> {
    vm: &'a mut Vm<H>,
    /// The block being built; `base` is its VM address.
    out: Vec<u8>,
    base: u32,
    next_node: usize,
    next_name: usize,
    /// Child counts of the objects and arrays, in pre-order (from [`Measure`]).
    counts: std::vec::IntoIter<u32>,
    /// A VM error (out of temp strings) that stopped the write.
    failed: Option<VmError>,
}

impl<H: Host> Writer<'_, H> {
    fn addr(&self, offset: usize) -> u32 {
        u32::try_from(offset).ok().and_then(|o| self.base.checked_add(o)).unwrap_or(0)
    }

    fn put(&mut self, at: usize, bytes: &[u8]) {
        if let Some(dst) = at.checked_add(bytes.len()).and_then(|end| self.out.get_mut(at..end)) {
            dst.copy_from_slice(bytes);
        }
    }

    /// Stores a name in the names area; returns its address (null for an empty name).
    fn name(&mut self, name: &[u8]) -> u32 {
        if name.is_empty() {
            return 0;
        }
        let at = self.next_name;
        self.put(at, name);
        self.next_name = at.saturating_add(name.len()).saturating_add(1);
        self.addr(at)
    }

    /// Writes node `slot`'s type and name, and the 8 bytes after them.
    fn node(&mut self, slot: usize, ty: u32, name: u32, value: [u8; 8]) {
        let at = slot.saturating_mul(NODE);
        self.put(at, &ty.to_le_bytes());
        self.put(at.saturating_add(4), &name.to_le_bytes());
        self.put(at.saturating_add(8), &value);
    }

    /// Reserves the child slots of a container: `(first slot, count)`.
    fn children(&mut self) -> (usize, u32) {
        let n = self.counts.next().unwrap_or(0);
        let first = self.next_node;
        self.next_node = first.saturating_add(usize_from(n));
        (first, n)
    }

    fn container(&mut self, slot: usize, ty: u32, name: u32, first: usize, n: u32) {
        let first = self.addr(first.saturating_mul(NODE));
        self.node(slot, ty, name, words(first, n));
    }
}

/// A key, stored in the names area; its value is the name's address.
struct WriteKey<'w, 'a, H: Host>(&'w mut Writer<'a, H>);

impl<'de, H: Host> DeserializeSeed<'de> for WriteKey<'_, '_, H> {
    type Value = u32;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<u32, D::Error> {
        d.deserialize_str(self)
    }
}

impl<H: Host> Visitor<'_> for WriteKey<'_, '_, H> {
    type Value = u32;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a key")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<u32, E> {
        Ok(self.0.name(&qc_text(v)))
    }
}

/// One node to write into `slot`, named by the name at address `name`.
struct WriteNode<'w, 'a, H: Host> {
    w: &'w mut Writer<'a, H>,
    slot: usize,
    name: u32,
}

impl<'de, H: Host> DeserializeSeed<'de> for WriteNode<'_, '_, H> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de, H: Host> Visitor<'de> for WriteNode<'_, '_, H> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<(), E> {
        let (ty, value) = if v { (TYPE_TRUE, 1.0f64) } else { (TYPE_FALSE, 0.0) };
        self.w.node(self.slot, ty, self.name, value.to_le_bytes());
        Ok(())
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<(), E> {
        self.visit_f64(v as f64)
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<(), E> {
        self.visit_f64(v as f64)
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<(), E> {
        self.w.node(self.slot, TYPE_NUMBER, self.name, v.to_le_bytes());
        Ok(())
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<(), E> {
        match self.w.vm.temp(&qc_text(v)) {
            Ok(s) => {
                self.w.node(self.slot, TYPE_STRING, self.name, words(s.0, 0));
                Ok(())
            }
            Err(e) => {
                self.w.failed = Some(e);
                Err(E::custom("out of temp strings"))
            }
        }
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        self.w.node(self.slot, TYPE_NULL, self.name, [0; 8]);
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let (first, n) = self.w.children();
        self.w.container(self.slot, TYPE_ARRAY, self.name, first, n);
        for k in 0..n {
            let name = self.w.name(k.to_string().as_bytes());
            let slot = first.saturating_add(usize_from(k));
            if seq.next_element_seed(WriteNode { w: self.w, slot, name })?.is_none() {
                return Err(de::Error::custom("the document changed between passes"));
            }
        }
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let (first, n) = self.w.children();
        self.w.container(self.slot, TYPE_OBJECT, self.name, first, n);
        for k in 0..n {
            let Some(name) = map.next_key_seed(WriteKey(self.w))? else {
                return Err(de::Error::custom("the document changed between passes"));
            };
            let slot = first.saturating_add(usize_from(k));
            map.next_value_seed(WriteNode { w: self.w, slot, name })?;
        }
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
    let arg = vm.arg_str(0).to_vec();
    let text = arg.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&arg);
    // Measure first, so a document whose tree cannot fit the heap costs no memory.
    let limit = usize_from(vm.core.config.limits.heap_bytes);
    let m = match measure(text, limit) {
        Ok(m) => m,
        Err(Failure::Invalid) => {
            vm.ret_raw([0; 3]);
            return Ok(());
        }
        Err(Failure::TooLarge) => {
            vm.ret_raw([0; 3]);
            return soft_error(vm, "json_parse: out of memory");
        }
    };
    let nodes_bytes = m.nodes.saturating_mul(NODE);
    let size = nodes_bytes.saturating_add(m.names);
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
    let mut w = Writer {
        vm,
        out,
        base,
        next_node: 1,
        next_name: nodes_bytes,
        counts: m.counts.into_iter(),
        failed: None,
    };
    let mut d = serde_json::Deserializer::from_slice(text);
    let written =
        WriteNode { w: &mut w, slot: 0, name: 0 }.deserialize(&mut d).and_then(|()| d.end());
    let Writer { vm, out, failed, .. } = w;
    if written.is_err() {
        // Out of temp strings: do not leak the block.
        super::memory::heap_free(&mut vm.core, base);
        vm.ret_raw([0; 3]);
        return failed.map_or(Ok(()), Err);
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
        TYPE_STRING => atoi(vm.core.str_bytes(n.a).unwrap_or_default()),
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
        TYPE_STRING => atof(vm.core.str_bytes(n.a).unwrap_or_default()) as f32,
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
        assert_eq!(atof(b"  -1.5e2x"), -150.0);
        assert_eq!(atof(b"0x10"), 16.0);
        assert_eq!(atof(b"0x1p-1"), 0.5);
        assert!(atof(b"Infinity").is_infinite());
        assert!(atof(b"nan").is_nan());
        assert_eq!(atof(b".5"), 0.5);
        assert_eq!(atof(b"abc"), 0.0);
        assert_eq!(atof(b"1e"), 1.0);
        assert_eq!(atoi(b" 42abc"), 42);
        assert_eq!(atoi(b"-7"), -7);
        assert_eq!(atoi(b"4294967297"), 1);
        assert_eq!(atoi(b"99999999999999999999"), -1);
    }

    fn valid(text: &str) -> bool {
        measure(text.as_bytes(), usize::MAX).is_ok()
    }

    #[test]
    fn strict_grammar() {
        assert!(valid("{}"));
        assert!(valid(" [1, \"two\", {\"k\": null}] "));
        assert!(valid("42"));
        for text in ["", "[1,]", "{\"a\":1,}", "[1 2]", "{} x", "// c\n[]", "[abc]", "[TRUE]"] {
            assert!(!valid(text), "{text:?}");
        }
        assert!(!valid(&("[".repeat(200) + &"]".repeat(200))), "nesting is bounded");
    }

    /// The layout size and the containers' child counts are known without building anything,
    /// and exceeding the limit stops the parse early.
    #[test]
    fn layout_size_and_limit() {
        let doc = br#"{"ab": [1, "x"], "": null, "n\u0000": {}}"#;
        let m = measure(doc, usize::MAX).unwrap();
        // Root, "ab", its two elements, the empty-named null and "n\0": 6 nodes; names "ab",
        // "0", "1" and "n" + C0 80 (the NUL of each counted).
        assert_eq!((m.nodes, m.names), (6, 3 + 2 + 2 + 4));
        assert_eq!(m.counts, [3, 2, 0], "pre-order: root, ab, n");
        let size = 6 * NODE + 11;
        assert!(measure(doc, size).is_ok());
        assert_eq!(measure(doc, size - 1).unwrap_err(), Failure::TooLarge);
        let wide = format!("[{}0]", "0,".repeat(100_000));
        assert_eq!(measure(wide.as_bytes(), 64 * 1024).unwrap_err(), Failure::TooLarge);
    }

    #[test]
    fn nul_is_stored_overlong() {
        assert_eq!(qc_text("a\0b"), &b"a\xC0\x80b"[..]);
        assert_eq!(qc_len("a\0b"), 4);
        assert_eq!(index_digits(0), 1);
        assert_eq!(index_digits(10), 2);
    }
}
