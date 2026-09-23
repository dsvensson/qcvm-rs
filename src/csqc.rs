// SPDX-License-Identifier: MIT OR Apache-2.0

//! Helpers for hosts running client-side QuakeC.
//!
//! [`VmConfig::csqc`](crate::VmConfig::csqc) holds the CSQC defaults (entity reuse, the fields
//! cleared on removal, the `dimension_*` spawn defaults); this module has the pieces of FTE's
//! CSQC engine glue that are pure VM work.

use crate::error::VmError;
use crate::host::Host;
use crate::value::{EntRef, Field, FuncRef, Global};
use crate::vm::Vm;

/// The fields and globals [`add_entities`] uses, looked up once (after loading the progs) so the
/// per-frame walk does no name lookups.
#[derive(Clone, Copy, Debug)]
pub struct Handles {
    /// `.drawmask`.
    pub drawmask: Field<f32>,
    /// `.predraw`, if the progs defines it.
    pub predraw: Option<Field<FuncRef>>,
    /// The `self` global, if the progs defines it.
    pub self_global: Option<Global<EntRef>>,
}

impl Handles {
    /// Looks the handles up in `vm`'s main progs. `None` if it has no `.drawmask` field, in
    /// which case no QuakeC entity is ever drawn.
    #[must_use]
    pub fn resolve<H: Host>(vm: &Vm<H>) -> Option<Self> {
        Some(Self {
            drawmask: vm.field("drawmask").ok()?,
            predraw: vm.field("predraw").ok(),
            self_global: vm.global("self").ok(),
        })
    }
}

/// FTE's `addentities` walk over QuakeC entities.
///
/// Visits every in-use entity except the world, in slot order, whose `drawmask` shares a bit with
/// `mask`. If the entity has a `predraw` function it is called with `self` set to the entity; an
/// entity whose predraw returns non-zero, or removes it, is skipped. Every other visited entity is
/// handed to `add`, which should copy it into the scene. Entities spawned during the walk are not
/// visited. Engine entities (`MASK_ENGINE`, `MASK_VIEWMODEL`) are the host's business.
///
/// # Errors
/// The first error raised by a predraw function; the walk stops there.
pub fn add_entities<H: Host>(
    vm: &mut Vm<H>,
    host: &mut H,
    handles: &Handles,
    mask: i32,
    mut add: impl FnMut(&mut Vm<H>, &mut H, EntRef),
) -> Result<(), VmError> {
    for index in 1..vm.num_edicts() {
        let e = EntRef(index);
        if !vm.is_in_use(e) {
            continue;
        }
        let bits = vm.get_field(e, handles.drawmask).map_or(0, crate::vm::num::f2i);
        if bits & mask == 0 {
            continue;
        }
        let predraw = handles.predraw.and_then(|f| vm.get_field(e, f)).filter(|f| f.index() != 0);
        if let Some(f) = predraw {
            let ret = match handles.self_global {
                Some(self_g) => vm.call_as_with(host, self_g, e, f, &[])?,
                None => vm.call(host, f, &[])?,
            };
            if ret.f32() != 0.0 || !vm.is_in_use(e) {
                continue;
            }
        }
        add(vm, host, e);
    }
    Ok(())
}
