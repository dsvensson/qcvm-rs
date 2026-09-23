// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keeping the progs' autocvars (`autocvar_<name>` globals) in step with the host's cvars.

use crate::bytes::usize_from;
use crate::error::VmError;
use crate::host::Host;
use crate::progs::Type;
use crate::stdlib::convert::{atof, parse_vector, strtol};
use crate::vm::Vm;

impl<H: Host> Vm<H> {
    /// Copies the host's cvar values ([`Host::cvar_string`]) into the autocvars of every loaded
    /// progs, as FTE does when a progs loads and whenever a cvar changes; call it again after
    /// changes. Each value is parsed by the global's type: floats like `atof`, integers like
    /// `atoi`, vectors like `stov`, strings as they are. Autocvars of cvars the host does not
    /// have keep their current value (the progs' default until set).
    ///
    /// # Errors
    /// [`ErrorKind::OutOfMemory`](crate::ErrorKind::OutOfMemory) if a string value cannot be
    /// stored.
    pub fn sync_autocvars(&mut self, host: &mut H) -> Result<(), VmError> {
        let mut writes: Vec<(usize, Type, Vec<u8>)> = Vec::new();
        for ps in &self.core.progs {
            for cvar in ps.program.autocvars() {
                let Some(value) = host.cvar_string(cvar.name) else { continue };
                let at = usize_from(ps.gbase)
                    .saturating_add(usize_from(cvar.global.offset).saturating_mul(4));
                writes.push((at, cvar.global.ty, value.into_owned()));
            }
        }
        for (at, ty, text) in writes {
            match ty {
                Type::Float => self.core.mem.set_gf(at, atof(&text) as f32),
                Type::Integer | Type::UInt => {
                    // C's `atoi`: `strtol` truncated to `int`.
                    self.core.mem.set_g(at, strtol(&text, 10) as u32);
                }
                Type::Vector => self.core.mem.set_gv(at, parse_vector(&text)),
                Type::String => {
                    let s = self.new_temp(&text)?;
                    self.core.mem.set_g(at, s);
                }
                _ => {}
            }
        }
        Ok(())
    }
}
