//! Destructuring evaluates each source once and keeps iterator cleanup explicit
//! in bytecode so both resume edges retain the state needed after `await`.

use oxc::ast::ast::*;
use oxc::span::GetSpan;
use yield_vm::BindingId;
use yield_vm::bytecode::{BinaryOp, Constant, ExceptionHandler, Instr};

use super::Lowerer;
use crate::emit::Reg;
use crate::error::{CompileError, unsupported};

enum Pattern<'ast> {
    Binding(&'ast BindingPattern<'ast>),
    Assignment(&'ast AssignmentTarget<'ast>),
}

enum Destination<'ast> {
    Pattern(Pattern<'ast>),
    Binding(BindingId),
    Member { object: Reg, key: Reg },
}

impl<'ast, 'borrow, 'src> Lowerer<'ast, 'borrow, 'src> {
    pub(super) fn compile_binding_pattern(
        &mut self,
        pattern: &'ast BindingPattern<'ast>,
        value: Reg,
    ) -> Result<(), CompileError> {
        self.compile_pattern(Pattern::Binding(pattern), value)
    }

    pub(super) fn compile_assignment_pattern(
        &mut self,
        target: &'ast AssignmentTarget<'ast>,
        value: Reg,
    ) -> Result<(), CompileError> {
        let destination = self.prepare_pattern(Pattern::Assignment(target))?;
        self.write_pattern(destination, value)
    }

    fn assignment_binding(&self, id: &IdentifierReference<'_>) -> Result<BindingId, CompileError> {
        let name = id.name.as_str();
        let binding = self.lookup(name).ok_or_else(|| {
            CompileError::new(
                self.source_text(),
                id.span,
                format!("assignment to unknown binding {name:?}"),
            )
        })?;
        self.check_tdz(binding, name, id.span)?;
        Ok(binding)
    }

    fn prepare_pattern(
        &mut self,
        pattern: Pattern<'ast>,
    ) -> Result<Destination<'ast>, CompileError> {
        if let Pattern::Assignment(target) = pattern {
            if let AssignmentTarget::AssignmentTargetIdentifier(id) = target {
                return Ok(Destination::Binding(self.assignment_binding(id)?));
            }
            if let Some(member) = target.as_member_expression() {
                let (object, key) = self.compile_member_parts(member)?;
                return Ok(Destination::Member { object, key });
            }
        }
        Ok(Destination::Pattern(pattern))
    }

    fn write_pattern(
        &mut self,
        destination: Destination<'ast>,
        value: Reg,
    ) -> Result<(), CompileError> {
        let src = self.slot(value);
        match destination {
            Destination::Pattern(pattern) => return self.compile_pattern(pattern, value),
            Destination::Binding(binding) => self.emit(Instr::SetBinding { binding, src }),
            Destination::Member { object, key } => self.emit(Instr::SetMember {
                object: self.slot(object),
                key: self.slot(key),
                src,
            }),
        };
        Ok(())
    }

    fn compile_pattern(&mut self, pattern: Pattern<'ast>, value: Reg) -> Result<(), CompileError> {
        match pattern {
            Pattern::Binding(BindingPattern::BindingIdentifier(id)) => {
                let binding = self
                    .lookup(id.name.as_str())
                    .expect("predeclared pattern binding");
                self.env.clear_pending(binding);
                self.write_pattern(Destination::Binding(binding), value)
            }
            Pattern::Binding(BindingPattern::AssignmentPattern(pattern)) => {
                let value = self.compile_pattern_default(value, &pattern.right)?;
                self.compile_binding_pattern(&pattern.left, value)
            }
            Pattern::Binding(BindingPattern::ObjectPattern(pattern)) => {
                self.assert_pattern_object(value);
                let mut excluded = Vec::new();
                for property in &pattern.properties {
                    let key = self.compile_property_key(&property.key, property.computed)?;
                    let item = self.read_pattern_property(value, key)?;
                    self.compile_binding_pattern(&property.value, item)?;
                    if pattern.rest.is_some() {
                        excluded.push(self.slot(key));
                    }
                }
                if let Some(rest) = &pattern.rest {
                    let rest_value = self.emit_object_rest(value, excluded)?;
                    self.compile_binding_pattern(&rest.argument, rest_value)?;
                }
                Ok(())
            }
            Pattern::Assignment(AssignmentTarget::ObjectAssignmentTarget(pattern)) => {
                self.assert_pattern_object(value);
                let mut excluded = Vec::new();
                for property in &pattern.properties {
                    let (key, destination, init) = match property {
                        AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(property) => (
                            self.emit_constant(Constant::String(
                                property.binding.name.as_str().to_owned(),
                            ))?,
                            Destination::Binding(self.assignment_binding(&property.binding)?),
                            property.init.as_ref(),
                        ),
                        AssignmentTargetProperty::AssignmentTargetPropertyProperty(property) => {
                            let key =
                                self.compile_property_key(&property.name, property.computed)?;
                            let (target, init) = assignment_element(&property.binding);
                            (key, self.prepare_pattern(target)?, init)
                        }
                    };
                    let item = self.read_pattern_property(value, key)?;
                    self.write_pattern_element(destination, item, init)?;
                    if pattern.rest.is_some() {
                        excluded.push(self.slot(key));
                    }
                }
                if let Some(rest) = &pattern.rest {
                    let destination = self.prepare_pattern(Pattern::Assignment(&rest.target))?;
                    let rest_value = self.emit_object_rest(value, excluded)?;
                    self.write_pattern(destination, rest_value)?;
                }
                Ok(())
            }
            Pattern::Binding(BindingPattern::ArrayPattern(pattern)) => self.compile_array_pattern(
                value,
                pattern
                    .elements
                    .iter()
                    .map(|el| el.as_ref().map(|el| (Pattern::Binding(el), None))),
                pattern
                    .rest
                    .as_ref()
                    .map(|rest| Pattern::Binding(&rest.argument)),
            ),
            Pattern::Assignment(AssignmentTarget::ArrayAssignmentTarget(pattern)) => self
                .compile_array_pattern(
                    value,
                    pattern
                        .elements
                        .iter()
                        .map(|el| el.as_ref().map(assignment_element)),
                    pattern
                        .rest
                        .as_ref()
                        .map(|rest| Pattern::Assignment(&rest.target)),
                ),
            Pattern::Assignment(target) => Err(unsupported(
                self.source_text(),
                target.span(),
                "assignment pattern target",
            )),
        }
    }

    pub(super) fn compile_pattern_default(
        &mut self,
        value: Reg,
        initializer: &'ast Expression<'ast>,
    ) -> Result<Reg, CompileError> {
        let undefined = self.emit_undefined()?;
        let is_undefined = self.alloc_reg()?;
        self.emit(Instr::Binary {
            dst: self.slot(is_undefined),
            op: BinaryOp::StrictEq,
            lhs: self.slot(value),
            rhs: self.slot(undefined),
        });
        let result = self.alloc_reg()?;
        self.emit(Instr::Move {
            dst: self.slot(result),
            src: self.slot(value),
        });
        let keep = self.branch_false(is_undefined);
        let fallback = self.compile_expression(initializer)?;
        self.emit(Instr::Move {
            dst: self.slot(result),
            src: self.slot(fallback),
        });
        self.patch_jump(keep, self.pc())?;
        Ok(result)
    }

    fn write_pattern_element(
        &mut self,
        destination: Destination<'ast>,
        value: Reg,
        init: Option<&'ast Expression<'ast>>,
    ) -> Result<(), CompileError> {
        let value = if let Some(init) = init {
            self.compile_pattern_default(value, init)?
        } else {
            value
        };
        self.write_pattern(destination, value)
    }

    fn assert_pattern_object(&mut self, value: Reg) {
        self.emit(Instr::AssertObjectCoercible {
            src: self.slot(value),
        });
    }

    fn read_pattern_property(&mut self, value: Reg, key: Reg) -> Result<Reg, CompileError> {
        let dst = self.alloc_reg()?;
        self.emit(Instr::GetDestructuredMember {
            dst: self.slot(dst),
            object: self.slot(value),
            key: self.slot(key),
        });
        Ok(dst)
    }

    fn emit_object_rest(
        &mut self,
        value: Reg,
        excluded: Vec<yield_vm::bytecode::Slot>,
    ) -> Result<Reg, CompileError> {
        let dst = self.alloc_reg()?;
        self.emit(Instr::ObjectRest {
            dst: self.slot(dst),
            src: self.slot(value),
            excluded,
        });
        Ok(dst)
    }

    fn compile_array_pattern(
        &mut self,
        iterable: Reg,
        elements: impl Iterator<Item = Option<(Pattern<'ast>, Option<&'ast Expression<'ast>>)>>,
        rest: Option<Pattern<'ast>>,
    ) -> Result<(), CompileError> {
        let iterator = self.alloc_reg()?;
        let next = self.alloc_reg()?;
        let done = self.emit_constant(Constant::Bool(false))?;
        let closed = self.emit_constant(Constant::Bool(true))?;
        self.emit(Instr::GetIterator {
            iterator: self.slot(iterator),
            next: self.slot(next),
            iterable: self.slot(iterable),
        });
        let start = self.pc();
        for element in elements {
            if let Some((target, init)) = element {
                let destination = self.prepare_pattern(target)?;
                let value = self.emit_undefined()?;
                self.step_pattern_iterator(iterator, next, done, closed, Some(value))?;
                self.write_pattern_element(destination, value, init)?;
            } else {
                self.step_pattern_iterator(iterator, next, done, closed, None)?;
            }
        }
        if let Some(rest) = rest {
            let destination = self.prepare_pattern(rest)?;
            let array = self.alloc_reg()?;
            self.emit(Instr::NewArray {
                dst: self.slot(array),
                elements: Vec::new(),
            });
            let value = self.emit_undefined()?;
            let step = self.pc();
            self.step_pattern_iterator(iterator, next, done, closed, Some(value))?;
            let append = self.branch_false(done);
            let exhausted = self.jump();
            self.patch_jump(append, self.pc())?;
            self.emit(Instr::ArrayAppend {
                array: self.slot(array),
                src: self.slot(value),
            });
            self.jump_to(step);
            self.patch_jump(exhausted, self.pc())?;
            self.write_pattern(destination, array)?;
        }
        let end = self.pc();
        self.close_pattern_iterator(iterator, done, false)?;
        if start != end {
            let exit = self.jump();
            let exception = self.alloc_reg()?;
            let handler = self.pc();
            self.close_pattern_iterator(iterator, done, true)?;
            self.emit(Instr::Throw {
                src: self.slot(exception),
            });
            self.control.push_handler(ExceptionHandler {
                start_pc: start as u32,
                end_pc: end as u32,
                target_pc: handler as u32,
                dst: self.slot(exception),
            });
            self.patch_jump(exit, self.pc())?;
        }
        Ok(())
    }

    fn step_pattern_iterator(
        &mut self,
        iterator: Reg,
        next: Reg,
        done: Reg,
        closed: Reg,
        value: Option<Reg>,
    ) -> Result<(), CompileError> {
        let step = self.branch_false(done);
        let exhausted = self.jump();
        self.patch_jump(step, self.pc())?;
        let iterator = self.slot(iterator);
        let next = self.slot(next);
        let done = self.slot(done);
        // A failing next/done/value read marks the record done. The step only
        // overwrites this slot after it has completed without throwing.
        self.emit(Instr::Move {
            dst: done,
            src: self.slot(closed),
        });
        self.emit(if let Some(value) = value {
            Instr::IteratorNext {
                done,
                value: self.slot(value),
                iterator,
                next,
            }
        } else {
            Instr::IteratorSkip {
                done,
                iterator,
                next,
            }
        });
        self.patch_jump(exhausted, self.pc())
    }

    fn close_pattern_iterator(
        &mut self,
        iterator: Reg,
        done: Reg,
        preserve_throw: bool,
    ) -> Result<(), CompileError> {
        let close = self.branch_false(done);
        let skip = self.jump();
        self.patch_jump(close, self.pc())?;
        self.emit(Instr::IteratorClose {
            iterator: self.slot(iterator),
            preserve_throw,
        });
        self.patch_jump(skip, self.pc())
    }
}

fn assignment_element<'ast>(
    element: &'ast AssignmentTargetMaybeDefault<'ast>,
) -> (Pattern<'ast>, Option<&'ast Expression<'ast>>) {
    if let AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(default) = element {
        (Pattern::Assignment(&default.binding), Some(&default.init))
    } else {
        (Pattern::Assignment(element.to_assignment_target()), None)
    }
}
