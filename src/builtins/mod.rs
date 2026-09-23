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
        let number = builtin_number(self.numbering, name);
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

    /// The registered builtins, sorted by name, with the number each is bound to (`None` for
    /// builtins bound by name only).
    #[must_use]
    pub fn registered(&self) -> Vec<(&[u8], Option<u32>)> {
        let mut out: Vec<(&[u8], Option<u32>)> = self
            .by_name
            .iter()
            .map(|(name, &i)| {
                let number = self
                    .entries
                    .get(i)
                    .and_then(|e| e.number)
                    .filter(|n| self.by_number.get(n) == Some(&i));
                (&**name, number)
            })
            .collect();
        out.sort_unstable();
        out
    }

    fn insert(&mut self, name: &[u8], number: Option<u32>, func: BuiltinFn<H>) {
        let index = match self.by_name.get(name) {
            Some(&i) => {
                if let Some(entry) = self.entries.get_mut(i) {
                    // Unbind the old number, unless another builtin has taken it over since.
                    if let Some(old) = entry.number
                        && self.by_number.get(&old) == Some(&i)
                    {
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
}

mod numbers;

/// FTE's builtin number for `name` under `numbering`, if it has one. Builtins FTE resolves by
/// name (`= #0:name`) have none.
#[must_use]
pub fn builtin_number(numbering: Numbering, name: &str) -> Option<u32> {
    let table: &[(&str, u32)] = match numbering {
        Numbering::Csqc => numbers::CSQC,
        Numbering::Ssqc => numbers::SSQC,
        Numbering::Menu => numbers::MENU,
        Numbering::None => &[],
    };
    let found = table.binary_search_by(|(n, _)| (*n).cmp(name)).ok().and_then(|i| table.get(i));
    found.map(|&(_, number)| number).or_else(|| extra_number(numbering, name))
}

/// Every builtin FTE declares for `numbering`: numbered ones with their number, name-resolved
/// ones (`= #0:name`) with `None`.
#[must_use]
pub fn known_builtins(numbering: Numbering) -> Vec<(&'static str, Option<u32>)> {
    let (table, named): (&[(&str, u32)], &[&str]) = match numbering {
        Numbering::Csqc => (numbers::CSQC, numbers::CSQC_NAMED),
        Numbering::Ssqc => (numbers::SSQC, numbers::SSQC_NAMED),
        Numbering::Menu => (numbers::MENU, numbers::MENU_NAMED),
        Numbering::None => (&[], &[]),
    };
    let mut out: Vec<_> = table.iter().map(|&(n, num)| (n, Some(num))).collect();
    out.extend(named.iter().map(|&n| (n, None)));
    out
}

/// Builtins in FTE's tables that the platform dump does not declare for that VM kind.
fn extra_number(numbering: Numbering, name: &str) -> Option<u32> {
    match (numbering, name) {
        (Numbering::Csqc, "fork") => Some(210),
        (Numbering::Csqc, "sleep") => Some(212),
        _ => None,
    }
}
