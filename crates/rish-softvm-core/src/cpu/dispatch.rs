//! Mnemonic dispatch: routes a decoded instruction to its implementation.

use iced_x86::{Instruction, Register};

use crate::arch::registers::index;
use crate::ops::{arithmetic, branch, data, logic, sse, stack, string, system, x87};
use crate::{CpuError, cpu::Cpu};

impl Cpu {
    pub(crate) fn dispatch(&mut self, instruction: &Instruction) -> Result<(), CpuError> {
        use iced_x86::Mnemonic;
        let mnemonic = instruction.mnemonic();
        if matches!(mnemonic, Mnemonic::Mov)
            && (is_control_register(instruction.op0_register())
                || is_control_register(instruction.op1_register())
                || is_debug_register(instruction.op0_register())
                || is_debug_register(instruction.op1_register()))
        {
            return system::system_op(self, instruction);
        }
        match mnemonic {
            Mnemonic::Nop | Mnemonic::Pause => Ok(()),
            Mnemonic::Mov => data::mov(self, instruction),
            Mnemonic::Lea => data::lea(self, instruction),
            Mnemonic::Movzx | Mnemonic::Movsx | Mnemonic::Movsxd => data::movx(self, instruction),
            Mnemonic::Xchg => data::xchg(self, instruction),
            Mnemonic::Xadd => data::xadd(self, instruction),
            Mnemonic::Cmpxchg => data::cmpxchg(self, instruction),
            Mnemonic::Cmpxchg8b | Mnemonic::Cmpxchg16b => data::cmpxchg8b(self, instruction),
            Mnemonic::Xlatb => data::xlatb(self, instruction),
            Mnemonic::Add => arithmetic::add(self, instruction),
            Mnemonic::Sub => arithmetic::sub(self, instruction),
            Mnemonic::Cmp => arithmetic::cmp(self, instruction),
            Mnemonic::Adc | Mnemonic::Sbb => arithmetic::adc_sbb(self, instruction),
            Mnemonic::Inc | Mnemonic::Dec => arithmetic::inc_dec(self, instruction),
            Mnemonic::Neg => arithmetic::neg(self, instruction),
            Mnemonic::Mul | Mnemonic::Imul | Mnemonic::Div | Mnemonic::Idiv => {
                arithmetic::mul_div(self, instruction)
            }
            Mnemonic::Cbw
            | Mnemonic::Cwde
            | Mnemonic::Cdqe
            | Mnemonic::Cwd
            | Mnemonic::Cdq
            | Mnemonic::Cqo => arithmetic::convert(self, instruction),
            Mnemonic::And | Mnemonic::Or | Mnemonic::Xor => logic::and_or_xor(self, instruction),
            Mnemonic::Test => logic::test(self, instruction),
            Mnemonic::Not
            | Mnemonic::Shl
            | Mnemonic::Shr
            | Mnemonic::Sar
            | Mnemonic::Rol
            | Mnemonic::Ror
            | Mnemonic::Rcl
            | Mnemonic::Rcr => logic::shift_rotate(self, instruction),
            Mnemonic::Bsf
            | Mnemonic::Bsr
            | Mnemonic::Tzcnt
            | Mnemonic::Lzcnt
            | Mnemonic::Bt
            | Mnemonic::Bts
            | Mnemonic::Btr
            | Mnemonic::Btc => logic::bit_scan_test(self, instruction),
            Mnemonic::Push
            | Mnemonic::Pop
            | Mnemonic::Pushf
            | Mnemonic::Pushfq
            | Mnemonic::Pushfd
            | Mnemonic::Popf
            | Mnemonic::Popfq
            | Mnemonic::Popfd
            | Mnemonic::Enter
            | Mnemonic::Leave => stack::push_pop(self, instruction),
            Mnemonic::Jmp => branch::jump(self, instruction),
            Mnemonic::Call => branch::call(self, instruction),
            Mnemonic::Ret | Mnemonic::Retf => branch::ret(self, instruction),
            Mnemonic::Loop
            | Mnemonic::Loope
            | Mnemonic::Loopne
            | Mnemonic::Jcxz
            | Mnemonic::Jecxz
            | Mnemonic::Jrcxz => branch::jump(self, instruction),
            Mnemonic::Jo
            | Mnemonic::Jno
            | Mnemonic::Jb
            | Mnemonic::Jae
            | Mnemonic::Je
            | Mnemonic::Jne
            | Mnemonic::Jbe
            | Mnemonic::Ja
            | Mnemonic::Js
            | Mnemonic::Jns
            | Mnemonic::Jp
            | Mnemonic::Jnp
            | Mnemonic::Jl
            | Mnemonic::Jge
            | Mnemonic::Jle
            | Mnemonic::Jg => branch::jcc(self, instruction),
            Mnemonic::Seto
            | Mnemonic::Setno
            | Mnemonic::Setb
            | Mnemonic::Setae
            | Mnemonic::Sete
            | Mnemonic::Setne
            | Mnemonic::Setbe
            | Mnemonic::Seta
            | Mnemonic::Sets
            | Mnemonic::Setns
            | Mnemonic::Setp
            | Mnemonic::Setnp
            | Mnemonic::Setl
            | Mnemonic::Setge
            | Mnemonic::Setle
            | Mnemonic::Setg => branch::setcc(self, instruction),
            Mnemonic::Stc
            | Mnemonic::Clc
            | Mnemonic::Cmc
            | Mnemonic::Cld
            | Mnemonic::Std
            | Mnemonic::Cli
            | Mnemonic::Sti
            | Mnemonic::Sahf
            | Mnemonic::Lahf => branch::flag_ops(self, instruction),
            Mnemonic::Movaps
            | Mnemonic::Movups
            | Mnemonic::Movapd
            | Mnemonic::Movupd
            | Mnemonic::Movdqa
            | Mnemonic::Movdqu
            | Mnemonic::Movq
            | Mnemonic::Movd
            | Mnemonic::Movss
            | Mnemonic::Movlps
            | Mnemonic::Movlpd
            | Mnemonic::Movhps
            | Mnemonic::Movhpd
            | Mnemonic::Movddup
            | Mnemonic::Movsldup
            | Mnemonic::Movshdup
            | Mnemonic::Xorps
            | Mnemonic::Xorpd
            | Mnemonic::Pxor
            | Mnemonic::Andps
            | Mnemonic::Andpd
            | Mnemonic::Pand
            | Mnemonic::Andnps
            | Mnemonic::Andnpd
            | Mnemonic::Pandn
            | Mnemonic::Orps
            | Mnemonic::Orpd
            | Mnemonic::Por
            | Mnemonic::Pshufd
            | Mnemonic::Pshuflw
            | Mnemonic::Pshufhw
            | Mnemonic::Shufps
            | Mnemonic::Shufpd
            | Mnemonic::Punpcklbw
            | Mnemonic::Punpcklwd
            | Mnemonic::Punpckldq
            | Mnemonic::Punpcklqdq
            | Mnemonic::Punpckhbw
            | Mnemonic::Punpckhwd
            | Mnemonic::Punpckhdq
            | Mnemonic::Punpckhqdq
            | Mnemonic::Movntdq
            | Mnemonic::Movntps
            | Mnemonic::Movntq
            | Mnemonic::Movnti
            | Mnemonic::Pcmpeqb
            | Mnemonic::Pcmpeqw
            | Mnemonic::Pcmpeqd
            | Mnemonic::Pcmpeqq
            | Mnemonic::Psllw
            | Mnemonic::Pslld
            | Mnemonic::Psllq
            | Mnemonic::Psrlw
            | Mnemonic::Psrld
            | Mnemonic::Psraw
            | Mnemonic::Psrad
            | Mnemonic::Psrlq
            | Mnemonic::Pslldq
            | Mnemonic::Psrldq
            | Mnemonic::Cvtsi2sd
            | Mnemonic::Cvtsi2ss
            | Mnemonic::Cvttsd2si
            | Mnemonic::Cvttss2si
            | Mnemonic::Pinsrw
            | Mnemonic::Pextrw
            | Mnemonic::Pmovmskb
            | Mnemonic::Movmskps
            | Mnemonic::Movmskpd
            | Mnemonic::Ucomisd
            | Mnemonic::Comisd
            | Mnemonic::Ucomiss
            | Mnemonic::Comiss
            | Mnemonic::Paddb
            | Mnemonic::Paddw
            | Mnemonic::Paddd
            | Mnemonic::Paddq
            | Mnemonic::Psubb
            | Mnemonic::Psubw
            | Mnemonic::Psubd
            | Mnemonic::Psubq
            | Mnemonic::Pcmpgtb
            | Mnemonic::Pcmpgtw
            | Mnemonic::Pcmpgtd
            | Mnemonic::Pminub
            | Mnemonic::Pmaxub
            | Mnemonic::Pminsw
            | Mnemonic::Pmaxsw
            | Mnemonic::Paddusb
            | Mnemonic::Paddusw
            | Mnemonic::Psubusb
            | Mnemonic::Psubusw
            | Mnemonic::Paddsb
            | Mnemonic::Paddsw
            | Mnemonic::Psubsb
            | Mnemonic::Psubsw
            | Mnemonic::Addsd
            | Mnemonic::Subsd
            | Mnemonic::Mulsd
            | Mnemonic::Divsd
            | Mnemonic::Minsd
            | Mnemonic::Maxsd
            | Mnemonic::Addss
            | Mnemonic::Subss
            | Mnemonic::Mulss
            | Mnemonic::Divss
            | Mnemonic::Minss
            | Mnemonic::Maxss
            | Mnemonic::Sqrtsd
            | Mnemonic::Sqrtss
            | Mnemonic::Addps
            | Mnemonic::Subps
            | Mnemonic::Mulps
            | Mnemonic::Divps
            | Mnemonic::Minps
            | Mnemonic::Maxps
            | Mnemonic::Addpd
            | Mnemonic::Subpd
            | Mnemonic::Mulpd
            | Mnemonic::Divpd
            | Mnemonic::Minpd
            | Mnemonic::Maxpd
            | Mnemonic::Pmuludq
            | Mnemonic::Movhlps
            | Mnemonic::Movlhps
            | Mnemonic::Unpcklps
            | Mnemonic::Unpckhps
            | Mnemonic::Unpcklpd
            | Mnemonic::Unpckhpd
            | Mnemonic::Cvtsd2si
            | Mnemonic::Cvtss2si
            | Mnemonic::Cvtss2sd
            | Mnemonic::Cvtsd2ss
            | Mnemonic::Cmpss
            | Mnemonic::Cmppd
            | Mnemonic::Cmpps
            | Mnemonic::Emms
            | Mnemonic::Femms => sse::sse_op(self, instruction),
            // Cmpsd and Movsd share a mnemonic with the string-compare/move
            // forms; the SSE forms are the ones with an XMM operand.
            Mnemonic::Movsd | Mnemonic::Cmpsd
                if is_xmm_register(instruction.op0_register())
                    || is_xmm_register(instruction.op1_register()) =>
            {
                sse::sse_op(self, instruction)
            }
            Mnemonic::Fld
            | Mnemonic::Fild
            | Mnemonic::Fld1
            | Mnemonic::Fldz
            | Mnemonic::Fldpi
            | Mnemonic::Fldl2e
            | Mnemonic::Fldl2t
            | Mnemonic::Fldlg2
            | Mnemonic::Fldln2
            | Mnemonic::Fst
            | Mnemonic::Fstp
            | Mnemonic::Fist
            | Mnemonic::Fistp
            | Mnemonic::Fisttp
            | Mnemonic::Fadd
            | Mnemonic::Fsub
            | Mnemonic::Fsubr
            | Mnemonic::Fmul
            | Mnemonic::Fdiv
            | Mnemonic::Fdivr
            | Mnemonic::Fiadd
            | Mnemonic::Fisub
            | Mnemonic::Fimul
            | Mnemonic::Fidiv
            | Mnemonic::Faddp
            | Mnemonic::Fsubp
            | Mnemonic::Fsubrp
            | Mnemonic::Fmulp
            | Mnemonic::Fdivp
            | Mnemonic::Fdivrp
            | Mnemonic::Fchs
            | Mnemonic::Fabs
            | Mnemonic::Fsqrt
            | Mnemonic::Frndint
            | Mnemonic::Fxch
            | Mnemonic::Fcom
            | Mnemonic::Fucom
            | Mnemonic::Fcomp
            | Mnemonic::Fucomp
            | Mnemonic::Fcompp
            | Mnemonic::Fucompp
            | Mnemonic::Ficom
            | Mnemonic::Ficomp
            | Mnemonic::Fcomi
            | Mnemonic::Fucomi
            | Mnemonic::Fcomip
            | Mnemonic::Fucomip
            | Mnemonic::Fxam
            | Mnemonic::Ftst
            | Mnemonic::Fldcw
            | Mnemonic::Fnstcw
            | Mnemonic::Fnstsw
            | Mnemonic::Ffree
            | Mnemonic::Ffreep
            | Mnemonic::Fincstp
            | Mnemonic::Fdecstp
            | Mnemonic::Finit
            | Mnemonic::Fninit
            | Mnemonic::Fclex
            | Mnemonic::Fnclex
            | Mnemonic::Fldenv
            | Mnemonic::Frstor
            | Mnemonic::Fnstenv
            | Mnemonic::Fnsave => x87::x87_op(self, instruction),
            Mnemonic::Cmovo
            | Mnemonic::Cmovno
            | Mnemonic::Cmovb
            | Mnemonic::Cmovae
            | Mnemonic::Cmove
            | Mnemonic::Cmovne
            | Mnemonic::Cmovbe
            | Mnemonic::Cmova
            | Mnemonic::Cmovs
            | Mnemonic::Cmovns
            | Mnemonic::Cmovp
            | Mnemonic::Cmovnp
            | Mnemonic::Cmovl
            | Mnemonic::Cmovge
            | Mnemonic::Cmovle
            | Mnemonic::Cmovg => branch::cmovcc(self, instruction),
            Mnemonic::Movsb
            | Mnemonic::Movsw
            | Mnemonic::Movsd
            | Mnemonic::Movsq
            | Mnemonic::Stosb
            | Mnemonic::Stosw
            | Mnemonic::Stosd
            | Mnemonic::Stosq
            | Mnemonic::Lodsb
            | Mnemonic::Lodsw
            | Mnemonic::Lodsd
            | Mnemonic::Lodsq
            | Mnemonic::Scasb
            | Mnemonic::Scasw
            | Mnemonic::Scasd
            | Mnemonic::Scasq
            | Mnemonic::Cmpsb
            | Mnemonic::Cmpsw
            | Mnemonic::Cmpsd
            | Mnemonic::Cmpsq => string::string_op(self, instruction),
            Mnemonic::In | Mnemonic::Out => system::in_out(self, instruction),
            Mnemonic::Hlt => system::hlt(self, instruction),
            Mnemonic::Cpuid => system::cpuid(self, instruction),
            Mnemonic::Rdtsc | Mnemonic::Rdtscp => system::rdtsc(self, instruction),
            Mnemonic::Rdmsr
            | Mnemonic::Wrmsr
            | Mnemonic::Rdpmc
            | Mnemonic::Lgdt
            | Mnemonic::Lidt
            | Mnemonic::Sgdt
            | Mnemonic::Sidt
            | Mnemonic::Lldt
            | Mnemonic::Sldt
            | Mnemonic::Ltr
            | Mnemonic::Str
            | Mnemonic::Clts
            | Mnemonic::Lmsw
            | Mnemonic::Smsw
            | Mnemonic::Invlpg
            | Mnemonic::Wbinvd
            | Mnemonic::Invd
            | Mnemonic::Lfence
            | Mnemonic::Sfence
            | Mnemonic::Mfence
            | Mnemonic::Clac
            | Mnemonic::Stac
            | Mnemonic::Iretd
            | Mnemonic::Iretq
            | Mnemonic::Int
            | Mnemonic::Int3
            | Mnemonic::Into
            | Mnemonic::Bound => system::system_op(self, instruction),
            Mnemonic::Bswap
            | Mnemonic::Popcnt
            | Mnemonic::Ud2
            | Mnemonic::Rdrand
            | Mnemonic::Rdseed
            | Mnemonic::Prefetcht0
            | Mnemonic::Prefetcht1
            | Mnemonic::Prefetcht2
            | Mnemonic::Prefetchnta
            | Mnemonic::Clflush
            | Mnemonic::Clflushopt
            | Mnemonic::Fxsave
            | Mnemonic::Fxsave64
            | Mnemonic::Fxrstor
            | Mnemonic::Fxrstor64
            | Mnemonic::Xsave
            | Mnemonic::Xsave64
            | Mnemonic::Xsaveopt
            | Mnemonic::Xsaveopt64
            | Mnemonic::Xsavec
            | Mnemonic::Xsavec64
            | Mnemonic::Xsaves
            | Mnemonic::Xsaves64
            | Mnemonic::Xrstor
            | Mnemonic::Xrstor64
            | Mnemonic::Xrstors
            | Mnemonic::Xrstors64
            | Mnemonic::Wait
            | Mnemonic::Shld
            | Mnemonic::Shrd
            | Mnemonic::Swapgs
            | Mnemonic::Xgetbv
            | Mnemonic::Xsetbv
            | Mnemonic::Endbr64
            | Mnemonic::Endbr32
            | Mnemonic::Prefetchw
            | Mnemonic::Syscall
            | Mnemonic::Sysret
            | Mnemonic::Sysretq => system::extra_op(self, instruction),
            _ => Err(CpuError::UnimplementedInstruction {
                code: format!("{mnemonic:?}"),
                address: self.regs.rip,
                bytes: Vec::new(),
            }),
        }
    }
}

fn is_xmm_register(register: Register) -> bool {
    matches!(
        register,
        Register::XMM0
            | Register::XMM1
            | Register::XMM2
            | Register::XMM3
            | Register::XMM4
            | Register::XMM5
            | Register::XMM6
            | Register::XMM7
            | Register::XMM8
            | Register::XMM9
            | Register::XMM10
            | Register::XMM11
            | Register::XMM12
            | Register::XMM13
            | Register::XMM14
            | Register::XMM15
    )
}

fn is_control_register(register: Register) -> bool {
    matches!(
        register,
        Register::CR0 | Register::CR2 | Register::CR3 | Register::CR4 | Register::CR8
    )
}

fn is_debug_register(register: Register) -> bool {
    matches!(
        register,
        Register::DR0
            | Register::DR1
            | Register::DR2
            | Register::DR3
            | Register::DR6
            | Register::DR7
    )
}

pub(crate) fn address_size_of(instruction: &Instruction) -> u32 {
    let register = if instruction.memory_base() != Register::None {
        instruction.memory_base()
    } else {
        instruction.memory_index()
    };
    match register {
        Register::None => 8,
        Register::RAX
        | Register::RCX
        | Register::RDX
        | Register::RBX
        | Register::RSP
        | Register::RBP
        | Register::RSI
        | Register::RDI
        | Register::R8
        | Register::R9
        | Register::R10
        | Register::R11
        | Register::R12
        | Register::R13
        | Register::R14
        | Register::R15 => 8,
        Register::EAX
        | Register::ECX
        | Register::EDX
        | Register::EBX
        | Register::ESP
        | Register::EBP
        | Register::ESI
        | Register::EDI
        | Register::R8D
        | Register::R9D
        | Register::R10D
        | Register::R11D
        | Register::R12D
        | Register::R13D
        | Register::R14D
        | Register::R15D => 4,
        _ => 2,
    }
}

/// General-purpose register file index keyed by the iced register discriminant.
/// The iced 1.21 layout packs the GPR encodings contiguously by width — 8-bit
/// (1..=20), 16-bit (21..=36), 32-bit (37..=52), 64-bit (53..=68) — each in
/// RAX,RCX,RDX,RBX,RSP,RBP,RSI,RDI,R8..R15 order, so the file index is a simple
/// offset within each width band. `register_index_matches_the_match` guards this
/// against an iced layout change. Non-GPR discriminants map to RAX.
const REGISTER_INDEX: [u8; 69] = {
    let mut table = [0_u8; 69];
    let mut value = 1;
    while value <= 8 {
        // AL,CL,DL,BL,AH,CH,DH,BH: high and low bytes share a parent.
        table[value] = ((value - 1) % 4) as u8;
        value += 1;
    }
    while value <= 20 {
        table[value] = (value - 5) as u8; // SPL,BPL,SIL,DIL,R8L..R15L
        value += 1;
    }
    while value <= 36 {
        table[value] = (value - 21) as u8; // AX..R15W
        value += 1;
    }
    while value <= 52 {
        table[value] = (value - 37) as u8; // EAX..R15D
        value += 1;
    }
    while value <= 68 {
        table[value] = (value - 53) as u8; // RAX..R15
        value += 1;
    }
    table
};

#[inline]
pub fn register_index(register: Register) -> usize {
    let value = register as usize;
    if value < REGISTER_INDEX.len() {
        REGISTER_INDEX[value] as usize
    } else {
        index::RAX
    }
}

#[cfg(test)]
mod register_index_tests {
    use super::{index, register_index};
    use iced_x86::Register;

    /// Guards the arithmetic `REGISTER_INDEX` table against an iced register
    /// enum reordering: every general-purpose sub-register must fold to the
    /// same file slot its 64-bit parent occupies.
    #[test]
    fn register_index_matches_the_match() {
        let groups: [(usize, [Register; 4]); 16] = [
            (
                index::RAX,
                [Register::RAX, Register::EAX, Register::AX, Register::AL],
            ),
            (
                index::RCX,
                [Register::RCX, Register::ECX, Register::CX, Register::CL],
            ),
            (
                index::RDX,
                [Register::RDX, Register::EDX, Register::DX, Register::DL],
            ),
            (
                index::RBX,
                [Register::RBX, Register::EBX, Register::BX, Register::BL],
            ),
            (
                index::RSP,
                [Register::RSP, Register::ESP, Register::SP, Register::SPL],
            ),
            (
                index::RBP,
                [Register::RBP, Register::EBP, Register::BP, Register::BPL],
            ),
            (
                index::RSI,
                [Register::RSI, Register::ESI, Register::SI, Register::SIL],
            ),
            (
                index::RDI,
                [Register::RDI, Register::EDI, Register::DI, Register::DIL],
            ),
            (
                index::R8,
                [Register::R8, Register::R8D, Register::R8W, Register::R8L],
            ),
            (
                index::R9,
                [Register::R9, Register::R9D, Register::R9W, Register::R9L],
            ),
            (
                index::R10,
                [
                    Register::R10,
                    Register::R10D,
                    Register::R10W,
                    Register::R10L,
                ],
            ),
            (
                index::R11,
                [
                    Register::R11,
                    Register::R11D,
                    Register::R11W,
                    Register::R11L,
                ],
            ),
            (
                index::R12,
                [
                    Register::R12,
                    Register::R12D,
                    Register::R12W,
                    Register::R12L,
                ],
            ),
            (
                index::R13,
                [
                    Register::R13,
                    Register::R13D,
                    Register::R13W,
                    Register::R13L,
                ],
            ),
            (
                index::R14,
                [
                    Register::R14,
                    Register::R14D,
                    Register::R14W,
                    Register::R14L,
                ],
            ),
            (
                index::R15,
                [
                    Register::R15,
                    Register::R15D,
                    Register::R15W,
                    Register::R15L,
                ],
            ),
        ];
        for (expected, registers) in groups {
            for register in registers {
                assert_eq!(register_index(register), expected, "{register:?}");
            }
        }
        // The legacy high bytes fold to their low-byte parent.
        assert_eq!(register_index(Register::AH), index::RAX);
        assert_eq!(register_index(Register::CH), index::RCX);
        assert_eq!(register_index(Register::DH), index::RDX);
        assert_eq!(register_index(Register::BH), index::RBX);
        // Non-GPR registers clamp to RAX.
        assert_eq!(register_index(Register::XMM0), index::RAX);
        assert_eq!(register_index(Register::None), index::RAX);
    }
}
