//! Versioned bytecode contract shared by the compiler and the VM.
//!
//! The VM owns this module: the compiler depends on it, never the other way
//! around. All operands are explicit slots. No implicit operand stack is
//! persisted across an `Await`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const BYTECODE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotKind {
    /// Discarded when an async frame suspends.
    Transient,
    /// Candidate for retention at a resume point.
    Persistent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Slot {
    pub kind: SlotKind,
    pub index: u16,
}

impl Slot {
    pub const fn transient(index: u16) -> Self {
        Self {
            kind: SlotKind::Transient,
            index,
        }
    }

    pub const fn persistent(index: u16) -> Self {
        Self {
            kind: SlotKind::Persistent,
            index,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Constant {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportDecl {
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Exp,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnaryOp {
    Neg,
    Pos,
    Not,
    BitNot,
    TypeOf,
    Void,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "instruction", rename_all = "snake_case")]
pub enum Instr {
    Const {
        dst: Slot,
        constant: u32,
    },
    Undefined {
        dst: Slot,
    },
    Move {
        dst: Slot,
        src: Slot,
    },
    Binary {
        dst: Slot,
        op: BinaryOp,
        lhs: Slot,
        rhs: Slot,
    },
    Unary {
        dst: Slot,
        op: UnaryOp,
        src: Slot,
    },
    CallImport {
        dst: Slot,
        import: u32,
        args: Vec<Slot>,
    },
    CallFunction {
        dst: Slot,
        function: u32,
        args: Vec<Slot>,
    },
    /// Suspends the current async frame. The VM writes the awaited value to
    /// either resume destination and branches to the matching resume PC.
    Await {
        dst: Slot,
        input: Slot,
        resume: u32,
    },
    Jump {
        target: u32,
    },
    BranchFalse {
        cond: Slot,
        target: u32,
    },
    Return {
        src: Slot,
    },
    Throw {
        src: Slot,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeEntry {
    pub fulfill_pc: u32,
    pub fulfill_dst: Slot,
    pub reject_pc: u32,
    pub reject_dst: Slot,
    /// Strictly ascending persistent-slot indices. Values are stored in this
    /// order in the continuation and snapshot.
    pub saved_slots: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Function {
    pub func_id: u32,
    pub name: String,
    pub is_async: bool,
    pub param_slots: Vec<Slot>,
    pub transient_slots: u16,
    pub persistent_slots: u16,
    pub code: Vec<Instr>,
    pub resumes: Vec<ResumeEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Build {
    pub bytecode_version: u32,
    pub build_id: String,
    pub constants: Vec<Constant>,
    pub imports: Vec<ImportDecl>,
    pub functions: Vec<Function>,
}

#[derive(Serialize)]
struct DigestInput<'a> {
    bytecode_version: u32,
    constants: &'a [Constant],
    imports: &'a [ImportDecl],
    functions: &'a [Function],
}

impl Build {
    pub fn new(
        constants: Vec<Constant>,
        imports: Vec<ImportDecl>,
        functions: Vec<Function>,
    ) -> Self {
        Self {
            bytecode_version: BYTECODE_VERSION,
            build_id: String::new(),
            constants,
            imports,
            functions,
        }
    }

    /// Sets `build_id` to a digest of every execution-relevant field. Debug
    /// metadata is deliberately absent from the contract.
    pub fn finalize(&mut self) {
        self.build_id = self.compute_id();
    }

    pub fn compute_id(&self) -> String {
        let input = DigestInput {
            bytecode_version: self.bytecode_version,
            constants: &self.constants,
            imports: &self.imports,
            functions: &self.functions,
        };
        let bytes = serde_json::to_vec(&input).expect("bytecode digest serialization cannot fail");
        let digest = Sha256::digest(bytes);
        let mut out = String::with_capacity(7 + digest.len() * 2);
        out.push_str("sha256:");
        for byte in digest {
            use std::fmt::Write;
            write!(out, "{byte:02x}").expect("writing to a String cannot fail");
        }
        out
    }

    pub fn function(&self, id: u32) -> Option<&Function> {
        self.functions
            .iter()
            .find(|function| function.func_id == id)
    }

    pub fn function_named(&self, name: &str) -> Option<&Function> {
        self.functions.iter().find(|function| function.name == name)
    }

    /// Structural validation performed before any script code runs.
    pub fn validate(&self) -> Result<(), String> {
        if self.bytecode_version != BYTECODE_VERSION {
            return Err(format!(
                "unsupported bytecode version {}, expected {}",
                self.bytecode_version, BYTECODE_VERSION
            ));
        }
        if self.build_id != self.compute_id() {
            return Err("build_id does not match the bytecode content".to_owned());
        }
        for (index, constant) in self.constants.iter().enumerate() {
            if let Constant::Number(value) = constant
                && !value.is_finite()
            {
                return Err(format!("constant {index} is not a finite number"));
            }
        }

        let mut import_names = std::collections::BTreeSet::new();
        for import in &self.imports {
            if import.name.is_empty() {
                return Err("empty host import name".to_owned());
            }
            if !import_names.insert(import.name.as_str()) {
                return Err(format!("duplicate host import {:?}", import.name));
            }
        }

        let mut func_ids = std::collections::BTreeSet::new();
        let mut func_names = std::collections::BTreeSet::new();
        for function in &self.functions {
            if !func_ids.insert(function.func_id) {
                return Err(format!("duplicate func_id {}", function.func_id));
            }
            if function.name.is_empty() {
                return Err(format!("function {} has an empty name", function.func_id));
            }
            if !func_names.insert(function.name.as_str()) {
                return Err(format!("duplicate function name {:?}", function.name));
            }
        }

        for function in &self.functions {
            self.validate_function(function)?;
        }
        Ok(())
    }

    fn validate_function(&self, function: &Function) -> Result<(), String> {
        let location = format!("function {} ({})", function.func_id, function.name);
        if function.code.is_empty() {
            return Err(format!("{location}: empty code"));
        }
        if function.resumes.len() > u32::MAX as usize {
            return Err(format!("{location}: too many resume entries"));
        }

        let check_slot = |slot: Slot, what: &str| -> Result<(), String> {
            let limit = match slot.kind {
                SlotKind::Transient => function.transient_slots,
                SlotKind::Persistent => function.persistent_slots,
            };
            if slot.index >= limit {
                return Err(format!(
                    "{location}: {what} references out-of-range {:?} slot {} (size {limit})",
                    slot.kind, slot.index
                ));
            }
            Ok(())
        };

        for (index, slot) in function.param_slots.iter().copied().enumerate() {
            check_slot(slot, &format!("parameter {index}"))?;
        }

        for (pc, instr) in function.code.iter().enumerate() {
            let check_target = |target: u32, what: &str| -> Result<(), String> {
                if target as usize >= function.code.len() {
                    return Err(format!(
                        "{location}: {what} at pc {pc} targets pc {target}, code len {}",
                        function.code.len()
                    ));
                }
                Ok(())
            };
            match instr {
                Instr::Const { dst, constant } => {
                    check_slot(*dst, "Const.dst")?;
                    if *constant as usize >= self.constants.len() {
                        return Err(format!(
                            "{location}: Const at pc {pc} references constant {constant}"
                        ));
                    }
                }
                Instr::Undefined { dst } => check_slot(*dst, "Undefined.dst")?,
                Instr::Move { dst, src } => {
                    check_slot(*dst, "Move.dst")?;
                    check_slot(*src, "Move.src")?;
                }
                Instr::Binary { dst, lhs, rhs, .. } => {
                    check_slot(*dst, "Binary.dst")?;
                    check_slot(*lhs, "Binary.lhs")?;
                    check_slot(*rhs, "Binary.rhs")?;
                }
                Instr::Unary { dst, src, .. } => {
                    check_slot(*dst, "Unary.dst")?;
                    check_slot(*src, "Unary.src")?;
                }
                Instr::CallImport { dst, import, args } => {
                    check_slot(*dst, "CallImport.dst")?;
                    if *import as usize >= self.imports.len() {
                        return Err(format!(
                            "{location}: CallImport at pc {pc} references import {import}"
                        ));
                    }
                    for arg in args {
                        check_slot(*arg, "CallImport.arg")?;
                    }
                }
                Instr::CallFunction {
                    dst,
                    function: callee,
                    args,
                } => {
                    check_slot(*dst, "CallFunction.dst")?;
                    if self.function(*callee).is_none() {
                        return Err(format!(
                            "{location}: CallFunction at pc {pc} references function {callee}"
                        ));
                    }
                    for arg in args {
                        check_slot(*arg, "CallFunction.arg")?;
                    }
                }
                Instr::Await { dst, input, resume } => {
                    if !function.is_async {
                        return Err(format!("{location}: Await in a synchronous function"));
                    }
                    check_slot(*dst, "Await.dst")?;
                    check_slot(*input, "Await.input")?;
                    if *resume as usize >= function.resumes.len() {
                        return Err(format!(
                            "{location}: Await at pc {pc} references resume {resume}"
                        ));
                    }
                }
                Instr::Jump { target } => check_target(*target, "Jump")?,
                Instr::BranchFalse { cond, target } => {
                    check_slot(*cond, "BranchFalse.cond")?;
                    check_target(*target, "BranchFalse")?;
                }
                Instr::Return { src } => check_slot(*src, "Return.src")?,
                Instr::Throw { src } => check_slot(*src, "Throw.src")?,
            }
        }

        for (resume_id, resume) in function.resumes.iter().enumerate() {
            let check_pc = |pc: u32, what: &str| -> Result<(), String> {
                if pc as usize >= function.code.len() {
                    return Err(format!(
                        "{location}: resume {resume_id} {what} pc {pc} is out of range"
                    ));
                }
                Ok(())
            };
            check_pc(resume.fulfill_pc, "fulfill")?;
            check_pc(resume.reject_pc, "reject")?;
            check_slot(resume.fulfill_dst, "fulfill_dst")?;
            check_slot(resume.reject_dst, "reject_dst")?;
            let mut previous = None;
            for slot in &resume.saved_slots {
                if *slot >= function.persistent_slots {
                    return Err(format!(
                        "{location}: resume {resume_id} saves persistent slot {slot}, size {}",
                        function.persistent_slots
                    ));
                }
                if previous.is_some_and(|prev| *slot <= prev) {
                    return Err(format!(
                        "{location}: resume {resume_id} saved_slots are not strictly ascending"
                    ));
                }
                previous = Some(*slot);
            }
        }

        Ok(())
    }
}
