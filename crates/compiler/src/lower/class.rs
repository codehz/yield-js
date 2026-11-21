use std::collections::{BTreeMap, btree_map::Entry};
use std::rc::Rc;

use oxc::ast::ast::*;
use oxc::span::GetSpan;
use yield_vm::BindingId;
use yield_vm::bytecode::{Instr, PrivateDefinition, PropertyDefinition};

use super::{FunctionSource, Lowerer};
use crate::emit::Reg;
use crate::error::{CompileError, unsupported};

pub(super) struct PrivateScope<'ast> {
    brand: BindingId,
    names: BTreeMap<&'ast str, PrivateName>,
}

struct PrivateName {
    index: u32,
    is_static: bool,
    // Only an unpaired accessor can accept another declaration.
    unpaired_get: Option<bool>,
}

impl<'ast, 'borrow, 'src> Lowerer<'ast, 'borrow, 'src> {
    pub(super) fn load_private_brand(
        &mut self,
        name: &PrivateIdentifier<'_>,
    ) -> Result<(Reg, BindingId, u32), CompileError> {
        let (binding, index) = self
            .private_scopes
            .iter()
            .rev()
            .find_map(|scope| {
                scope
                    .names
                    .get(name.name.as_str())
                    .map(|entry| (scope.brand, entry.index))
            })
            .ok_or_else(|| {
                CompileError::new(
                    self.source_text(),
                    name.span,
                    format!("undeclared private name #{}", name.name),
                )
            })?;
        let brand = self.alloc_reg()?;
        self.emit_binding_read(brand, binding);
        Ok((brand, binding, index))
    }

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
        let value = self.compile_class(class, "", None)?;
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
        name_key: Option<Reg>,
    ) -> Result<Reg, CompileError> {
        if let Some(decorator) = class.decorators.first() {
            return Err(unsupported(
                self.source_text(),
                decorator.span,
                "class decorator",
            ));
        }
        let mut constructor = FunctionSource::DefaultConstructor(class.span);
        let mut names: BTreeMap<_, PrivateName> = BTreeMap::new();
        for element in &class.body.body {
            match element {
                ClassElement::MethodDefinition(method) => {
                    let feature = if !method.decorators.is_empty() {
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
                    let feature = if !field.decorators.is_empty() {
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
            let (key, is_static, accessor) = match element {
                ClassElement::MethodDefinition(method) => (
                    &method.key,
                    method.r#static,
                    match method.kind {
                        MethodDefinitionKind::Get => Some(true),
                        MethodDefinitionKind::Set => Some(false),
                        _ => None,
                    },
                ),
                ClassElement::PropertyDefinition(field) => (&field.key, field.r#static, None),
                _ => continue,
            };
            let PropertyKey::PrivateIdentifier(id) = key else {
                continue;
            };
            let name = id.name.as_str();
            if name == "constructor" {
                return Err(CompileError::new(
                    self.source_text(),
                    id.span,
                    "illegal private name #constructor",
                ));
            }
            let index = names.len() as u32;
            match names.entry(name) {
                Entry::Vacant(entry) => {
                    entry.insert(PrivateName {
                        index,
                        is_static,
                        unpaired_get: accessor,
                    });
                }
                Entry::Occupied(mut entry) => {
                    let prior = entry.get_mut();
                    if prior.is_static != is_static
                        || !matches!(
                            (prior.unpaired_get, accessor),
                            (Some(true), Some(false)) | (Some(false), Some(true))
                        )
                    {
                        return Err(CompileError::new(
                            self.source_text(),
                            id.span,
                            format!("duplicate private name #{name}"),
                        ));
                    }
                    prior.unpaired_get = None;
                }
            }
        }

        let parent = self.class_scope;
        if class.id.is_some() || !names.is_empty() {
            let scope = self.alloc_reg()?;
            self.emit(Instr::NewClassScope {
                dst: self.slot(scope),
                parent: parent.map(|scope| self.slot(scope)),
            });
            self.class_scope = Some(scope);
        }
        let inner = if let Some(id) = &class.id {
            self.env.enter_block();
            let (binding, _) =
                self.declare(id.name.as_str(), VariableDeclarationKind::Const, id.span)?;
            self.class_bindings.insert(binding);
            Some(binding)
        } else {
            None
        };
        let name = class
            .id
            .as_ref()
            .map_or(inferred_name, |id| id.name.as_str());
        let private_scope = (!names.is_empty()).then(|| {
            Rc::new(PrivateScope {
                brand: self.alloc_binding(),
                names,
            })
        });
        let depth = self.private_scopes.len();
        let result = self.emit_class(class, (name, name_key), constructor, inner, private_scope);
        for scope in self.private_scopes.drain(depth..) {
            self.class_bindings.remove(&scope.brand);
        }
        if let Some(binding) = inner {
            self.class_bindings.remove(&binding);
            self.env.exit_block();
        }
        self.class_scope = parent;
        result
    }

    fn emit_class(
        &mut self,
        class: &'ast Class<'ast>,
        (name, name_key): (&str, Option<Reg>),
        constructor: FunctionSource<'ast>,
        inner: Option<BindingId>,
        private_scope: Option<Rc<PrivateScope<'ast>>>,
    ) -> Result<Reg, CompileError> {
        let heritage = class
            .heritage
            .as_ref()
            .map(|heritage| self.compile_expression(&heritage.expression))
            .transpose()?;
        // The heritage sees outer private names only; computed keys and
        // member bodies see all names declared by this class.
        if let Some(scope) = &private_scope {
            self.class_bindings.insert(scope.brand);
            self.private_scopes.push(Rc::clone(scope));
        }
        let default_constructor = matches!(constructor, FunctionSource::DefaultConstructor(_));
        let constructor =
            self.enqueue_named_function(constructor, name, name.to_owned(), constructor.span())?;
        let dst = self.alloc_reg()?;
        self.emit(Instr::CreateClass {
            dst: self.slot(dst),
            scope: self.class_scope.map(|scope| self.slot(scope)),
            constructor,
            default_constructor,
            heritage: heritage.map(|reg| self.slot(reg)),
            name_key: name_key.map(|reg| self.slot(reg)),
            private_count: private_scope
                .as_ref()
                .map_or(0, |scope| scope.names.len() as u32),
            private_binding: private_scope.as_ref().map(|scope| scope.brand),
        });
        if let Some(scope) = &private_scope {
            self.emit(Instr::InitClassBinding {
                scope: self.slot(self.class_scope.expect("private class scope")),
                binding: scope.brand,
                src: self.slot(dst),
            });
        }
        for element in &class.body.body {
            let method = match element {
                ClassElement::MethodDefinition(method) => method,
                ClassElement::PropertyDefinition(field) => {
                    let key = match &field.key {
                        PropertyKey::PrivateIdentifier(_) => None,
                        key => Some(self.compile_property_key(key, field.computed)?),
                    };
                    let name_from_key =
                        field.computed || self.static_key_name(&field.key)?.is_none();
                    let initializer = if let Some(value) = &field.value {
                        let function = self.enqueue_function(
                            FunctionSource::ClassField(value, &field.key, name_from_key),
                            "<class field>",
                            value.span(),
                        )?;
                        Some(self.emit_function_value(function)?)
                    } else {
                        None
                    };
                    if let Some(key) = key {
                        self.emit(Instr::AddClassField {
                            class: self.slot(dst),
                            key: self.slot(key),
                            initializer: initializer.map(|reg| self.slot(reg)),
                            is_static: field.r#static,
                        });
                    } else if let PropertyKey::PrivateIdentifier(id) = &field.key {
                        self.emit(Instr::AddClassPrivate {
                            class: self.slot(dst),
                            index: private_scope.as_ref().unwrap().names[id.name.as_str()].index,
                            lexical: private_scope.as_ref().unwrap().brand,
                            src: initializer.map(|reg| self.slot(reg)),
                            is_static: field.r#static,
                            kind: PrivateDefinition::Field,
                        });
                    }
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
            let key = match &method.key {
                PropertyKey::PrivateIdentifier(_) => None,
                key => Some(self.compile_property_key(key, method.computed)?),
            };
            let name = self.static_key_name(&method.key)?;
            let name_from_key = method.computed || name.is_none();
            let initial_name = if name_from_key {
                String::new()
            } else {
                format!("{prefix}{}", name.as_deref().unwrap_or(""))
            };
            let function = self.enqueue_named_function(
                FunctionSource::Method(&method.value),
                format_args!("{prefix}{}", name.as_deref().unwrap_or("")),
                initial_name,
                method.value.span,
            )?;
            let src = self.emit_function_value(function)?;
            if name_from_key {
                self.emit_function_name(src, key.expect("computed key"), kind);
            }
            if let Some(key) = key {
                self.emit(Instr::DefineClassMethod {
                    class: self.slot(dst),
                    key: self.slot(key),
                    src: self.slot(src),
                    is_static: method.r#static,
                    kind,
                });
            } else if let PropertyKey::PrivateIdentifier(id) = &method.key {
                self.emit(Instr::AddClassPrivate {
                    class: self.slot(dst),
                    index: private_scope.as_ref().unwrap().names[id.name.as_str()].index,
                    lexical: private_scope.as_ref().unwrap().brand,
                    src: Some(self.slot(src)),
                    is_static: method.r#static,
                    kind: match kind {
                        PropertyDefinition::Data => PrivateDefinition::Method,
                        PropertyDefinition::Get => PrivateDefinition::Get,
                        PropertyDefinition::Set => PrivateDefinition::Set,
                    },
                });
            }
        }
        if let Some(binding) = inner {
            self.emit(Instr::InitClassBinding {
                scope: self.slot(self.class_scope.expect("named class scope")),
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
