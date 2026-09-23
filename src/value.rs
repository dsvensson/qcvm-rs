// SPDX-License-Identifier: MIT OR Apache-2.0

//! QuakeC value types as seen by the host.

use std::marker::PhantomData;

use crate::progs::Type;

/// A 3-component float vector.
pub type Vec3 = [f32; 3];

/// A reference to an entity: its number (0 is the world).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntRef(pub u32);

impl EntRef {
    /// The world entity (entity 0).
    pub const WORLD: Self = Self(0);
}

/// A string reference: an offset into VM memory, a temp-string handle (`0x8000_0000 | slot`)
/// or a host-interned string (`0xC000_0000 | index`). 0 is the null string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct StrRef(pub u32);

impl StrRef {
    /// The null string.
    pub const NULL: Self = Self(0);

    /// Whether this is the null string reference.
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.0 == 0
    }
}

/// A function reference: `progs_number << 24 | function_index`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FuncRef(pub u32);

impl FuncRef {
    /// The null function.
    pub const NULL: Self = Self(0);

    /// Builds a reference to function `index` of progs `progs`.
    #[must_use]
    pub const fn new(progs: PrNum, index: u32) -> Self {
        Self(((progs.0 as u32) << 24) | (index & 0x00FF_FFFF))
    }

    /// The progs number.
    #[must_use]
    pub const fn progs(self) -> PrNum {
        PrNum((self.0 >> 24) as u8)
    }

    /// The function index within its progs.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0 & 0x00FF_FFFF
    }
}

/// A pointer: a byte address in VM memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Ptr(pub u32);

/// The number of a progs loaded into a VM (0 is the main progs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrNum(pub u8);

/// A field offset in words, as held by field-typed QuakeC values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FieldOfs(pub u32);

mod sealed {
    pub trait Sealed {}
}

/// A value that can be stored in QuakeC globals and entity fields.
pub trait QcValue: Copy + sealed::Sealed {
    /// Size in 32-bit words.
    const WORDS: usize;
    /// The definition type of fields created for this value ([`Vm::ensure_field`]).
    ///
    /// [`Vm::ensure_field`]: crate::Vm::ensure_field
    const TYPE: Type;
    /// Definition types that hold this value.
    #[doc(hidden)]
    fn accepts(ty: Type) -> bool;
    /// Converts from words.
    #[doc(hidden)]
    fn from_words(words: &[u32]) -> Self;
    /// Converts to words (only the first [`QcValue::WORDS`] entries are used).
    #[doc(hidden)]
    fn to_words(self) -> [u32; 3];
}

fn word(words: &[u32], i: usize) -> u32 {
    words.get(i).copied().unwrap_or(0)
}

/// The first listed type is the value's own ([`QcValue::TYPE`]); it may also be read from the
/// others.
macro_rules! scalar {
    ($t:ty, [$own:ident $(, $ty:ident)*], |$w:ident| $from:expr, |$v:ident| $to:expr) => {
        impl sealed::Sealed for $t {}
        impl QcValue for $t {
            const WORDS: usize = 1;
            const TYPE: Type = Type::$own;
            fn accepts(ty: Type) -> bool {
                matches!(ty, Type::$own $(| Type::$ty)*)
            }
            fn from_words(words: &[u32]) -> Self {
                let $w = word(words, 0);
                $from
            }
            fn to_words(self) -> [u32; 3] {
                let $v = self;
                [$to, 0, 0]
            }
        }
    };
}

scalar!(f32, [Float], |w| f32::from_bits(w), |v| v.to_bits());
scalar!(
    i32,
    [Integer, Float, Entity, Field, Function, Pointer, String],
    |w| w.cast_signed(),
    |v| v.cast_unsigned()
);
scalar!(u32, [UInt, Integer, Float, Entity, Field, Function, Pointer, String], |w| w, |v| v);
scalar!(EntRef, [Entity], |w| EntRef(w), |v| v.0);
scalar!(StrRef, [String], |w| StrRef(w), |v| v.0);
scalar!(FuncRef, [Function], |w| FuncRef(w), |v| v.0);
scalar!(Ptr, [Pointer], |w| Ptr(w), |v| v.0);
scalar!(FieldOfs, [Field], |w| FieldOfs(w), |v| v.0);

impl sealed::Sealed for Vec3 {}
impl QcValue for Vec3 {
    const WORDS: usize = 3;
    const TYPE: Type = Type::Vector;
    fn accepts(ty: Type) -> bool {
        ty == Type::Vector
    }
    fn from_words(words: &[u32]) -> Self {
        [0, 1, 2].map(|i| f32::from_bits(word(words, i)))
    }
    fn to_words(self) -> [u32; 3] {
        self.map(f32::to_bits)
    }
}

macro_rules! wide {
    ($t:ty, [$own:ident $(, $ty:ident)*], |$b:ident| $from:expr, |$v:ident| $to:expr) => {
        impl sealed::Sealed for $t {}
        impl QcValue for $t {
            const WORDS: usize = 2;
            const TYPE: Type = Type::$own;
            fn accepts(ty: Type) -> bool {
                matches!(ty, Type::$own $(| Type::$ty)*)
            }
            fn from_words(words: &[u32]) -> Self {
                let $b = u64::from(word(words, 0)) | (u64::from(word(words, 1)) << 32);
                $from
            }
            fn to_words(self) -> [u32; 3] {
                let $v = self;
                let bits: u64 = $to;
                [bits as u32, (bits >> 32) as u32, 0]
            }
        }
    };
}

wide!(i64, [Int64, UInt64], |b| b.cast_signed(), |v| v.cast_unsigned());
wide!(u64, [UInt64, Int64], |b| b, |v| v);
wide!(f64, [Double], |b| f64::from_bits(b), |v| v.to_bits());

/// A typed handle to a global, from [`Vm::global`](crate::Vm::global). Valid for the lifetime of
/// the VM that produced it.
#[derive(Debug)]
pub struct Global<T: QcValue> {
    /// Absolute byte address of the global in VM memory.
    pub(crate) addr: u32,
    pub(crate) _t: PhantomData<fn() -> T>,
}

impl<T: QcValue> Clone for Global<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: QcValue> Copy for Global<T> {}

impl<T: QcValue> Global<T> {
    pub(crate) const fn new(addr: u32) -> Self {
        Self { addr, _t: PhantomData }
    }

    /// The global's byte address in VM memory (usable as a QuakeC pointer).
    #[must_use]
    pub const fn ptr(self) -> Ptr {
        Ptr(self.addr)
    }
}

/// A typed handle to an entity field, from [`Vm::field`](crate::Vm::field). Valid for the
/// lifetime of the VM that produced it.
#[derive(Debug)]
pub struct Field<T: QcValue> {
    /// Word offset within an entity.
    pub(crate) ofs: u32,
    pub(crate) _t: PhantomData<fn() -> T>,
}

impl<T: QcValue> Clone for Field<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: QcValue> Copy for Field<T> {}

impl<T: QcValue> Field<T> {
    pub(crate) const fn new(ofs: u32) -> Self {
        Self { ofs, _t: PhantomData }
    }

    /// The field's word offset (the value QuakeC field variables hold).
    #[must_use]
    pub const fn offset(self) -> FieldOfs {
        FieldOfs(self.ofs)
    }
}

/// An argument for [`Vm::call`](crate::Vm::call).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Arg<'a> {
    /// A float.
    Float(f32),
    /// A vector.
    Vector(Vec3),
    /// An integer.
    Int(i32),
    /// An entity.
    Ent(EntRef),
    /// A string already known to the VM.
    Str(StrRef),
    /// Bytes to pass as a new temp string.
    Bytes(&'a [u8]),
    /// A function.
    Func(FuncRef),
    /// Raw words.
    Raw([u32; 3]),
}

/// The value a QuakeC function returned (the three words of the return slot).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ret(pub [u32; 3]);

impl Ret {
    /// As a float.
    #[must_use]
    pub fn f32(self) -> f32 {
        f32::from_bits(self.0[0])
    }

    /// As a vector.
    #[must_use]
    pub fn vec(self) -> Vec3 {
        self.0.map(f32::from_bits)
    }

    /// As an integer.
    #[must_use]
    pub fn i32(self) -> i32 {
        self.0[0].cast_signed()
    }

    /// As an entity.
    #[must_use]
    pub fn ent(self) -> EntRef {
        EntRef(self.0[0])
    }

    /// As a string reference.
    #[must_use]
    pub fn str_ref(self) -> StrRef {
        StrRef(self.0[0])
    }

    /// As a function reference.
    #[must_use]
    pub fn func(self) -> FuncRef {
        FuncRef(self.0[0])
    }
}
