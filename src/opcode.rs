// SPDX-License-Identifier: MIT OR Apache-2.0

//! The QuakeC instruction set.
//!
//! One table lists every opcode FTE executes (0–281) together with the kind of each operand. It
//! drives decoding, the load-time operand sanitizer, operand relocation and the disassembler, so
//! the knowledge lives in exactly one place.

/// How an instruction uses one of its three operands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operand {
    /// Refers to a global (read and/or written). Range-checked at load time.
    Global,
    /// A global *index* used as a number (array base of `LOADA_*`, `FETCH_GBL_*` and
    /// `GLOBALADDRESS`). Range-checked like a global but not relocated.
    GlobalIndex,
    /// A signed jump offset relative to the instruction.
    Jump,
    /// A raw immediate (the bounds of `BOUNDCHECK`). Not checked.
    Immediate,
    /// Not used by the instruction. Checked like a global, as FTE does.
    Unused,
}

impl Operand {
    /// Whether the load-time sanitizer requires this operand to name an existing global.
    #[must_use]
    pub const fn is_checked(self) -> bool {
        matches!(self, Self::Global | Self::GlobalIndex | Self::Unused)
    }

    /// Whether the operand is stored relocated (as a byte offset into the globals block).
    #[must_use]
    pub const fn is_relocated(self) -> bool {
        matches!(self, Self::Global | Self::Unused)
    }
}

macro_rules! opcodes {
    ($( $num:literal $variant:ident $name:literal [$a:ident $b:ident $c:ident], )*) => {
        /// A QuakeC opcode.
        ///
        /// The discriminants are FTE's opcode numbers. Two extra values exist only inside a loaded
        /// program: [`Op::Bad`] replaces statements that are invalid or have out-of-range operands,
        /// and [`Op::JumpOutOfRange`] is the target of jumps that leave the statement table. Both
        /// fault when executed.
        #[allow(missing_docs)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(u16)]
        pub enum Op {
            $( $variant = $num, )*
            Bad = 282,
            JumpOutOfRange = 283,
        }

        impl Op {
            /// Number of opcodes that can appear in a progs file (`OP_NUMREALOPS`).
            pub const NUM_REAL: u16 = 282;

            /// Every opcode that can appear in a progs file, in numeric order.
            pub const ALL: [Op; 282] = [ $( Op::$variant, )* ];

            /// Decodes an opcode number. Unknown numbers decode to [`Op::Bad`].
            #[must_use]
            pub const fn from_u32(n: u32) -> Op {
                match n {
                    $( $num => Op::$variant, )*
                    _ => Op::Bad,
                }
            }

            /// FTE's name for the opcode, without the `OP_` prefix.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $( Op::$variant => $name, )*
                    Op::Bad => "BAD",
                    Op::JumpOutOfRange => "JUMP_OUT_OF_RANGE",
                }
            }

            /// How the instruction uses its operands `a`, `b` and `c`.
            #[must_use]
            pub const fn operands(self) -> [Operand; 3] {
                match self {
                    $( Op::$variant => [$a, $b, $c], )*
                    Op::Bad | Op::JumpOutOfRange => [Operand::Unused, Operand::Unused, Operand::Unused],
                }
            }
        }
    };
}

// Operand kinds: G = global, A = global index used as a number, J = jump, I = immediate,
// U = unused.
use Operand::{Global as G, GlobalIndex as A, Immediate as I, Jump as J, Unused as U};

opcodes! {
    0 Done "DONE" [G U U],
    1 MulF "MUL_F" [G G G],
    2 MulV "MUL_V" [G G G],
    3 MulFV "MUL_FV" [G G G],
    4 MulVF "MUL_VF" [G G G],
    5 DivF "DIV_F" [G G G],
    6 AddF "ADD_F" [G G G],
    7 AddV "ADD_V" [G G G],
    8 SubF "SUB_F" [G G G],
    9 SubV "SUB_V" [G G G],
    10 EqF "EQ_F" [G G G],
    11 EqV "EQ_V" [G G G],
    12 EqS "EQ_S" [G G G],
    13 EqE "EQ_E" [G G G],
    14 EqFnc "EQ_FNC" [G G G],
    15 NeF "NE_F" [G G G],
    16 NeV "NE_V" [G G G],
    17 NeS "NE_S" [G G G],
    18 NeE "NE_E" [G G G],
    19 NeFnc "NE_FNC" [G G G],
    20 LeF "LE_F" [G G G],
    21 GeF "GE_F" [G G G],
    22 LtF "LT_F" [G G G],
    23 GtF "GT_F" [G G G],
    24 LoadF "LOAD_F" [G G G],
    25 LoadV "LOAD_V" [G G G],
    26 LoadS "LOAD_S" [G G G],
    27 LoadEnt "LOAD_ENT" [G G G],
    28 LoadFld "LOAD_FLD" [G G G],
    29 LoadFnc "LOAD_FNC" [G G G],
    30 Address "ADDRESS" [G G G],
    31 StoreF "STORE_F" [G G U],
    32 StoreV "STORE_V" [G G U],
    33 StoreS "STORE_S" [G G U],
    34 StoreEnt "STORE_ENT" [G G U],
    35 StoreFld "STORE_FLD" [G G U],
    36 StoreFnc "STORE_FNC" [G G U],
    37 StorePF "STOREP_F" [G G G],
    38 StorePV "STOREP_V" [G G G],
    39 StorePS "STOREP_S" [G G G],
    40 StorePEnt "STOREP_ENT" [G G G],
    41 StorePFld "STOREP_FLD" [G G G],
    42 StorePFnc "STOREP_FNC" [G G G],
    43 Return "RETURN" [G U U],
    44 NotF "NOT_F" [G U G],
    45 NotV "NOT_V" [G U G],
    46 NotS "NOT_S" [G U G],
    47 NotEnt "NOT_ENT" [G U G],
    48 NotFnc "NOT_FNC" [G U G],
    49 IfI "IF_I" [G J U],
    50 IfNotI "IFNOT_I" [G J U],
    51 Call0 "CALL0" [G U U],
    52 Call1 "CALL1" [G U U],
    53 Call2 "CALL2" [G U U],
    54 Call3 "CALL3" [G U U],
    55 Call4 "CALL4" [G U U],
    56 Call5 "CALL5" [G U U],
    57 Call6 "CALL6" [G U U],
    58 Call7 "CALL7" [G U U],
    59 Call8 "CALL8" [G U U],
    60 State "STATE" [G G U],
    61 Goto "GOTO" [J U U],
    62 AndF "AND_F" [G G G],
    63 OrF "OR_F" [G G G],
    64 BitAndF "BITAND_F" [G G G],
    65 BitOrF "BITOR_F" [G G G],
    66 MulStoreF "MULSTORE_F" [G G G],
    67 MulStoreVF "MULSTORE_VF" [G G G],
    68 MulStorePF "MULSTOREP_F" [G G G],
    69 MulStorePVF "MULSTOREP_VF" [G G G],
    70 DivStoreF "DIVSTORE_F" [G G G],
    71 DivStorePF "DIVSTOREP_F" [G G G],
    72 AddStoreF "ADDSTORE_F" [G G G],
    73 AddStoreV "ADDSTORE_V" [G G G],
    74 AddStorePF "ADDSTOREP_F" [G G G],
    75 AddStorePV "ADDSTOREP_V" [G G G],
    76 SubStoreF "SUBSTORE_F" [G G G],
    77 SubStoreV "SUBSTORE_V" [G G G],
    78 SubStorePF "SUBSTOREP_F" [G G G],
    79 SubStorePV "SUBSTOREP_V" [G G G],
    80 FetchGblF "FETCH_GBL_F" [A G G],
    81 FetchGblV "FETCH_GBL_V" [A G G],
    82 FetchGblS "FETCH_GBL_S" [A G G],
    83 FetchGblE "FETCH_GBL_E" [A G G],
    84 FetchGblFnc "FETCH_GBL_FNC" [A G G],
    85 CState "CSTATE" [G G U],
    86 CWState "CWSTATE" [G G U],
    87 ThinkTime "THINKTIME" [G G U],
    88 BitSetStoreF "BITSETSTORE_F" [G G G],
    89 BitSetStorePF "BITSETSTOREP_F" [G G G],
    90 BitClrStoreF "BITCLRSTORE_F" [G G G],
    91 BitClrStorePF "BITCLRSTOREP_F" [G G G],
    92 Rand0 "RAND0" [U U G],
    93 Rand1 "RAND1" [G U G],
    94 Rand2 "RAND2" [G G G],
    95 RandV0 "RANDV0" [U U G],
    96 RandV1 "RANDV1" [G U G],
    97 RandV2 "RANDV2" [G G G],
    98 SwitchF "SWITCH_F" [G J U],
    99 SwitchV "SWITCH_V" [G J U],
    100 SwitchS "SWITCH_S" [G J U],
    101 SwitchE "SWITCH_E" [G J U],
    102 SwitchFnc "SWITCH_FNC" [G J U],
    103 Case "CASE" [G J U],
    104 CaseRange "CASERANGE" [G G J],
    105 Call1H "CALL1H" [G G G],
    106 Call2H "CALL2H" [G G G],
    107 Call3H "CALL3H" [G G G],
    108 Call4H "CALL4H" [G G G],
    109 Call5H "CALL5H" [G G G],
    110 Call6H "CALL6H" [G G G],
    111 Call7H "CALL7H" [G G G],
    112 Call8H "CALL8H" [G G G],
    113 StoreI "STORE_I" [G G U],
    114 StoreIF "STORE_IF" [G G U],
    115 StoreFI "STORE_FI" [G G U],
    116 AddI "ADD_I" [G G G],
    117 AddFI "ADD_FI" [G G G],
    118 AddIF "ADD_IF" [G G G],
    119 SubI "SUB_I" [G G G],
    120 SubFI "SUB_FI" [G G G],
    121 SubIF "SUB_IF" [G G G],
    122 ConvItoF "CONV_ITOF" [G U G],
    123 ConvFtoI "CONV_FTOI" [G U G],
    124 LoadPItoF "LOADP_ITOF" [G U G],
    125 LoadPFtoI "LOADP_FTOI" [G U G],
    126 LoadI "LOAD_I" [G G G],
    127 StorePI "STOREP_I" [G G G],
    128 StorePIF "STOREP_IF" [G G G],
    129 StorePFI "STOREP_FI" [G G G],
    130 BitAndI "BITAND_I" [G G G],
    131 BitOrI "BITOR_I" [G G G],
    132 MulI "MUL_I" [G G G],
    133 DivI "DIV_I" [G G G],
    134 EqI "EQ_I" [G G G],
    135 NeI "NE_I" [G G G],
    136 IfNotS "IFNOT_S" [G J U],
    137 IfS "IF_S" [G J U],
    138 NotI "NOT_I" [G U G],
    139 DivVF "DIV_VF" [G G G],
    140 BitXorI "BITXOR_I" [G G G],
    141 RShiftI "RSHIFT_I" [G G G],
    142 LShiftI "LSHIFT_I" [G G G],
    143 GlobalAddress "GLOBALADDRESS" [A G G],
    144 AddPIW "ADD_PIW" [G G G],
    145 LoadAF "LOADA_F" [A G G],
    146 LoadAV "LOADA_V" [A G G],
    147 LoadAS "LOADA_S" [A G G],
    148 LoadAEnt "LOADA_ENT" [A G G],
    149 LoadAFld "LOADA_FLD" [A G G],
    150 LoadAFnc "LOADA_FNC" [A G G],
    151 LoadAI "LOADA_I" [A G G],
    152 StoreP "STORE_P" [G G U],
    153 LoadP "LOAD_P" [G G G],
    154 LoadPF "LOADP_F" [G G G],
    155 LoadPV "LOADP_V" [G G G],
    156 LoadPS "LOADP_S" [G G G],
    157 LoadPEnt "LOADP_ENT" [G G G],
    158 LoadPFld "LOADP_FLD" [G G G],
    159 LoadPFnc "LOADP_FNC" [G G G],
    160 LoadPI "LOADP_I" [G G G],
    161 LeI "LE_I" [G G G],
    162 GeI "GE_I" [G G G],
    163 LtI "LT_I" [G G G],
    164 GtI "GT_I" [G G G],
    165 LeIF "LE_IF" [G G G],
    166 GeIF "GE_IF" [G G G],
    167 LtIF "LT_IF" [G G G],
    168 GtIF "GT_IF" [G G G],
    169 LeFI "LE_FI" [G G G],
    170 GeFI "GE_FI" [G G G],
    171 LtFI "LT_FI" [G G G],
    172 GtFI "GT_FI" [G G G],
    173 EqIF "EQ_IF" [G G G],
    174 EqFI "EQ_FI" [G G G],
    175 AddSF "ADD_SF" [G G G],
    176 SubS "SUB_S" [G G G],
    177 StorePC "STOREP_C" [G G G],
    178 LoadPC "LOADP_C" [G G G],
    179 MulIF "MUL_IF" [G G G],
    180 MulFI "MUL_FI" [G G G],
    181 MulVI "MUL_VI" [G G G],
    182 MulIV "MUL_IV" [G G G],
    183 DivIF "DIV_IF" [G G G],
    184 DivFI "DIV_FI" [G G G],
    185 BitAndIF "BITAND_IF" [G G G],
    186 BitOrIF "BITOR_IF" [G G G],
    187 BitAndFI "BITAND_FI" [G G G],
    188 BitOrFI "BITOR_FI" [G G G],
    189 AndI "AND_I" [G G G],
    190 OrI "OR_I" [G G G],
    191 AndIF "AND_IF" [G G G],
    192 OrIF "OR_IF" [G G G],
    193 AndFI "AND_FI" [G G G],
    194 OrFI "OR_FI" [G G G],
    195 NeIF "NE_IF" [G G G],
    196 NeFI "NE_FI" [G G G],
    197 GStorePI "GSTOREP_I" [G G U],
    198 GStorePF "GSTOREP_F" [G G U],
    199 GStorePEnt "GSTOREP_ENT" [G G U],
    200 GStorePFld "GSTOREP_FLD" [G G U],
    201 GStorePS "GSTOREP_S" [G G U],
    202 GStorePFnc "GSTOREP_FNC" [G G U],
    203 GStorePV "GSTOREP_V" [G G U],
    204 GAddress "GADDRESS" [G G G],
    205 GLoadI "GLOAD_I" [G U G],
    206 GLoadF "GLOAD_F" [G U G],
    207 GLoadFld "GLOAD_FLD" [G U G],
    208 GLoadEnt "GLOAD_ENT" [G U G],
    209 GLoadS "GLOAD_S" [G U G],
    210 GLoadFnc "GLOAD_FNC" [G U G],
    211 BoundCheck "BOUNDCHECK" [G I I],
    212 Unused "UNUSED" [U U U],
    213 Push "PUSH" [G U G],
    214 Pop "POP" [U U U],
    215 SwitchI "SWITCH_I" [G J U],
    216 GLoadV "GLOAD_V" [G U G],
    217 IfF "IF_F" [G J U],
    218 IfNotF "IFNOT_F" [G J U],
    219 StoreFieldV "STOREF_V" [G G G],
    220 StoreFieldF "STOREF_F" [G G G],
    221 StoreFieldS "STOREF_S" [G G G],
    222 StoreFieldI "STOREF_I" [G G G],
    223 StorePI8 "STOREP_I8" [G G G],
    224 LoadPU8 "LOADP_U8" [G G G],
    225 LeU "LE_U" [G G G],
    226 LtU "LT_U" [G G G],
    227 DivU "DIV_U" [G G G],
    228 RShiftU "RSHIFT_U" [G G G],
    229 AddI64 "ADD_I64" [G G G],
    230 SubI64 "SUB_I64" [G G G],
    231 MulI64 "MUL_I64" [G G G],
    232 DivI64 "DIV_I64" [G G G],
    233 BitAndI64 "BITAND_I64" [G G G],
    234 BitOrI64 "BITOR_I64" [G G G],
    235 BitXorI64 "BITXOR_I64" [G G G],
    236 LShiftI64I "LSHIFT_I64I" [G G G],
    237 RShiftI64I "RSHIFT_I64I" [G G G],
    238 LeI64 "LE_I64" [G G G],
    239 LtI64 "LT_I64" [G G G],
    240 EqI64 "EQ_I64" [G G G],
    241 NeI64 "NE_I64" [G G G],
    242 LeU64 "LE_U64" [G G G],
    243 LtU64 "LT_U64" [G G G],
    244 DivU64 "DIV_U64" [G G G],
    245 RShiftU64I "RSHIFT_U64I" [G G G],
    246 StoreI64 "STORE_I64" [G G U],
    247 StorePI64 "STOREP_I64" [G G G],
    248 StoreFieldI64 "STOREF_I64" [G G G],
    249 LoadI64 "LOAD_I64" [G G G],
    250 LoadAI64 "LOADA_I64" [A G G],
    251 LoadPI64 "LOADP_I64" [G G G],
    252 ConvUI64 "CONV_UI64" [G U G],
    253 ConvII64 "CONV_II64" [G U G],
    254 ConvI64I "CONV_I64I" [G U G],
    255 ConvFD "CONV_FD" [G U G],
    256 ConvDF "CONV_DF" [G U G],
    257 ConvI64F "CONV_I64F" [G U G],
    258 ConvFI64 "CONV_FI64" [G U G],
    259 ConvI64D "CONV_I64D" [G U G],
    260 ConvDI64 "CONV_DI64" [G U G],
    261 AddD "ADD_D" [G G G],
    262 SubD "SUB_D" [G G G],
    263 MulD "MUL_D" [G G G],
    264 DivD "DIV_D" [G G G],
    265 LeD "LE_D" [G G G],
    266 LtD "LT_D" [G G G],
    267 EqD "EQ_D" [G G G],
    268 NeD "NE_D" [G G G],
    269 StorePI16 "STOREP_I16" [G G G],
    270 LoadPI16 "LOADP_I16" [G G G],
    271 LoadPU16 "LOADP_U16" [G G G],
    272 LoadPI8 "LOADP_I8" [G G G],
    273 BitExtendI "BITEXTEND_I" [G G G],
    274 BitExtendU "BITEXTEND_U" [G G G],
    275 BitCopyI "BITCOPY_I" [G G G],
    276 ConvUF "CONV_UF" [G U G],
    277 ConvFU "CONV_FU" [G U G],
    278 ConvU64D "CONV_U64D" [G U G],
    279 ConvDU64 "CONV_DU64" [G U G],
    280 ConvU64F "CONV_U64F" [G U G],
    281 ConvFU64 "CONV_FU64" [G U G],
}

impl Op {
    /// For `CALL0`..`CALL8` and `CALL1H`..`CALL8H`, the number of arguments passed.
    #[must_use]
    pub const fn call_argc(self) -> Option<u8> {
        let n = self as u16;
        match n {
            51..=59 => Some(n.wrapping_sub(51) as u8),
            105..=112 => Some(n.wrapping_sub(104) as u8),
            _ => None,
        }
    }

    /// Whether the instruction is one of the Hexen 2 `CALLnH` forms.
    #[must_use]
    pub const fn is_hexen2_call(self) -> bool {
        matches!(self as u16, 105..=112)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_dense_and_numbered() {
        for (i, op) in Op::ALL.iter().enumerate() {
            assert_eq!(*op as usize, i, "{}", op.name());
            assert_eq!(Op::from_u32(u32::try_from(i).unwrap_or(u32::MAX)), *op);
        }
        assert_eq!(Op::from_u32(282), Op::Bad);
        assert_eq!(Op::from_u32(0xFFFF), Op::Bad);
    }

    #[test]
    fn call_argc() {
        assert_eq!(Op::Call0.call_argc(), Some(0));
        assert_eq!(Op::Call8.call_argc(), Some(8));
        assert_eq!(Op::Call1H.call_argc(), Some(1));
        assert_eq!(Op::Call8H.call_argc(), Some(8));
        assert_eq!(Op::Return.call_argc(), None);
    }
}
