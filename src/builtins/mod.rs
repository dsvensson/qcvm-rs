// SPDX-License-Identifier: MIT OR Apache-2.0

//! Builtin registration and binding.
//!
//! A progs file refers to builtins either by number (`= #N`) or by name (`= #0`, resolved by the
//! function's own name). A [`Builtins`] registry maps both to host functions; each VM binds its
//! program's builtin stubs against it once, when the program is loaded. Stubs the registry does
//! not know fail only when QuakeC actually calls them.

use std::collections::HashMap;
use std::fmt;

use crate::error::VmError;
use crate::host::Host;
use crate::vm::Vm;

/// A builtin implementation.
///
/// It reads its arguments from the VM (`vm.arg_*`), writes its result (`vm.ret_*`) and may call
/// back into QuakeC with `vm.call`. Host state lives in `H`.
pub type BuiltinFn<H> = fn(&mut Vm<H>, &mut H) -> Result<(), VmError>;

/// Which builtin numbering a registry follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Numbering {
    /// FTE's client-side QuakeC numbers.
    #[default]
    Csqc,
    /// FTE's server-side QuakeC numbers.
    Ssqc,
    /// FTE's menu QuakeC numbers.
    Menu,
    /// No numbers: builtins bind by name only.
    None,
}

struct Entry<H> {
    name: Box<[u8]>,
    number: Option<u32>,
    func: BuiltinFn<H>,
}

impl<H> Clone for Entry<H> {
    fn clone(&self) -> Self {
        Self { name: self.name.clone(), number: self.number, func: self.func }
    }
}

/// A set of builtins, shared (in an `Arc`) by the VMs that use it.
pub struct Builtins<H> {
    numbering: Numbering,
    entries: Vec<Entry<H>>,
    by_name: HashMap<Box<[u8]>, usize>,
    by_number: HashMap<u32, usize>,
}

impl<H> Clone for Builtins<H> {
    fn clone(&self) -> Self {
        Self {
            numbering: self.numbering,
            entries: self.entries.clone(),
            by_name: self.by_name.clone(),
            by_number: self.by_number.clone(),
        }
    }
}

impl<H> fmt::Debug for Builtins<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Builtins")
            .field("numbering", &self.numbering)
            .field("count", &self.entries.len())
            .finish()
    }
}

impl<H: Host> Builtins<H> {
    /// An empty registry using `numbering` for builtins registered by name only.
    #[must_use]
    pub fn empty(numbering: Numbering) -> Self {
        Self { numbering, entries: Vec::new(), by_name: HashMap::new(), by_number: HashMap::new() }
    }

    /// The numbering this registry follows.
    #[must_use]
    pub fn numbering(&self) -> Numbering {
        self.numbering
    }

    /// Registers (or replaces) builtin `name` at number `number`.
    pub fn set_numbered(&mut self, number: u32, name: &str, func: BuiltinFn<H>) -> &mut Self {
        self.insert(name.as_bytes(), Some(number), func);
        self
    }

    /// Registers (or replaces) builtin `name`. Its number comes from the registry's numbering
    /// table; builtins without a number there bind by name only.
    pub fn set(&mut self, name: &str, func: BuiltinFn<H>) -> &mut Self {
        let number = numbers::lookup(self.numbering, name);
        self.insert(name.as_bytes(), number, func);
        self
    }

    /// Makes `alias` resolve (by name) to the same function as `target`.
    pub fn alias(&mut self, alias: &str, target: &str) -> &mut Self {
        if let Some(&i) = self.by_name.get(target.as_bytes())
            && let Some(func) = self.entries.get(i).map(|e| e.func)
        {
            self.insert(alias.as_bytes(), None, func);
        }
        self
    }

    /// Removes builtin `name`.
    pub fn remove(&mut self, name: &str) -> &mut Self {
        if let Some(i) = self.by_name.remove(name.as_bytes()) {
            self.by_number.retain(|_, v| *v != i);
        }
        self
    }

    /// Whether a builtin with this name is registered.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(name.as_bytes())
    }

    fn insert(&mut self, name: &[u8], number: Option<u32>, func: BuiltinFn<H>) {
        let index = match self.by_name.get(name) {
            Some(&i) => {
                if let Some(entry) = self.entries.get_mut(i) {
                    if let Some(old) = entry.number {
                        self.by_number.remove(&old);
                    }
                    *entry = Entry { name: name.into(), number, func };
                }
                i
            }
            None => {
                self.entries.push(Entry { name: name.into(), number, func });
                let i = self.entries.len().saturating_sub(1);
                self.by_name.insert(name.into(), i);
                i
            }
        };
        if let Some(n) = number {
            self.by_number.insert(n, index);
        }
    }

    /// Binds a `#N` builtin.
    pub(crate) fn bind_number(&self, number: u32) -> Option<u32> {
        self.by_number.get(&number).and_then(|&i| u32::try_from(i).ok())
    }

    /// Binds a `#0` builtin by name.
    pub(crate) fn bind_name(&self, name: &[u8]) -> Option<u32> {
        self.by_name.get(name).and_then(|&i| u32::try_from(i).ok())
    }

    /// The function in binding slot `slot`.
    pub(crate) fn func(&self, slot: u32) -> Option<BuiltinFn<H>> {
        self.entries.get(usize::try_from(slot).ok()?).map(|e| e.func)
    }

    /// The name registered in binding slot `slot`.
    pub(crate) fn name(&self, slot: u32) -> Option<&[u8]> {
        self.entries.get(usize::try_from(slot).ok()?).map(|e| &*e.name)
    }
}

/// FTE's builtin numbers per VM kind (filled in with the standard library).
pub(crate) mod numbers {
    use super::Numbering;

    pub(crate) fn lookup(numbering: Numbering, name: &str) -> Option<u32> {
        let table: &[(&str, u32)] = match numbering {
            Numbering::Csqc | Numbering::Ssqc | Numbering::Menu | Numbering::None => &[],
        };
        table.iter().find(|(n, _)| *n == name).map(|&(_, num)| num)
    }
}
