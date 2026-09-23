// SPDX-License-Identifier: MIT OR Apache-2.0

//! Every opcode, checked against `docs/spec/vm.md` (with the fixes in `deviations.md`).
//!
//! One test drives all cases so it can assert at the end that every opcode ran at least once,
//! in both the 16-bit and the 32-bit statement formats.

use std::collections::BTreeSet;
use std::sync::Arc;

use qcvm::{Builtins, EntRef, ErrorKind, Numbering, Op, Program, ProgsFormat, Vm, VmConfig};

use crate::support::asm::{Asm, OFS_RETURN, parm, ty};
use crate::vm_basic::TestHost;

type W = [u32; 3];

const MARK: u32 = 0xDEAD_BEEF;

fn f(x: f32) -> W {
    [x.to_bits(), 0, 0]
}
fn v(x: f32, y: f32, z: f32) -> W {
    [x.to_bits(), y.to_bits(), z.to_bits()]
}
fn i(x: i32) -> W {
    [x as u32, 0, 0]
}
fn u(x: u32) -> W {
    [x, 0, 0]
}
fn l(x: i64) -> W {
    [x as u32, (x as u64 >> 32) as u32, 0]
}
fn d(x: f64) -> W {
    let b = x.to_bits();
    [b as u32, (b >> 32) as u32, 0]
}
fn fb(b: bool) -> u32 {
    if b { 1.0f32.to_bits() } else { 0 }
}

/// Result of running one statement.
#[derive(Debug)]
struct Out {
    a: W,
    b: W,
    c: W,
    ret: W,
    err: Option<ErrorKind>,
}

struct Tester {
    format: ProgsFormat,
    covered: BTreeSet<u16>,
}

impl Tester {
    fn vm(&self, asm: &Asm) -> Vm<TestHost> {
        let program = Arc::new(Program::parse(&asm.build(self.format)).unwrap());
        Vm::new(program, Arc::new(Builtins::empty(Numbering::None)), VmConfig::default()).unwrap()
    }

    /// Runs `op A B C` on globals initialised to `a`, `b`, `c`.
    fn run(&mut self, op: Op, a: W, b: W, c: W) -> Out {
        self.run_with(op, a, b, c, |_| {})
    }

    /// Like [`Tester::run`], letting `setup` prepare the program first (it runs on a fresh
    /// assembler, so strings it interns get predictable offsets).
    fn run_with(&mut self, op: Op, a: W, b: W, c: W, setup: impl FnOnce(&mut Asm)) -> Out {
        self.covered.insert(op as u16);
        let mut asm = Asm::new();
        setup(&mut asm);
        let ga = asm.global("A", ty::VECTOR, &a);
        let gb = asm.global("B", ty::VECTOR, &b);
        let gc = asm.global("C", ty::VECTOR, &c);
        asm.function("t", &[], 0);
        asm.emit(op, ga, gb, gc);
        asm.emit(Op::Done, 0, 0, 0);
        self.exec(&asm)
    }

    fn exec(&self, asm: &Asm) -> Out {
        let mut vm = self.vm(asm);
        let mut host = TestHost::default();
        let t = vm.find_function("t").unwrap();
        let r = vm.call(&mut host, t, &[]);
        let get = |vm: &Vm<TestHost>, n: &str| {
            vm.global::<[f32; 3]>(n).map(|g| vm.get(g).map(f32::to_bits)).unwrap_or([0; 3])
        };
        Out {
            a: get(&vm, "A"),
            b: get(&vm, "B"),
            c: get(&vm, "C"),
            ret: r.as_ref().map(|r| r.0).unwrap_or([0; 3]),
            err: r.err().map(|e| e.kind().clone()),
        }
    }

    /// Checks the first words of C after `op A B`.
    fn c(&mut self, op: Op, a: W, b: W, want: &[u32]) {
        let out = self.run(op, a, b, [MARK; 3]);
        assert_eq!(out.err, None, "{op:?}");
        assert_eq!(&out.c[..want.len()], want, "{op:?} a={a:x?} b={b:x?} ({:?})", self.format);
    }

    /// Checks the first words of B after `op A B` (stores).
    fn b(&mut self, op: Op, a: W, want: &[u32]) {
        let out = self.run(op, a, [MARK; 3], [0; 3]);
        assert_eq!(out.err, None, "{op:?}");
        assert_eq!(&out.b[..want.len()], want, "{op:?} a={a:x?} ({:?})", self.format);
    }

    /// Checks that `op A B C` faults with `kind`.
    fn fault(&mut self, op: Op, a: W, b: W, c: W, kind: ErrorKind) {
        let out = self.run(op, a, b, c);
        assert_eq!(out.err, Some(kind), "{op:?}");
    }
}

fn arithmetic(t: &mut Tester) {
    t.c(Op::MulF, f(3.0), f(4.0), &[12.0f32.to_bits()]);
    t.c(Op::DivF, f(6.0), f(4.0), &[1.5f32.to_bits()]);
    t.c(Op::DivF, f(1.0), f(0.0), &[f32::INFINITY.to_bits()]);
    t.c(Op::AddF, f(1.5), f(2.25), &[3.75f32.to_bits()]);
    t.c(Op::SubF, f(1.5), f(2.25), &[(-0.75f32).to_bits()]);
    t.c(Op::MulV, v(1.0, 2.0, 3.0), v(4.0, 5.0, 6.0), &[32.0f32.to_bits()]);
    t.c(Op::MulFV, f(2.0), v(1.0, 2.0, 3.0), &v(2.0, 4.0, 6.0));
    t.c(Op::MulVF, v(1.0, 2.0, 3.0), f(2.0), &v(2.0, 4.0, 6.0));
    t.c(Op::DivVF, v(2.0, 4.0, 6.0), f(2.0), &v(1.0, 2.0, 3.0));
    t.c(Op::AddV, v(1.0, 2.0, 3.0), v(10.0, 20.0, 30.0), &v(11.0, 22.0, 33.0));
    t.c(Op::SubV, v(1.0, 2.0, 3.0), v(10.0, 20.0, 30.0), &v(-9.0, -18.0, -27.0));
}

fn comparisons(t: &mut Tester) {
    let (one, zero) = (fb(true), fb(false));
    t.c(Op::EqF, f(-0.0), f(0.0), &[one]);
    t.c(Op::EqF, f(f32::NAN), f(f32::NAN), &[zero]);
    t.c(Op::NeF, f(f32::NAN), f(1.0), &[one]);
    t.c(Op::LeF, f(1.0), f(1.0), &[one]);
    t.c(Op::GeF, f(0.5), f(1.0), &[zero]);
    t.c(Op::LtF, f(0.5), f(1.0), &[one]);
    t.c(Op::GtF, f(0.5), f(1.0), &[zero]);
    t.c(Op::EqV, v(1.0, 2.0, 3.0), v(1.0, 2.0, 3.0), &[one]);
    t.c(Op::EqV, v(1.0, 2.0, 3.0), v(1.0, 2.0, 4.0), &[zero]);
    t.c(Op::NeV, v(1.0, 2.0, 3.0), v(1.0, 2.0, 4.0), &[one]);
    t.c(Op::EqE, u(5), u(5), &[one]);
    t.c(Op::NeE, u(5), u(6), &[one]);
    t.c(Op::EqFnc, u(0x0100_0003), u(3), &[zero]);
    t.c(Op::NeFnc, u(3), u(3), &[zero]);
}

fn strings(t: &mut Tester) {
    let (one, zero) = (fb(true), fb(false));
    // Offsets in the assembler's string table: 0 = null, 1 = "", then interned strings.
    let mut probe = Asm::new();
    let abc = probe.string("abc");
    let abd = probe.string("abd");
    let xabc = probe.string("xabc");
    let intern = |asm: &mut Asm| {
        asm.string("abc");
        asm.string("abd");
        asm.string("xabc");
    };
    let mut s = |op, a: u32, b: u32| t.run_with(op, u(a), u(b), [MARK; 3], intern).c[0];
    assert_eq!(s(Op::EqS, 0, 1), one, "null == \"\"");
    assert_eq!(s(Op::EqS, abc, abc), one);
    assert_eq!(s(Op::EqS, abc, abd), zero);
    assert_eq!(s(Op::EqS, abc, xabc + 1), one, "same content, different reference");
    assert_eq!(s(Op::NeS, abc, abd), one);
    assert_eq!(s(Op::NeS, 0, 0), zero);
    assert_eq!(s(Op::NotS, 0, 0), one);
    assert_eq!(s(Op::NotS, 1, 0), one, "\"\" is 'not'");
    assert_eq!(s(Op::NotS, abc, 0), zero);
    // Pointer arithmetic on string references.
    assert_eq!(s(Op::AddSF, abc, 2.0f32.to_bits()), abc + 2);
    assert_eq!(s(Op::SubS, abc + 2, abc), 2);
    // LOADP_C reads characters through a string reference.
    assert_eq!(s(Op::LoadPC, abc, 1.0f32.to_bits()), f32::from(b'b').to_bits());
}

fn logic(t: &mut Tester) {
    let (one, zero) = (fb(true), fb(false));
    t.c(Op::NotF, f(-0.0), f(0.0), &[one]);
    t.c(Op::NotF, u(1), f(0.0), &[zero]);
    t.c(Op::NotV, v(0.0, -0.0, 0.0), f(0.0), &[one]);
    t.c(Op::NotV, v(0.0, 0.0, 1.0), f(0.0), &[zero]);
    t.c(Op::NotEnt, u(0), u(0), &[one]);
    t.c(Op::NotFnc, u(0x0100_0000), u(0), &[one]);
    t.c(Op::NotI, u(0), u(0), &[1]);
    t.c(Op::NotI, u(7), u(0), &[0]);
    t.c(Op::AndF, f(-0.0), f(1.0), &[zero]);
    t.c(Op::AndF, f(2.0), f(1.0), &[one]);
    t.c(Op::OrF, f(-0.0), f(0.0), &[zero]);
    t.c(Op::OrF, f(-0.0), u(1), &[one]);
    t.c(Op::BitAndF, f(6.9), f(3.1), &[2.0f32.to_bits()]);
    t.c(Op::BitOrF, f(4.0), f(1.0), &[5.0f32.to_bits()]);
}

/// `op cond -> skip; C = 1; skip: RETURN C`. Returns whether the branch was taken.
fn branch(t: &mut Tester, op: Op, cond: W) -> bool {
    t.covered.insert(op as u16);
    let mut asm = Asm::new();
    let ga = asm.global("A", ty::VECTOR, &cond);
    let one = asm.float(1.0);
    asm.global("B", ty::VECTOR, &[]);
    let gc = asm.global("C", ty::VECTOR, &[]);
    asm.function("t", &[], 0);
    let br = asm.emit(op, ga, 0, 0);
    asm.emit(Op::StoreF, one, gc, 0);
    let end = asm.emit(Op::Return, gc, 0, 0);
    asm.patch_jump(br, 1, end);
    t.exec(&asm).c[0] == 0
}

fn branches(t: &mut Tester) {
    assert!(branch(t, Op::IfI, f(-0.0)), "IF_I is integer truth: -0.0 is true");
    assert!(!branch(t, Op::IfI, u(0)));
    assert!(branch(t, Op::IfNotI, u(0)));
    assert!(!branch(t, Op::IfF, f(-0.0)), "IF_F masks the sign");
    assert!(branch(t, Op::IfF, u(1)), "denormals are true");
    assert!(branch(t, Op::IfNotF, f(-0.0)));
    assert!(branch(t, Op::IfS, u(1)), "IF_S tests for null only");
    assert!(!branch(t, Op::IfS, u(0)));
    assert!(branch(t, Op::IfNotS, u(0)));
    assert!(!branch(t, Op::IfNotS, u(1)));

    t.covered.insert(Op::Goto as u16);
    let mut asm = Asm::new();
    asm.global("A", ty::VECTOR, &[]);
    asm.global("B", ty::VECTOR, &[]);
    let gc = asm.global("C", ty::VECTOR, &[]);
    let one = asm.float(1.0);
    asm.function("t", &[], 0);
    let g = asm.emit(Op::Goto, 0, 0, 0);
    asm.emit(Op::StoreF, one, gc, 0);
    let end = asm.emit(Op::Done, 0, 0, 0);
    asm.patch_jump(g, 0, end);
    assert_eq!(t.exec(&asm).c[0], 0);
}

fn stores(t: &mut Tester) {
    for op in
        [Op::StoreF, Op::StoreS, Op::StoreEnt, Op::StoreFld, Op::StoreFnc, Op::StoreI, Op::StoreP]
    {
        t.b(op, u(42), &[42, MARK, MARK]);
    }
    t.b(Op::StoreV, v(1.0, 2.0, 3.0), &v(1.0, 2.0, 3.0));
    t.b(Op::StoreI64, [1, 2, 3], &[1, 2, MARK]);
    t.b(Op::StoreIF, i(-3), &[(-3.0f32).to_bits()]);
    t.b(Op::StoreFI, f(-3.9), &[(-3i32) as u32]);
    t.b(Op::StoreFI, f(f32::NAN), &[i32::MIN as u32]);
}

/// Runs code working on entity 1 (spawned before the call) with fields `fa` (float), `fv`
/// (vector) and `fi` (int64-sized, two words).
fn entity_case(
    t: &mut Tester,
    op: Op,
    a: W,
    b: W,
    c: W,
    body: impl FnOnce(&mut Asm, [u32; 3], [u32; 3]),
) -> (Out, Vec<u32>) {
    t.covered.insert(op as u16);
    let mut asm = Asm::new();
    let ga = asm.global("A", ty::VECTOR, &a);
    let gb = asm.global("B", ty::VECTOR, &b);
    let gc = asm.global("C", ty::VECTOR, &c);
    let (_, fa) = asm.field("fa", ty::FLOAT);
    let (_, fvg) = asm.field("fv", ty::VECTOR);
    let (_, fi) = asm.field("fi", ty::FLOAT);
    asm.field("fi2", ty::FLOAT);
    asm.function("t", &[], 0);
    body(&mut asm, [ga, gb, gc], [fa, fvg, fi]);
    asm.emit(Op::Done, 0, 0, 0);
    let mut vm = t.vm(&asm);
    let e = vm.spawn().unwrap();
    assert_eq!(e, EntRef(1));
    let fields = ["fa", "fv_x", "fv_y", "fv_z", "fi", "fi2"];
    for (k, name) in fields.iter().enumerate() {
        let fld = vm.field::<f32>(*name).unwrap();
        vm.set_field(e, fld, f32::from_bits(100 + k as u32));
    }
    let mut host = TestHost::default();
    let tf = vm.find_function("t").unwrap();
    let r = vm.call(&mut host, tf, &[]);
    let get = |n: &str| vm.global::<[f32; 3]>(n).map(|g| vm.get(g).map(f32::to_bits)).unwrap();
    let out = Out {
        a: get("A"),
        b: get("B"),
        c: get("C"),
        ret: [0; 3],
        err: r.err().map(|e| e.kind().clone()),
    };
    let values = fields
        .iter()
        .map(|n| vm.get_field(e, vm.field::<f32>(*n).unwrap()).unwrap().to_bits())
        .collect();
    (out, values)
}

fn entities(t: &mut Tester) {
    // LOAD_*: A = entity, B = field global (holding the offset), C = destination.
    for op in [Op::LoadF, Op::LoadS, Op::LoadEnt, Op::LoadFld, Op::LoadFnc, Op::LoadI, Op::LoadP] {
        let (out, _) =
            entity_case(t, op, u(1), [0; 3], [MARK; 3], |asm, [ga, gb, gc], [fa, _, _]| {
                asm.emit(Op::StoreF, fa, gb, 0);
                asm.emit(op, ga, gb, gc);
            });
        assert_eq!(out.c[0], 100, "{op:?}");
    }
    let (out, _) =
        entity_case(t, Op::LoadV, u(1), [0; 3], [MARK; 3], |asm, [ga, gb, gc], [_, fv, _]| {
            asm.emit(Op::StoreF, fv, gb, 0);
            asm.emit(Op::LoadV, ga, gb, gc);
        });
    assert_eq!(out.c, [101, 102, 103]);
    let (out, _) =
        entity_case(t, Op::LoadI64, u(1), [0; 3], [MARK; 3], |asm, [ga, gb, gc], [_, _, fi]| {
            asm.emit(Op::StoreF, fi, gb, 0);
            asm.emit(Op::LoadI64, ga, gb, gc);
        });
    assert_eq!(out.c, [104, 105, MARK]);
    // Invalid entity: LOAD_V zeroes all three words.
    let (out, _) =
        entity_case(t, Op::LoadV, u(77), [0; 3], [MARK; 3], |asm, [ga, gb, gc], [_, fv, _]| {
            asm.emit(Op::StoreF, fv, gb, 0);
            asm.emit(Op::LoadV, ga, gb, gc);
        });
    assert_eq!(out.c, [0, 0, 0]);

    // STOREF_*: A = entity, B = field, C = value.
    for (op, field, want) in [
        (Op::StoreFieldF, 0, vec![7, 101, 102, 103, 104, 105]),
        (Op::StoreFieldS, 0, vec![7, 101, 102, 103, 104, 105]),
        (Op::StoreFieldI, 0, vec![7, 101, 102, 103, 104, 105]),
        (Op::StoreFieldV, 1, vec![100, 7, 8, 9, 104, 105]),
        (Op::StoreFieldI64, 2, vec![100, 101, 102, 103, 7, 8]),
    ] {
        let (out, values) =
            entity_case(t, op, u(1), [0; 3], [7, 8, 9], |asm, [ga, gb, gc], fields| {
                asm.emit(Op::StoreF, fields[field], gb, 0);
                asm.emit(op, ga, gb, gc);
            });
        assert_eq!(out.err, None);
        assert_eq!(values, want, "{op:?}");
    }

    // ADDRESS + STOREP_*: pointer to a field, then store through it.
    for op in [Op::StorePF, Op::StorePS, Op::StorePEnt, Op::StorePFld, Op::StorePFnc, Op::StorePI] {
        let (_, values) =
            entity_case(t, op, u(55), u(1), [0; 3], |asm, [ga, gb, gc], [fa, _, _]| {
                let p = asm.temp(1);
                asm.emit(Op::Address, gb, fa, p);
                asm.emit(op, ga, p, 0);
                let _ = gc;
            });
        assert_eq!(values[0], 55, "{op:?}");
    }
    t.covered.insert(Op::Address as u16);
    let (_, values) =
        entity_case(t, Op::StorePV, [1, 2, 3], u(1), [0; 3], |asm, [ga, gb, _], [_, fv, _]| {
            let p = asm.temp(1);
            asm.emit(Op::Address, gb, fv, p);
            asm.emit(Op::StorePV, ga, p, 0);
        });
    assert_eq!(values[1..4], [1, 2, 3]);
}

fn pointers(t: &mut Tester) {
    // GLOBALADDRESS: &global[a + B] as a byte address; LOADP/STOREP through it.
    let mut asm = Asm::new();
    t.covered.extend([
        Op::GlobalAddress as u16,
        Op::LoadPF as u16,
        Op::StorePI as u16,
        Op::AddPIW as u16,
    ]);
    let arr = asm.alloc(4, &[10, 20, 30, 40]);
    let ga = asm.global("A", ty::VECTOR, &[]);
    let gb = asm.global("B", ty::VECTOR, &[]);
    let gc = asm.global("C", ty::VECTOR, &[]);
    let two = asm.int(2);
    let one = asm.int(1);
    let p = asm.temp(1);
    asm.function("t", &[], 0);
    asm.emit(Op::GlobalAddress, arr, two, p); // p = &arr[2]
    asm.emit(Op::LoadPF, p, 0, ga); // A = arr[2]
    asm.emit(Op::LoadPF, p, one, gb); // B = arr[3]
    asm.emit(Op::StorePI, two, p, one); // arr[3] = 2 (value global `two` holds 2)
    asm.emit(Op::AddPIW, p, one, p); // p = &arr[3]
    asm.emit(Op::LoadPF, p, 0, gc);
    asm.emit(Op::Done, 0, 0, 0);
    let out = t.exec(&asm);
    assert_eq!(out.err, None);
    assert_eq!((out.a[0], out.b[0], out.c[0]), (30, 40, 2));

    // Sized loads/stores and conversions through pointers, on a global word.
    let sized = |t: &mut Tester, ops: &[(Op, u32, u32)], init: u32| {
        let mut asm = Asm::new();
        let word = asm.alloc(1, &[init]);
        let ga = asm.global("A", ty::VECTOR, &[]);
        let gb = asm.global("B", ty::VECTOR, &[]);
        let gc = asm.global("C", ty::VECTOR, &[]);
        let p = asm.temp(1);
        let zero = asm.int(0);
        asm.function("t", &[], 0);
        asm.emit(Op::GlobalAddress, word, zero, p);
        for &(op, x, idx) in ops {
            t.covered.insert(op as u16);
            let xg = asm.alloc(1, &[x]);
            let ig = asm.alloc(1, &[idx]);
            match op {
                Op::StorePC | Op::StorePI8 | Op::StorePI16 | Op::StorePIF | Op::StorePFI => {
                    asm.emit(op, xg, p, ig);
                }
                _ => {
                    asm.emit(op, p, ig, gc);
                }
            }
        }
        asm.emit(Op::LoadPI, p, zero, gb);
        let _ = ga;
        asm.emit(Op::Done, 0, 0, 0);
        t.exec(&asm)
    };
    let out = sized(t, &[(Op::LoadPU8, 0, 1)], 0x8877_6655);
    assert_eq!(out.c[0], 0x66);
    let out = sized(t, &[(Op::LoadPI8, 0, 3)], 0x8877_6655);
    assert_eq!(out.c[0], 0xFFFF_FF88);
    let out = sized(t, &[(Op::LoadPU16, 0, 1)], 0x8877_6655);
    assert_eq!(out.c[0], 0x8877);
    let out = sized(t, &[(Op::LoadPI16, 0, 1)], 0x8877_6655);
    assert_eq!(out.c[0], 0xFFFF_8877);
    let out = sized(t, &[(Op::StorePI8, 0xAB, 2)], 0x1111_1111);
    assert_eq!(out.b[0], 0x11AB_1111);
    let out = sized(t, &[(Op::StorePI16, 0xBEEF, 1)], 0x1111_1111);
    assert_eq!(out.b[0], 0xBEEF_1111);
    let out = sized(t, &[(Op::StorePC, 65.0f32.to_bits(), 0)], 0x1111_1111);
    assert_eq!(out.b[0], 0x1111_1141);
    let out = sized(t, &[(Op::StorePIF, 3, 0)], 0);
    assert_eq!(out.b[0], 3.0f32.to_bits());
    let out = sized(t, &[(Op::StorePFI, (-2.5f32).to_bits(), 0)], 0);
    assert_eq!(out.b[0], (-2i32) as u32);
    let out = sized(t, &[(Op::LoadPItoF, 0, 99)], 7);
    assert_eq!(out.c[0], 7.0f32.to_bits());
    let out = sized(t, &[(Op::LoadPFtoI, 0, 99)], 7.9f32.to_bits());
    assert_eq!(out.c[0], 7);

    // Vector, 64-bit and remaining word forms through pointers.
    let mut asm = Asm::new();
    let src = asm.alloc(3, &[1, 2, 3]);
    let dst = asm.alloc(3, &[0, 0, 0]);
    asm.global("A", ty::VECTOR, &[]);
    let gb = asm.global("B", ty::VECTOR, &[]);
    let gc = asm.global("C", ty::VECTOR, &[]);
    let (p, q, zero) = (asm.temp(1), asm.temp(1), asm.int(0));
    asm.function("t", &[], 0);
    asm.emit(Op::GlobalAddress, src, zero, p);
    asm.emit(Op::GlobalAddress, dst, zero, q);
    asm.emit(Op::LoadPV, p, zero, gc);
    asm.emit(Op::StorePV, gc, q, zero);
    asm.emit(Op::LoadPV, q, zero, gb);
    asm.emit(Op::Done, 0, 0, 0);
    t.covered.extend([Op::LoadPV as u16, Op::StorePV as u16]);
    let out = t.exec(&asm);
    assert_eq!((out.b, out.c), ([1, 2, 3], [1, 2, 3]));

    for (load, store) in [
        (Op::LoadPS, Op::StorePS),
        (Op::LoadPEnt, Op::StorePEnt),
        (Op::LoadPFld, Op::StorePFld),
        (Op::LoadPFnc, Op::StorePFnc),
        (Op::LoadPI, Op::StorePF),
        (Op::LoadPI64, Op::StorePI64),
    ] {
        t.covered.extend([load as u16, store as u16]);
        let mut asm = Asm::new();
        let cell = asm.alloc(2, &[0, 0]);
        asm.global("A", ty::VECTOR, &[]);
        let gb = asm.global("B", ty::VECTOR, &[]);
        let gc = asm.global("C", ty::VECTOR, &[77, 88, 0]);
        let (p, zero) = (asm.temp(1), asm.int(0));
        asm.function("t", &[], 0);
        asm.emit(Op::GlobalAddress, cell, zero, p);
        asm.emit(store, gc, p, zero);
        asm.emit(load, p, zero, gb);
        asm.emit(Op::Done, 0, 0, 0);
        let out = t.exec(&asm);
        let words = if load == Op::LoadPI64 { 2 } else { 1 };
        assert_eq!(out.b[..words], [77, 88][..words], "{load:?}/{store:?}");
    }

    // Faults.
    t.fault(Op::LoadPF, u(0x7FFF_0000), u(0), u(0), ErrorKind::BadPointerRead(0x7FFF_0000));
    t.fault(Op::StorePF, u(1), u(0), u(0), ErrorKind::NullPointerWrite);
    t.fault(Op::StorePF, u(1), u(0x7FFF_0000), u(0), ErrorKind::BadPointerWrite(0x7FFF_0000));
    // The 0xFFFFFFFF sentinel reads zero and swallows writes.
    let out = t.run(Op::LoadPF, u(u32::MAX), u(0), [MARK; 3]);
    assert_eq!((out.err, out.c[0]), (None, 0));
    let out = t.run(Op::StorePF, u(5), u(u32::MAX), u(0));
    assert_eq!(out.err, None);
}

fn compound(t: &mut Tester) {
    // Non-pointer forms update B and leave C alone.
    let run = |t: &mut Tester, op, a, b| t.run(op, a, b, [MARK; 3]);
    let out = run(t, Op::MulStoreF, f(3.0), f(4.0));
    assert_eq!((out.b[0], out.c[0]), (12.0f32.to_bits(), MARK));
    let out = run(t, Op::DivStoreF, f(4.0), f(2.0));
    assert_eq!(out.b[0], 0.5f32.to_bits());
    let out = run(t, Op::AddStoreF, f(4.0), f(2.0));
    assert_eq!(out.b[0], 6.0f32.to_bits());
    let out = run(t, Op::SubStoreF, f(4.0), f(2.0));
    assert_eq!(out.b[0], (-2.0f32).to_bits());
    let out = run(t, Op::MulStoreVF, f(2.0), v(1.0, 2.0, 3.0));
    assert_eq!(out.b, v(2.0, 4.0, 6.0));
    let out = run(t, Op::AddStoreV, v(1.0, 1.0, 1.0), v(1.0, 2.0, 3.0));
    assert_eq!(out.b, v(2.0, 3.0, 4.0));
    let out = run(t, Op::SubStoreV, v(1.0, 1.0, 1.0), v(1.0, 2.0, 3.0));
    assert_eq!(out.b, v(0.0, 1.0, 2.0));
    let out = run(t, Op::BitSetStoreF, f(1.0), f(4.0));
    assert_eq!(out.b[0], 5.0f32.to_bits());
    let out = run(t, Op::BitClrStoreF, f(1.0), f(5.0));
    assert_eq!(out.b[0], 4.0f32.to_bits());

    // Pointer forms: B is a pointer; C receives the new value (except BITSET/BITCLR).
    let ptr = |t: &mut Tester, op: Op, a: W, cell: W| {
        t.covered.insert(op as u16);
        let mut asm = Asm::new();
        let target = asm.alloc(3, &cell);
        let ga = asm.global("A", ty::VECTOR, &a);
        let gb = asm.global("B", ty::VECTOR, &[]);
        let gc = asm.global("C", ty::VECTOR, &[MARK; 3]);
        let zero = asm.int(0);
        asm.function("t", &[], 0);
        asm.emit(Op::GlobalAddress, target, zero, gb);
        asm.emit(op, ga, gb, gc);
        // Copy the target back into A for inspection.
        asm.emit(Op::LoadPV, gb, zero, ga);
        asm.emit(Op::Done, 0, 0, 0);
        t.exec(&asm)
    };
    let out = ptr(t, Op::MulStorePF, f(3.0), f(4.0));
    assert_eq!((out.a[0], out.c[0]), (12.0f32.to_bits(), 12.0f32.to_bits()));
    let out = ptr(t, Op::DivStorePF, f(2.0), f(4.0));
    assert_eq!(out.c[0], 2.0f32.to_bits());
    let out = ptr(t, Op::AddStorePF, f(2.0), f(4.0));
    assert_eq!(out.c[0], 6.0f32.to_bits());
    let out = ptr(t, Op::SubStorePF, f(2.0), f(4.0));
    assert_eq!(out.c[0], 2.0f32.to_bits());
    let out = ptr(t, Op::MulStorePVF, f(2.0), v(1.0, 2.0, 3.0));
    assert_eq!((out.a, out.c), (v(2.0, 4.0, 6.0), v(2.0, 4.0, 6.0)));
    let out = ptr(t, Op::AddStorePV, v(1.0, 1.0, 1.0), v(1.0, 2.0, 3.0));
    assert_eq!(out.c, v(2.0, 3.0, 4.0));
    let out = ptr(t, Op::SubStorePV, v(1.0, 1.0, 1.0), v(1.0, 2.0, 3.0));
    assert_eq!(out.c, v(0.0, 1.0, 2.0));
    let out = ptr(t, Op::BitSetStorePF, f(1.0), f(4.0));
    assert_eq!((out.a[0], out.c[0]), (5.0f32.to_bits(), MARK));
    let out = ptr(t, Op::BitClrStorePF, f(1.0), f(5.0));
    assert_eq!((out.a[0], out.c[0]), (4.0f32.to_bits(), MARK));
    // Interleaving with C aliasing A (as fteqcc emits it): C[k] is written before A[k+1] is read.
    t.covered.insert(Op::AddStorePV as u16);
    let mut asm = Asm::new();
    let target = asm.alloc(3, &v(1.0, 2.0, 3.0));
    let ga = asm.global("A", ty::VECTOR, &v(10.0, 10.0, 10.0));
    let gb = asm.global("B", ty::VECTOR, &[]);
    asm.global("C", ty::VECTOR, &[]);
    let zero = asm.int(0);
    asm.function("t", &[], 0);
    asm.emit(Op::GlobalAddress, target, zero, gb);
    asm.emit(Op::AddStorePV, ga, gb, ga);
    asm.emit(Op::Done, 0, 0, 0);
    assert_eq!(t.exec(&asm).a, v(11.0, 12.0, 13.0));
    // An invalid pointer is fatal.
    t.fault(Op::AddStorePF, f(1.0), u(0x7FFF_0000), u(0), ErrorKind::BadPointerWrite(0x7FFF_0000));
}

fn hexen2_arrays(t: &mut Tester) {
    for (op, stride) in [
        (Op::FetchGblF, 1),
        (Op::FetchGblS, 1),
        (Op::FetchGblE, 1),
        (Op::FetchGblFnc, 1),
        (Op::FetchGblV, 3),
    ] {
        let mut run = |t: &mut Tester, index: f32| {
            t.covered.insert(op as u16);
            let mut asm = Asm::new();
            // Prefix word holds count - 1 (3 elements).
            let _prefix = asm.alloc(1, &[2]);
            let base = asm.alloc(3 * stride, &(1..=9).collect::<Vec<_>>());
            asm.global("A", ty::VECTOR, &[]);
            let gb = asm.global("B", ty::VECTOR, &f(index));
            let gc = asm.global("C", ty::VECTOR, &[MARK; 3]);
            asm.function("t", &[], 0);
            asm.emit(op, base, gb, gc);
            asm.emit(Op::Done, 0, 0, 0);
            t.exec(&asm)
        };
        let out = run(t, 2.0);
        assert_eq!(out.err, None, "{op:?}");
        if stride == 3 {
            assert_eq!(out.c, [7, 8, 9]);
        } else {
            assert_eq!(out.c[0], 3);
        }
        assert_eq!(run(t, 3.0).err, Some(ErrorKind::ArrayIndex(3)));
        assert_eq!(run(t, -1.0).err, Some(ErrorKind::ArrayIndex(-1)));
    }
}

fn animation(t: &mut Tester) {
    let setup = |asm: &mut Asm| {
        asm.global("self", ty::ENTITY, &[1]);
        asm.global("time", ty::FLOAT, &f(10.0));
        asm.global("cycle_wrapped", ty::FLOAT, &[]);
        asm.field("frame", ty::FLOAT);
        asm.field("weaponframe", ty::FLOAT);
        asm.field("think", ty::FUNCTION);
        asm.field("nextthink", ty::FLOAT);
    };
    let run = |t: &mut Tester, op: Op, a: W, b: W, start_frame: f32| {
        t.covered.insert(op as u16);
        let mut asm = Asm::new();
        let ga = asm.global("A", ty::VECTOR, &a);
        let gb = asm.global("B", ty::VECTOR, &b);
        asm.global("C", ty::VECTOR, &[]);
        setup(&mut asm);
        let tf = asm.function("t", &[], 0);
        asm.emit(op, ga, gb, 0);
        asm.emit(Op::Done, 0, 0, 0);
        let mut vm = t.vm(&asm);
        let e = vm.spawn().unwrap();
        for n in ["frame", "weaponframe"] {
            let fld = vm.field::<f32>(n).unwrap();
            vm.set_field(e, fld, start_frame);
        }
        let mut host = TestHost::default();
        let tfr = vm.find_function("t").unwrap();
        vm.call(&mut host, tfr, &[]).unwrap();
        let get = |n: &str| vm.get_field(e, vm.field::<f32>(n).unwrap()).unwrap();
        let think = vm.get_field(e, vm.field::<qcvm::FuncRef>("think").unwrap()).unwrap();
        let wrapped = vm.get(vm.global::<f32>("cycle_wrapped").unwrap());
        (get("frame"), get("weaponframe"), get("nextthink"), think.0, wrapped, tf.index)
    };
    let (frame, _, next, think, _, _) = run(t, Op::State, f(4.0), u(9), 0.0);
    assert_eq!((frame, next, think), (4.0, 10.1, 9));
    // CSTATE cycles 2..5.
    let (frame, _, _, think, wrapped, tf) = run(t, Op::CState, f(2.0), f(5.0), 3.0);
    assert_eq!((frame, wrapped, think), (4.0, 0.0, tf));
    let (frame, _, _, _, wrapped, _) = run(t, Op::CState, f(2.0), f(5.0), 5.0);
    assert_eq!((frame, wrapped), (2.0, 1.0));
    let (frame, _, _, _, _, _) = run(t, Op::CState, f(2.0), f(5.0), 9.0);
    assert_eq!(frame, 2.0, "outside the range restarts at `first`");
    let (frame, _, _, _, _, _) = run(t, Op::CState, f(5.0), f(2.0), 4.0);
    assert_eq!(frame, 3.0, "first > last runs backwards");
    let (_, wframe, _, _, _, _) = run(t, Op::CWState, f(2.0), f(5.0), 3.0);
    assert_eq!(wframe, 4.0);
    let (_, _, next, _, _, _) = run(t, Op::ThinkTime, u(1), f(2.5), 0.0);
    assert_eq!(next, 12.5);
}

fn random(t: &mut Tester) {
    for _ in 0..50 {
        let r = f32::from_bits(t.run(Op::Rand0, [0; 3], [0; 3], [0; 3]).c[0]);
        assert!((0.0..1.0).contains(&r));
        let r = f32::from_bits(t.run(Op::Rand1, f(10.0), [0; 3], [0; 3]).c[0]);
        assert!((0.0..10.0).contains(&r));
        let r = f32::from_bits(t.run(Op::Rand2, f(5.0), f(7.0), [0; 3]).c[0]);
        assert!((5.0..7.0).contains(&r));
        let out = t.run(Op::RandV0, [0; 3], [0; 3], [0; 3]);
        assert!(out.c.iter().all(|w| (0.0..=1.0).contains(&f32::from_bits(*w))));
        let out = t.run(Op::RandV1, v(2.0, 4.0, 8.0), [0; 3], [0; 3]);
        assert!(
            out.c.iter().zip([2.0, 4.0, 8.0]).all(|(w, m)| (0.0..=m).contains(&f32::from_bits(*w)))
        );
        let out = t.run(Op::RandV2, v(1.0, 1.0, 1.0), v(2.0, 2.0, 2.0), [0; 3]);
        assert!(out.c.iter().all(|w| (1.0..=2.0).contains(&f32::from_bits(*w))));
    }
}

/// switch (A) { case X: C = 1; case lo..hi: C = 2; default: C = 3 }, laid out like fteqcc:
/// SWITCH jumps to the test chain after the bodies.
fn switch_case(t: &mut Tester, sw: Op, value: W, case: W, lo: W, hi: W) -> u32 {
    t.covered.extend([sw as u16, Op::Case as u16, Op::CaseRange as u16]);
    let mut asm = Asm::new();
    // Interned first, so string cases can use the same offsets as a fresh assembler.
    asm.string("abc");
    asm.string("xabc");
    let ga = asm.global("A", ty::VECTOR, &value);
    asm.global("B", ty::VECTOR, &[]);
    let gc = asm.global("C", ty::VECTOR, &[]);
    let (one, two, three) = (asm.float(1.0), asm.float(2.0), asm.float(3.0));
    let (gcase, glo, ghi) = (asm.vector_raw(case), asm.vector_raw(lo), asm.vector_raw(hi));
    asm.function("t", &[], 0);
    let s = asm.emit(sw, ga, 0, 0);
    let body1 = asm.emit(Op::StoreF, one, gc, 0);
    let j1 = asm.emit(Op::Goto, 0, 0, 0);
    let body2 = asm.emit(Op::StoreF, two, gc, 0);
    let j2 = asm.emit(Op::Goto, 0, 0, 0);
    let body3 = asm.emit(Op::StoreF, three, gc, 0);
    let j3 = asm.emit(Op::Goto, 0, 0, 0);
    let chain = asm.here();
    let c1 = asm.emit(Op::Case, gcase, 0, 0);
    let c2 = if sw == Op::SwitchS {
        asm.emit(Op::Goto, 0, 0, 0)
    } else {
        asm.emit(Op::CaseRange, glo, ghi, 0)
    };
    let dflt = asm.emit(Op::Goto, 0, 0, 0);
    let end = asm.emit(Op::Done, 0, 0, 0);
    asm.patch_jump(s, 1, chain);
    asm.patch_jump(c1, 1, body1);
    if sw == Op::SwitchS {
        asm.patch_jump(c2, 0, dflt);
    } else {
        asm.patch_jump(c2, 2, body2);
    }
    asm.patch_jump(dflt, 0, body3);
    for j in [j1, j2, j3] {
        asm.patch_jump(j, 0, end);
    }
    let out = t.exec(&asm);
    assert_eq!(out.err, None, "{sw:?}");
    f32::from_bits(out.c[0]) as u32
}

fn switches(t: &mut Tester) {
    let (lo, hi) = (f(10.0), f(20.0));
    assert_eq!(switch_case(t, Op::SwitchF, f(-0.0), f(0.0), lo, hi), 1, "-0 == 0");
    assert_eq!(switch_case(t, Op::SwitchF, f(15.0), f(0.0), lo, hi), 2);
    assert_eq!(switch_case(t, Op::SwitchF, f(20.0), f(0.0), lo, hi), 2, "inclusive");
    assert_eq!(switch_case(t, Op::SwitchF, f(21.0), f(0.0), lo, hi), 3);
    let (vlo, vhi) = (v(0.0, 0.0, 0.0), v(1.0, 1.0, 1.0));
    assert_eq!(switch_case(t, Op::SwitchV, v(1.0, 2.0, 3.0), v(1.0, 2.0, 3.0), vlo, vhi), 1);
    assert_eq!(switch_case(t, Op::SwitchV, v(0.5, 0.5, 1.0), v(1.0, 2.0, 3.0), vlo, vhi), 2);
    assert_eq!(switch_case(t, Op::SwitchV, v(0.5, 2.0, 1.0), v(1.0, 2.0, 3.0), vlo, vhi), 3);
    for sw in [Op::SwitchE, Op::SwitchFnc, Op::SwitchI] {
        assert_eq!(switch_case(t, sw, u(5), u(5), i(-3), i(3)), 1);
        assert_eq!(switch_case(t, sw, i(-2), u(5), i(-3), i(3)), 2, "signed range");
        assert_eq!(switch_case(t, sw, u(9), u(5), i(-3), i(3)), 3);
    }
    // String switch compares contents: "abc" (offset of "xabc" + 1) matches the literal "abc".
    let mut probe = Asm::new();
    let abc = probe.string("abc");
    let xabc = probe.string("xabc");
    assert_eq!(switch_case(t, Op::SwitchS, u(xabc + 1), u(abc), [0; 3], [0; 3]), 1);
    assert_eq!(switch_case(t, Op::SwitchS, u(0), u(1), [0; 3], [0; 3]), 1, "null matches \"\"");
    assert_eq!(switch_case(t, Op::SwitchS, u(abc), u(1), [0; 3], [0; 3]), 3);
}

fn calls(t: &mut Tester) {
    // Every CALLn/CALLnH passes its arguments and argc; the callee returns the sum of its
    // parameters (all vectors, to see all three words move).
    for n in 0..=8u32 {
        for hexen2 in [false, true] {
            if hexen2 && n == 0 {
                continue;
            }
            let op = if hexen2 {
                Op::from_u32(Op::Call1H as u32 + n - 1)
            } else {
                Op::from_u32(Op::Call0 as u32 + n)
            };
            t.covered.insert(op as u16);
            let mut asm = Asm::new();
            asm.global("A", ty::VECTOR, &[]);
            asm.global("B", ty::VECTOR, &[]);
            asm.global("C", ty::VECTOR, &[]);
            let sizes = vec![3u8; n as usize];
            let callee = asm.function("sum", &sizes, 3);
            let acc = callee.local(3 * n);
            for k in 0..n {
                asm.emit(Op::AddV, acc, callee.local(3 * k), acc);
            }
            asm.emit(Op::Return, acc, 0, 0);
            let callee_g = asm.global("sum_g", ty::FUNCTION, &[callee.index]);
            let args: Vec<u32> =
                (0..n).map(|k| asm.vector([k as f32 + 1.0, 10.0, 100.0])).collect();
            asm.function("t", &[], 0);
            let first_inline = if hexen2 { 2.min(n) } else { 0 };
            for k in first_inline..n {
                asm.emit(Op::StoreV, args[k as usize], parm(k), 0);
            }
            let (b, c) = match (hexen2, n) {
                (true, 1) => (args[0], 0),
                (true, _) => (args[0], args[1]),
                _ => (0, 0),
            };
            asm.emit(op, callee_g, b, c);
            asm.emit(Op::Return, OFS_RETURN, 0, 0);
            let out = t.exec(&asm);
            assert_eq!(out.err, None, "{op:?}");
            let sum_x: f32 = (1..=n).fold(0.0, |s, k| s + k as f32);
            let want = [sum_x, 10.0 * n as f32, 100.0 * n as f32].map(f32::to_bits);
            assert_eq!(out.ret, want, "{op:?}");
        }
    }
    t.covered.extend([Op::Return as u16, Op::Done as u16]);
}

fn integers(t: &mut Tester) {
    t.c(Op::AddI, i(i32::MAX), i(1), &[i32::MIN as u32]);
    t.c(Op::SubI, i(1), i(3), &[(-2i32) as u32]);
    t.c(Op::MulI, i(-4), i(5), &[(-20i32) as u32]);
    t.c(Op::DivI, i(-7), i(2), &[(-3i32) as u32]);
    t.c(Op::DivI, i(7), i(0), &[0]);
    t.c(Op::DivI, i(i32::MIN), i(-1), &[i32::MAX as u32]);
    t.c(Op::BitAndI, u(0b1100), u(0b1010), &[0b1000]);
    t.c(Op::BitOrI, u(0b1100), u(0b1010), &[0b1110]);
    t.c(Op::BitXorI, u(0b1100), u(0b1010), &[0b0110]);
    t.c(Op::RShiftI, i(-16), i(2), &[(-4i32) as u32]);
    t.c(Op::LShiftI, i(3), i(4), &[48]);
    t.c(Op::LShiftI, i(1), i(33), &[2]);
    t.c(Op::EqI, i(3), i(3), &[1]);
    t.c(Op::NeI, i(3), i(3), &[0]);
    t.c(Op::LeI, i(-1), i(0), &[1]);
    t.c(Op::GeI, i(-1), i(0), &[0]);
    t.c(Op::LtI, i(-1), i(0), &[1]);
    t.c(Op::GtI, i(-1), i(0), &[0]);
    t.c(Op::AndI, i(2), i(0), &[0]);
    t.c(Op::OrI, i(2), i(0), &[1]);
    t.c(Op::ConvItoF, i(-3), [0; 3], &[(-3.0f32).to_bits()]);
    t.c(Op::ConvFtoI, f(-3.7), [0; 3], &[(-3i32) as u32]);
    t.c(Op::ConvFtoI, f(3e9), [0; 3], &[i32::MIN as u32]);
}

fn mixed(t: &mut Tester) {
    let fl = |x: f32| x.to_bits();
    t.c(Op::AddFI, f(1.5), i(2), &[fl(3.5)]);
    t.c(Op::AddIF, i(2), f(1.5), &[fl(3.5)]);
    t.c(Op::SubFI, f(1.5), i(2), &[fl(-0.5)]);
    t.c(Op::SubIF, i(2), f(1.5), &[fl(0.5)]);
    t.c(Op::MulIF, i(2), f(1.5), &[fl(3.0)]);
    t.c(Op::MulFI, f(1.5), i(2), &[fl(3.0)]);
    t.c(Op::DivIF, i(3), f(2.0), &[fl(1.5)]);
    t.c(Op::DivFI, f(3.0), i(2), &[fl(1.5)]);
    t.c(Op::MulVI, v(1.0, 2.0, 3.0), i(2), &v(2.0, 4.0, 6.0));
    t.c(Op::MulIV, i(2), v(1.0, 2.0, 3.0), &v(2.0, 4.0, 6.0));
    for (op, a, b, want) in [
        (Op::LeIF, i(1), f(1.0), 1),
        (Op::GeIF, i(1), f(1.5), 0),
        (Op::LtIF, i(1), f(1.5), 1),
        (Op::GtIF, i(2), f(1.5), 1),
        (Op::EqIF, i(2), f(2.0), 1),
        (Op::NeIF, i(2), f(2.0), 0),
        (Op::LeFI, f(1.0), i(1), 1),
        (Op::GeFI, f(0.5), i(1), 0),
        (Op::LtFI, f(0.5), i(1), 1),
        (Op::GtFI, f(1.5), i(1), 1),
        (Op::EqFI, f(2.0), i(2), 1),
        (Op::NeFI, f(2.0), i(2), 0),
        (Op::AndIF, i(1), f(-0.0), 0),
        (Op::OrIF, i(0), f(f32::NAN), 1),
        (Op::AndFI, f(0.5), i(3), 1),
        (Op::OrFI, f(0.0), i(0), 0),
    ] {
        t.c(op, a, b, &[want]);
    }
    t.c(Op::BitAndIF, i(7), f(2.9), &[2]);
    t.c(Op::BitOrIF, i(4), f(1.9), &[5]);
    t.c(Op::BitAndFI, f(7.9), i(2), &[2]);
    t.c(Op::BitOrFI, f(4.2), i(1), &[5]);
}

fn globals_indexed(t: &mut Tester) {
    let arr_case = |t: &mut Tester, op: Op, index: i32| {
        t.covered.insert(op as u16);
        let mut asm = Asm::new();
        let arr = asm.alloc(6, &[11, 22, 33, 44, 55, 66]);
        asm.global("A", ty::VECTOR, &[]);
        let gb = asm.global("B", ty::VECTOR, &i(index));
        let gc = asm.global("C", ty::VECTOR, &[MARK; 3]);
        asm.function("t", &[], 0);
        asm.emit(op, arr, gb, gc);
        asm.emit(Op::Done, 0, 0, 0);
        (t.exec(&asm), arr)
    };
    for op in [Op::LoadAF, Op::LoadAS, Op::LoadAEnt, Op::LoadAFld, Op::LoadAFnc, Op::LoadAI] {
        assert_eq!(arr_case(t, op, 2).0.c[0], 33, "{op:?}");
    }
    assert_eq!(arr_case(t, Op::LoadAV, 1).0.c, [22, 33, 44]);
    assert_eq!(arr_case(t, Op::LoadAI64, 4).0.c[..2], [55, 66]);
    let (out, arr) = arr_case(t, Op::LoadAF, -100);
    assert_eq!(out.err, Some(ErrorKind::ArrayIndex(i64::from(arr) - 100)));

    // GLOAD/GSTOREP address globals by a runtime index.
    let g_case = |t: &mut Tester, op: Op| {
        t.covered.insert(op as u16);
        let mut asm = Asm::new();
        let target = asm.alloc(3, &[7, 8, 9]);
        let ga = asm.global("A", ty::VECTOR, &[target, 0, 0]);
        let gb = asm.global("B", ty::VECTOR, &[target, 0, 0]);
        let gc = asm.global("C", ty::VECTOR, &[MARK; 3]);
        asm.function("t", &[], 0);
        if matches!(
            op,
            Op::GStorePI
                | Op::GStorePF
                | Op::GStorePEnt
                | Op::GStorePFld
                | Op::GStorePS
                | Op::GStorePFnc
                | Op::GStorePV
        ) {
            let val = asm.vector_raw([5, 6, 4]);
            asm.emit(op, val, gb, 0);
            asm.emit(Op::GLoadV, ga, 0, gc);
        } else {
            asm.emit(op, ga, 0, gc);
        }
        asm.emit(Op::Done, 0, 0, 0);
        t.exec(&asm)
    };
    for op in [Op::GLoadI, Op::GLoadF, Op::GLoadFld, Op::GLoadEnt, Op::GLoadS, Op::GLoadFnc] {
        assert_eq!(g_case(t, op).c[0], 7, "{op:?}");
    }
    assert_eq!(g_case(t, Op::GLoadV).c, [7, 8, 9]);
    for op in
        [Op::GStorePI, Op::GStorePF, Op::GStorePEnt, Op::GStorePFld, Op::GStorePS, Op::GStorePFnc]
    {
        assert_eq!(g_case(t, op).c, [5, 8, 9], "{op:?}");
    }
    assert_eq!(g_case(t, Op::GStorePV).c, [5, 6, 4]);
    t.fault(Op::GLoadF, i(-1), [0; 3], [0; 3], ErrorKind::ArrayIndex(-1));
    t.fault(Op::GStorePF, [0; 3], i(1_000_000), [0; 3], ErrorKind::ArrayIndex(1_000_000));

    // BOUNDCHECK c <= A < b, unsigned.
    let bound = |t: &mut Tester, value: i32| {
        t.covered.insert(Op::BoundCheck as u16);
        let mut asm = Asm::new();
        let ga = asm.global("A", ty::VECTOR, &i(value));
        asm.global("B", ty::VECTOR, &[]);
        asm.global("C", ty::VECTOR, &[]);
        asm.function("t", &[], 0);
        asm.emit(Op::BoundCheck, ga, 10, 2);
        asm.emit(Op::Done, 0, 0, 0);
        t.exec(&asm).err
    };
    assert_eq!(bound(t, 2), None);
    assert_eq!(bound(t, 9), None);
    assert_eq!(bound(t, 10), Some(ErrorKind::BoundCheck { value: 10, low: 2, high: 10 }));
    assert_eq!(bound(t, 1), Some(ErrorKind::BoundCheck { value: 1, low: 2, high: 10 }));
    assert_eq!(bound(t, -1), Some(ErrorKind::BoundCheck { value: -1, low: 2, high: 10 }));
}

fn push_and_faults(t: &mut Tester) {
    // PUSH reserves local-stack words; the pointer is usable until the function returns.
    t.covered.extend([Op::Push as u16]);
    let mut asm = Asm::new();
    asm.global("A", ty::VECTOR, &[]);
    let gb = asm.global("B", ty::VECTOR, &[]);
    let gc = asm.global("C", ty::VECTOR, &[]);
    let (four, zero, v) = (asm.int(4), asm.int(0), asm.int(1234));
    let p = asm.temp(1);
    let q = asm.temp(1);
    asm.function("t", &[], 0);
    asm.emit(Op::Push, four, 0, p);
    asm.emit(Op::Push, four, 0, q);
    asm.emit(Op::StorePI, v, p, zero);
    asm.emit(Op::LoadPI, p, zero, gc);
    asm.emit(Op::SubI, q, p, gb);
    asm.emit(Op::Done, 0, 0, 0);
    let out = t.exec(&asm);
    assert_eq!(out.err, None);
    assert_eq!((out.c[0], out.b[0]), (1234, 16));
    t.fault(Op::Push, i(i32::MAX), [0; 3], [0; 3], ErrorKind::PushedTooMuch);

    t.fault(Op::GAddress, [0; 3], [0; 3], [0; 3], ErrorKind::GAddress);
    t.fault(Op::Unused, [0; 3], [0; 3], [0; 3], ErrorKind::BadOpcode(Op::Unused as u16));
    t.fault(Op::Pop, [0; 3], [0; 3], [0; 3], ErrorKind::BadOpcode(Op::Pop as u16));
}

fn unsigned_and_wide(t: &mut Tester) {
    t.c(Op::LeU, u(1), u(u32::MAX), &[1]);
    t.c(Op::LtU, u(u32::MAX), u(1), &[0]);
    t.c(Op::DivU, u(u32::MAX), u(2), &[u32::MAX / 2]);
    t.c(Op::DivU, u(1), u(0), &[0]);
    t.c(Op::RShiftU, u(0x8000_0000), u(31), &[1]);
    t.c(Op::ConvUF, u(u32::MAX), [0; 3], &[(u32::MAX as f32).to_bits()]);
    t.c(Op::ConvFU, f(-1.0), [0; 3], &[u32::MAX]);

    let w = |x: i64| l(x)[..2].to_vec();
    t.c(Op::AddI64, l(i64::MAX), l(1), &w(i64::MIN));
    t.c(Op::SubI64, l(1), l(3), &w(-2));
    t.c(Op::MulI64, l(1 << 40), l(3), &w(3 << 40));
    t.c(Op::DivI64, l(-9), l(2), &w(-4));
    t.c(Op::DivI64, l(9), l(0), &w(0));
    t.c(Op::DivI64, l(i64::MIN), l(-1), &w(i64::MIN));
    t.c(Op::BitAndI64, l(0xF0F0), l(0xFF00), &w(0xF000));
    t.c(Op::BitOrI64, l(0xF0F0), l(0xFF00), &w(0xFFF0));
    t.c(Op::BitXorI64, l(0xF0F0), l(0xFF00), &w(0x0FF0));
    t.c(Op::LShiftI64I, l(1), i(40), &w(1 << 40));
    t.c(Op::RShiftI64I, l(-(1 << 40)), i(8), &w(-(1 << 32)));
    t.c(Op::RShiftU64I, l(-1), i(60), &w(15));
    t.c(Op::LeI64, l(-1), l(0), &[1]);
    t.c(Op::LtI64, l(0), l(-1), &[0]);
    t.c(Op::EqI64, l(1 << 33), l(1 << 33), &[1]);
    t.c(Op::NeI64, l(1 << 33), l(1 << 32), &[1]);
    t.c(Op::LeU64, l(-1), l(0), &[0]);
    t.c(Op::LtU64, l(0), l(-1), &[1]);
    t.c(Op::DivU64, l(-1), l(2), &w(i64::MAX));
    t.c(Op::DivU64, l(5), l(0), &w(0));
    t.c(Op::ConvUI64, u(u32::MAX), [0; 3], &w(i64::from(u32::MAX)));
    t.c(Op::ConvII64, i(-5), [0; 3], &w(-5));
    t.c(Op::ConvI64I, l(0x1_0000_0007), [0; 3], &[7]);
    t.c(Op::ConvI64F, l(-3), [0; 3], &[(-3.0f32).to_bits()]);
    t.c(Op::ConvU64F, l(-1), [0; 3], &[(u64::MAX as f32).to_bits()]);
    t.c(Op::ConvFI64, f(-2.5), [0; 3], &w(-2));
    t.c(Op::ConvFU64, f(-1.0), [0; 3], &w(-1));

    let dw = |x: f64| d(x)[..2].to_vec();
    t.c(Op::AddD, d(0.1), d(0.2), &dw(0.1 + 0.2));
    t.c(Op::SubD, d(1.0), d(0.25), &dw(0.75));
    t.c(Op::MulD, d(1.5), d(4.0), &dw(6.0));
    t.c(Op::DivD, d(1.0), d(3.0), &dw(1.0 / 3.0));
    t.c(Op::LeD, d(1.0), d(1.0), &[1]);
    t.c(Op::LtD, d(1.0), d(1.0), &[0]);
    t.c(Op::EqD, d(0.5), d(0.5), &[1]);
    t.c(Op::NeD, d(0.5), d(0.5), &[0]);
    t.c(Op::ConvFD, f(0.1), [0; 3], &dw(f64::from(0.1f32)));
    t.c(Op::ConvDF, d(0.1), [0; 3], &[0.1f32.to_bits()]);
    t.c(Op::ConvI64D, l(-7), [0; 3], &dw(-7.0));
    t.c(Op::ConvU64D, l(-1), [0; 3], &dw(u64::MAX as f64));
    t.c(Op::ConvDI64, d(-7.9), [0; 3], &w(-7));
    t.c(Op::ConvDU64, d(1e19), [0; 3], &w(10_000_000_000_000_000_000u64 as i64));
}

fn bitfields(t: &mut Tester) {
    // Field of width 4 at bit 8.
    let desc = u(4 | (8 << 8));
    t.c(Op::BitExtendI, u(0x0000_0F00), desc, &[u32::MAX]);
    t.c(Op::BitExtendI, u(0x0000_0700), desc, &[7]);
    t.c(Op::BitExtendU, u(0x0000_0F00), desc, &[15]);
    t.c(Op::BitExtendU, u(0xFFFF_FFFF), u(0), &[0]);
    let out = t.run(Op::BitCopyI, u(0x5), desc, u(0xFFFF_FFFF));
    assert_eq!(out.c[0], 0xFFFF_F5FF);
}

fn check_all(format: ProgsFormat) {
    let mut t = Tester { format, covered: BTreeSet::new() };
    arithmetic(&mut t);
    comparisons(&mut t);
    strings(&mut t);
    logic(&mut t);
    branches(&mut t);
    stores(&mut t);
    entities(&mut t);
    pointers(&mut t);
    compound(&mut t);
    hexen2_arrays(&mut t);
    animation(&mut t);
    random(&mut t);
    switches(&mut t);
    calls(&mut t);
    integers(&mut t);
    mixed(&mut t);
    globals_indexed(&mut t);
    push_and_faults(&mut t);
    unsigned_and_wide(&mut t);
    bitfields(&mut t);
    let missing: Vec<&str> = Op::ALL
        .iter()
        .filter(|op| !t.covered.contains(&(**op as u16)))
        .map(|op| op.name())
        .collect();
    assert!(missing.is_empty(), "opcodes never executed: {missing:?}");
}

#[test]
fn every_opcode_16bit() {
    check_all(ProgsFormat::Fte16);
}

#[test]
fn every_opcode_32bit() {
    check_all(ProgsFormat::Fte32);
}
