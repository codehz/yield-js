use oxc::ast::ast::*;
use oxc::span::GetSpan;
use yield_vm::bytecode::{Instr, PropertyDefinition};

use super::{FunctionSource, Lowerer};
use crate::emit::Reg;
use crate::error::{CompileError, unsupported};

impl<'ast, 'borrow, 'src> Lowerer<'ast, 'borrow, 'src> {
    pub(super) fn compile_class_declaration(
        &mut self,
        class: &'ast Class<'ast>,
    ) -> Result<(), CompileError> {
        let id = class.id.as_ref().ok_or_else(|| {
            unsupported(
                self.source_text(),
                class.span,
                "anonymous class declaration",
            )
        })?;
        let (binding, _) = self.declare(id.name.as_str(), VariableDeclarationKind::Let, id.span)?;
        let value = self.compile_class(class, "")?;
        self.emit(Instr::SetBinding {
            binding,
            src: self.slot(value),
        });
        self.env.clear_pending(binding);
        Ok(())
    }

    pub(super) fn compile_class(
        &mut self,
        class: &'ast Class<'ast>,
        inferred_name: &str,
    ) -> Result<Reg, CompileError> {
        if let Some(decorator) = class.decorators.first() {
            return Err(unsupported(
                self.source_text(),
                decorator.span,
                "class decorator",
            ));
        }
        let mut constructor = FunctionSource::DefaultConstructor(class.span);
        for element in &class.body.body {
            match element {
                ClassElement::MethodDefinition(method) => {
                    let feature = if matches!(method.key, PropertyKey::PrivateIdentifier(_)) {
                        Some("private class method")
                    } else if !method.decorators.is_empty() {
                        Some("class method decorator")
                    } else if method.value.generator {
                        Some("generator")
                    } else {
                        None
                    };
                    if let Some(feature) = feature {
                        return Err(unsupported(self.source_text(), method.span, feature));
                    }
                    if method.kind == MethodDefinitionKind::Constructor {
                        if !matches!(constructor, FunctionSource::DefaultConstructor(_)) {
                            return Err(CompileError::new(
                                self.source_text(),
                                method.span,
                                "duplicate class constructor",
                            ));
                        }
                        if method.value.r#async {
                            return Err(unsupported(
                                self.source_text(),
                                method.span,
                                "async constructor",
                            ));
                        }
                        constructor = if class.heritage.is_some() {
                            FunctionSource::DerivedConstructor(&method.value)
                        } else {
                            FunctionSource::Constructor(&method.value)
                        };
                    }
                }
                ClassElement::StaticBlock(_) => {}
                ClassElement::PropertyDefinition(field) => {
                    let feature = if matches!(field.key, PropertyKey::PrivateIdentifier(_)) {
                        Some("private class field")
                    } else if !field.decorators.is_empty() {
                        Some("class field decorator")
                    } else {
                        None
                    };
                    if let Some(feature) = feature {
                        return Err(unsupported(self.source_text(), field.span, feature));
                    }
                }
                other => {
                    return Err(unsupported(
                        self.source_text(),
                        other.span(),
                        "class element",
                    ));
                }
            }
        }

        let parent = self.class_scope;
        let inner = if let Some(id) = &class.id {
            let scope = self.alloc_reg()?;
            self.emit(Instr::NewClassScope {
                dst: self.slot(scope),
                parent: parent.map(|scope| self.slot(scope)),
            });
            self.class_scope = Some(scope);
            self.env.enter_block();
            let (binding, _) =
                self.declare(id.name.as_str(), VariableDeclarationKind::Const, id.span)?;
            self.class_bindings.insert(binding);
            Some((scope, binding))
        } else {
            None
        };
        let name = class
            .id
            .as_ref()
            .map_or(inferred_name, |id| id.name.as_str());
        let result = self.emit_class(class, name, constructor, inner);
        if let Some((_, binding)) = inner {
            self.class_bindings.remove(&binding);
            self.env.exit_block();
        }
        self.class_scope = parent;
        result
    }

    fn emit_class(
        &mut self,
        class: &'ast Class<'ast>,
        name: &str,
        constructor: FunctionSource<'ast>,
        inner: Option<(Reg, yield_vm::BindingId)>,
    ) -> Result<Reg, CompileError> {
        let heritage = class
            .heritage
            .as_ref()
            .map(|heritage| self.compile_expression(&heritage.expression))
            .transpose()?;
        let default_constructor = matches!(constructor, FunctionSource::DefaultConstructor(_));
        let constructor = self.enqueue_function(constructor, name, constructor.span())?;
        let dst = self.alloc_reg()?;
        self.emit(Instr::CreateClass {
            dst: self.slot(dst),
            scope: self.class_scope.map(|scope| self.slot(scope)),
            constructor,
            default_constructor,
            heritage: heritage.map(|reg| self.slot(reg)),
        });
        for element in &class.body.body {
            let method = match element {
                ClassElement::MethodDefinition(method) => method,
                ClassElement::PropertyDefinition(field) => {
                    let key = self.compile_property_key(&field.key, field.computed)?;
                    let initializer = if let Some(value) = &field.value {
                        let function = self.enqueue_function(
                            FunctionSource::ClassField(value),
                            "<class field>",
                            value.span(),
                        )?;
                        Some(self.emit_function_value(function)?)
                    } else {
                        None
                    };
                    self.emit(Instr::AddClassField {
                        class: self.slot(dst),
                        key: self.slot(key),
                        initializer: initializer.map(|reg| self.slot(reg)),
                        is_static: field.r#static,
                    });
                    continue;
                }
                ClassElement::StaticBlock(block) => {
                    let function = self.enqueue_function(
                        FunctionSource::ClassBlock(block),
                        "<class static block>",
                        block.span,
                    )?;
                    let body = self.emit_function_value(function)?;
                    self.emit(Instr::AddClassBlock {
                        class: self.slot(dst),
                        body: self.slot(body),
                    });
                    continue;
                }
                _ => unreachable!("class elements were checked before emission"),
            };
            let (kind, prefix) = match method.kind {
                MethodDefinitionKind::Constructor => continue,
                MethodDefinitionKind::Method => (PropertyDefinition::Data, ""),
                MethodDefinitionKind::Get => (PropertyDefinition::Get, "get "),
                MethodDefinitionKind::Set => (PropertyDefinition::Set, "set "),
            };
            let key = self.compile_property_key(&method.key, method.computed)?;
            let mut buffer = ryu_js::Buffer::new();
            let name = match &method.key {
                PropertyKey::NumericLiteral(literal) => Some(buffer.format(literal.value).into()),
                key => key.static_name().and_then(|name| name.into_cow_str()),
            };
            let function = self.enqueue_function(
                FunctionSource::Method(&method.value),
                format_args!("{prefix}{}", name.as_deref().unwrap_or("")),
                method.value.span,
            )?;
            let src = self.emit_function_value(function)?;
            self.emit(Instr::DefineClassMethod {
                class: self.slot(dst),
                key: self.slot(key),
                src: self.slot(src),
                is_static: method.r#static,
                kind,
            });
        }
        if let Some((scope, binding)) = inner {
            self.emit(Instr::InitClassBinding {
                scope: self.slot(scope),
                binding,
                src: self.slot(dst),
            });
        }
        self.emit(Instr::RunClassStatic {
            class: self.slot(dst),
        });
        Ok(dst)
    }
}
