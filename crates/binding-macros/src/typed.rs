use std::collections::{HashMap, HashSet};

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{
    Attribute, Error, Expr, Fields, FnArg, GenericArgument, Ident, ImplItem, Item, ItemImpl,
    ItemMod, ItemStruct, Lit, LitInt, LitStr, Meta, Path, PathArguments, Result, ReturnType,
    Signature, Token, Type,
    ext::IdentExt,
    parse::{ParseStream, Parser},
    punctuated::Punctuated,
    spanned::Spanned,
};

use super::state_machine;

fn options(tokens: TokenStream, allowed: &[&str]) -> Result<HashMap<String, Meta>> {
    let mut options = HashMap::new();
    for meta in Punctuated::<Meta, Token![,]>::parse_terminated.parse2(tokens)? {
        let key = meta.path().get_ident().map(ToString::to_string);
        let Some(key) = key.filter(|key| allowed.contains(&key.as_str())) else {
            return Err(Error::new_spanned(meta.path(), "unknown attribute option"));
        };
        if options.contains_key(&key) {
            return Err(Error::new_spanned(
                meta.path(),
                "duplicate attribute option",
            ));
        }
        options.insert(key, meta);
    }
    Ok(options)
}

fn value(meta: &Meta) -> Result<&Expr> {
    match meta {
        Meta::NameValue(meta) => Ok(&meta.value),
        _ => Err(Error::new_spanned(meta, "expected `key = value`")),
    }
}

fn string(meta: &Meta) -> Result<LitStr> {
    if let Expr::Lit(expr) = value(meta)?
        && let Lit::Str(lit) = &expr.lit
    {
        return Ok(lit.clone());
    }
    Err(Error::new_spanned(meta, "expected a string literal"))
}

fn path(meta: &Meta) -> Result<Path> {
    if let Expr::Path(expr) = value(meta)?
        && expr.qself.is_none()
    {
        return Ok(expr.path.clone());
    }
    Err(Error::new_spanned(meta, "expected a Rust path"))
}

fn required<'a>(opts: &'a HashMap<String, Meta>, key: &str, span: Span) -> Result<&'a Meta> {
    opts.get(key)
        .ok_or_else(|| Error::new(span, format!("missing `{key}` option")))
}

fn take_marker(attrs: &mut Vec<Attribute>, names: &[&str]) -> Result<Option<(String, Attribute)>> {
    let mut marker = None;
    for (i, attr) in attrs.iter().enumerate() {
        if let Some(name) = attr.path().get_ident().map(ToString::to_string)
            && names.contains(&name.as_str())
        {
            if marker.is_some() {
                return Err(Error::new_spanned(
                    attr,
                    "conflicting or repeated binding attributes",
                ));
            }
            marker = Some((i, name));
        }
    }
    Ok(marker.map(|(i, name)| (name, attrs.remove(i))))
}

fn plain_ident(ident: &Ident) -> String {
    let text = ident.to_string();
    text.strip_prefix("r#").unwrap_or(&text).to_owned()
}

fn last(ty: &Type) -> Option<&syn::PathSegment> {
    match ty {
        Type::Path(ty) if ty.qself.is_none() => ty.path.segments.last(),
        _ => None,
    }
}

fn simple(ty: &Type, name: &str) -> bool {
    last(ty).is_some_and(|segment| segment.ident == name && segment.arguments.is_none())
}

fn wrapped<'a>(ty: &'a Type, name: &str) -> Option<&'a Type> {
    let segment = last(ty)?;
    if segment.ident == name
        && let PathArguments::AngleBracketed(args) = &segment.arguments
        && args.args.len() == 1
        && let Some(GenericArgument::Type(inner)) = args.args.first()
    {
        return Some(inner);
    }
    None
}

fn lower_async(
    sig: &mut Signature,
    block: &mut syn::Block,
    kind: &str,
    class: Option<&Type>,
) -> Result<()> {
    let Some(asyncness) = &sig.asyncness else {
        return Ok(());
    };
    if !matches!(kind, "function" | "method") {
        return Err(Error::new_spanned(
            asyncness,
            "async is only supported on #[function] and #[method]",
        ));
    }
    if !matches!(&sig.output, ReturnType::Type(_, ty)
        if wrapped(ty, "JsResult").is_some_and(|inner| simple(inner, "HostStep")))
    {
        return Err(Error::new_spanned(
            &sig.output,
            "async bindings must return JsResult<HostStep>",
        ));
    }
    let body = match class {
        Some(class) => state_machine::lower_method(sig, block, class)?,
        None => state_machine::lower(sig, block)?,
    };
    *block = syn::parse2(body)?;
    sig.asyncness = None;
    Ok(())
}

fn lower_binding(
    sig: &mut Signature,
    block: &mut syn::Block,
    kind: &str,
    class: Option<&Type>,
    module: Option<&Type>,
) -> Result<(Call, Option<TokenStream>)> {
    let call = |sig: &Signature| Call::parse(sig, class, module.is_some());
    let spawn = block.stmts.split_last().and_then(|(tail, prefix)| {
        let syn::Stmt::Expr(Expr::MethodCall(spawn), None) = tail else {
            return None;
        };
        let Some(FnArg::Typed(cx)) = sig.inputs.first() else {
            return None;
        };
        let syn::Pat::Ident(cx) = cx.pat.as_ref() else {
            return None;
        };
        (spawn.method == "spawn"
            && matches!(spawn.receiver.as_ref(), Expr::Path(receiver)
                if receiver.qself.is_none() && receiver.path.is_ident(&cx.ident)))
        .then_some((spawn, prefix))
    });
    let Some((spawn, prefix)) = spawn else {
        if let Some(module) = module.filter(|_| sig.asyncness.is_some()) {
            if kind != "function" {
                return Err(Error::new_spanned(
                    sig,
                    "async is only supported on #[function] and #[method]",
                ));
            }
            let (body, stages) = state_machine::lower_module(sig, &[], block, module, false)?;
            *block = syn::parse2(body)?;
            sig.asyncness = None;
            let call = call(sig)?;
            if !matches!(call.output, Output::Step) {
                return Err(Error::new_spanned(
                    &sig.output,
                    "async bindings must return JsResult<HostStep>",
                ));
            }
            return Ok((call, Some(stages)));
        }
        lower_async(sig, block, kind, class)?;
        return Ok((call(sig)?, None));
    };
    if sig.asyncness.is_some() || !matches!(kind, "function" | "method") {
        return Err(Error::new_spanned(
            spawn,
            "spawn is only supported on synchronous #[function] and #[method] bindings",
        ));
    }
    // Parse the JS-facing signature before changing the Rust entry to return HostStep.
    let call = call(sig)?;
    if !matches!(call.output, Output::Value) {
        return Err(Error::new_spanned(
            &sig.output,
            "spawn bindings must return JsResult<JsValue>",
        ));
    }
    let Some(Expr::Async(background)) = spawn
        .args
        .first()
        .filter(|_| spawn.args.len() == 1 && spawn.turbofish.is_none())
    else {
        return Err(Error::new_spanned(
            spawn,
            "expected cx.spawn(async move { ... })",
        ));
    };
    if background.capture.is_none() {
        return Err(Error::new_spanned(
            background,
            "expected an async move block",
        ));
    }
    let (body, stages) = if let Some(module) = module {
        state_machine::lower_module(sig, prefix, &background.block, module, true)?
    } else {
        state_machine::lower_spawn(sig, prefix, &background.block, class)?
    };
    *block = syn::parse2(body)?;
    sig.output = syn::parse_quote!(-> ::yield_vm::JsResult<::yield_vm::HostStep>);
    Ok((call, Some(stages)))
}

fn same_type(left: &Type, right: &Type) -> bool {
    quote!(#left).to_string() == quote!(#right).to_string()
}

fn generics(generics: &syn::Generics) -> Result<()> {
    if !generics.params.is_empty() || generics.where_clause.is_some() {
        return Err(Error::new_spanned(
            generics,
            "binding declarations cannot be generic",
        ));
    }
    Ok(())
}

enum Output {
    Value,
    Void,
    Instance(Box<Type>),
    Step,
}

struct Call {
    optional: Vec<bool>,
    output: Output,
}

impl Call {
    fn parse(sig: &Signature, receiver: Option<&Type>, module: bool) -> Result<Self> {
        if sig.asyncness.is_some()
            || sig.unsafety.is_some()
            || sig.abi.is_some()
            || sig.variadic.is_some()
        {
            return Err(Error::new_spanned(
                sig,
                "expected a synchronous safe Rust function",
            ));
        }
        generics(&sig.generics)?;
        let mut inputs = sig.inputs.iter().map(|arg| match arg {
            FnArg::Typed(arg) => Ok(arg.ty.as_ref()),
            FnArg::Receiver(_) => Err(Error::new_spanned(
                arg,
                "Rust self receivers are not supported; use This<Class>",
            )),
        });
        let expected = if module {
            "first parameter must be &mut ModuleCx<'_, Self>"
        } else {
            "first parameter must be &mut CallCx<'_>"
        };
        let Some(cx) = inputs.next().transpose()? else {
            return Err(Error::new_spanned(sig, expected));
        };
        let valid_cx = match cx {
            Type::Reference(reference) if reference.mutability.is_some() && reference.lifetime.is_none() => {
                last(&reference.elem).is_some_and(|segment| {
                    segment.ident == if module { "ModuleCx" } else { "CallCx" }
                        && matches!(&segment.arguments, PathArguments::AngleBracketed(args)
                            if args.args.len() == if module { 2 } else { 1 }
                            && matches!(args.args.first(), Some(GenericArgument::Lifetime(lt)) if lt.ident == "_")
                            && (!module || matches!(args.args.last(), Some(GenericArgument::Type(ty)) if simple(ty, "Self"))))
                })
            }
            _ => false,
        };
        if !valid_cx {
            return Err(Error::new_spanned(cx, expected));
        }
        if let Some(class) = receiver {
            let Some(ty) = inputs.next().transpose()? else {
                return Err(Error::new_spanned(
                    sig,
                    "second parameter must be This<Class>",
                ));
            };
            if !wrapped(ty, "This").is_some_and(|ty| same_type(ty, class) || simple(ty, "Self")) {
                return Err(Error::new_spanned(
                    ty,
                    "second parameter must be This<Class> for this class",
                ));
            }
        }
        let mut optional = Vec::new();
        for ty in inputs {
            let ty = ty?;
            if simple(ty, "JsValue") {
                optional.push(false);
            } else if wrapped(ty, "Option").is_some_and(|inner| simple(inner, "JsValue")) {
                optional.push(true);
            } else {
                return Err(Error::new_spanned(
                    ty,
                    "JavaScript parameters must be JsValue or Option<JsValue>",
                ));
            }
        }
        let ReturnType::Type(_, ty) = &sig.output else {
            return Err(Error::new_spanned(sig, "expected a JsResult return type"));
        };
        let output = match wrapped(ty, "JsResult") {
            Some(inner) if simple(inner, "JsValue") => Output::Value,
            Some(inner) if simple(inner, "HostStep") => Output::Step,
            Some(Type::Tuple(tuple)) if tuple.elems.is_empty() => Output::Void,
            Some(inner)
                if wrapped(inner, "Instance")
                    .is_some_and(|class| matches!(class, Type::Path(_))) =>
            {
                let class = wrapped(inner, "Instance").expect("matched Instance");
                Output::Instance(Box::new(if simple(class, "Self") {
                    receiver
                        .ok_or_else(|| Error::new_spanned(class, "Self requires a class impl"))?
                        .clone()
                } else {
                    class.clone()
                }))
            }
            _ => {
                return Err(Error::new_spanned(
                    ty,
                    "expected JsResult<JsValue>, JsResult<()>, JsResult<Instance<Class>> or JsResult<HostStep>",
                ));
            }
        };
        Ok(Self { optional, output })
    }

    fn signature(&self) -> TokenStream {
        let args = self.optional.iter().map(|optional| {
            if *optional {
                quote!(OptionalValue)
            } else {
                quote!(Value)
            }
        });
        let result = match &self.output {
            Output::Value => quote!(Value),
            Output::Void => quote!(Void),
            Output::Instance(class) => quote!(Instance(<#class as ::yield_vm::HostClass>::TAG)),
            Output::Step => quote!(Step),
        };
        quote! {
            ::yield_vm::__macro_support::CallSignature {
                args: &[#(::yield_vm::__macro_support::ArgKind::#args),*],
                result: ::yield_vm::__macro_support::ReturnKind::#result,
            }
        }
    }

    fn adapter(
        &self,
        target: TokenStream,
        receiver: Option<&Type>,
        body_returns_step: bool,
        module: Option<&Type>,
    ) -> TokenStream {
        let decode_this = receiver.map(|class| quote!(let receiver = cx.this::<#class>()?;));
        let pass_this = receiver.map(|_| quote!(receiver,));
        let names = (0..self.optional.len())
            .map(|i| format_ident!("arg_{i}"))
            .collect::<Vec<_>>();
        let decode = names
            .iter()
            .zip(&self.optional)
            .enumerate()
            .map(|(i, (name, optional))| {
                if *optional {
                    quote!(let #name = cx.args().get(#i).cloned();)
                } else {
                    quote!(let #name = cx.arg(#i)?;)
                }
            });
        let call = quote!(#target(cx, #pass_this #(#names),*));
        let result = match &self.output {
            Output::Value if body_returns_step => call,
            Output::Value => quote!(#call.map(::yield_vm::HostStep::Return)),
            Output::Void => {
                quote!({ #call?; Ok(::yield_vm::HostStep::Return(::yield_vm::JsValue::undefined())) })
            }
            Output::Instance(_) => {
                quote!({ let instance = #call?; Ok(::yield_vm::HostStep::Return(instance.value(cx))) })
            }
            Output::Step => call,
        };
        let context = match module {
            Some(ty) => quote!(::yield_vm::ModuleCx<'_, #ty>),
            None => quote!(::yield_vm::binding::CallCx<'_>),
        };
        quote! {
            fn __yield_call(cx: &mut #context) -> ::yield_vm::JsResult<::yield_vm::HostStep> {
                #decode_this
                #(#decode)*
                #result
            }
        }
    }
}

fn reject_markers(attrs: &[Attribute], names: &[&str], message: &str) -> Result<()> {
    if let Some(attr) = attrs
        .iter()
        .find(|attr| names.iter().any(|name| attr.path().is_ident(name)))
    {
        return Err(Error::new_spanned(attr, message));
    }
    Ok(())
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

fn nonempty_string(meta: &Meta) -> Result<LitStr> {
    let name = string(meta)?;
    if name.value().is_empty() {
        return Err(Error::new_spanned(name, "expected a nonempty string"));
    }
    Ok(name)
}

enum BuiltinMemberAction {
    Constant(Ident),
    Handler {
        length: u32,
        call: TokenStream,
    },
    Direct {
        length: u32,
        binding: Option<Ident>,
        getter: bool,
        symbol: Option<Box<Expr>>,
        call: Box<Expr>,
    },
}

struct BuiltinMember {
    name: LitStr,
    prototype: bool,
    cfg: Vec<Attribute>,
    action: BuiltinMemberAction,
}

// Entries are `name: length | name: length as key => handler;`, optionally prefixed with
// `get` for an accessor and `symbol(SYMBOL_ID)` for a symbol property.
fn parse_builtin_calls(input: ParseStream<'_>) -> Result<Vec<(LitStr, BuiltinMemberAction)>> {
    if input.is_empty() {
        return Err(input.error("expected at least one builtin member"));
    }
    let mut members = Vec::new();
    while !input.is_empty() {
        // Keep `get` and `symbol` available as ordinary method names.
        let getter = input.peek(Ident::peek_any)
            && input.peek2(Ident::peek_any)
            && !input.peek2(Token![as])
            && input.fork().call(Ident::parse_any)? == "get";
        if getter {
            input.call(Ident::parse_any)?;
        }
        let symbol = if input.peek(Ident::peek_any)
            && input.peek2(syn::token::Paren)
            && input.fork().call(Ident::parse_any)? == "symbol"
        {
            input.call(Ident::parse_any)?;
            let contents;
            syn::parenthesized!(contents in input);
            let symbol: Expr = contents.parse()?;
            if !contents.is_empty() {
                return Err(contents.error("expected a single symbol expression"));
            }
            Some(Box::new(symbol))
        } else {
            None
        };
        let names = Punctuated::<_, Token![|]>::parse_separated_nonempty_with(input, |input| {
            let ident = input.call(Ident::parse_any)?;
            input.parse::<Token![:]>()?;
            Ok((ident, input.parse::<LitInt>()?))
        })?;
        let binding = if input.peek(Token![as]) {
            input.parse::<Token![as]>()?;
            Some(input.parse::<Ident>()?)
        } else {
            None
        };
        input.parse::<Token![=>]>()?;
        let call: Expr = input.parse()?;
        input.parse::<Token![;]>()?;
        for (ident, length) in names {
            let value = builtin_length(&length)?;
            if getter && value != 0 {
                return Err(Error::new_spanned(
                    length,
                    "a builtin getter must have length 0",
                ));
            }
            members.push((
                LitStr::new(&plain_ident(&ident), ident.span()),
                BuiltinMemberAction::Direct {
                    length: value,
                    binding: binding.clone(),
                    getter,
                    symbol: symbol.clone(),
                    call: Box::new(call.clone()),
                },
            ));
        }
    }
    Ok(members)
}

fn builtin_length(lit: &LitInt) -> Result<u32> {
    if !matches!(lit.suffix(), "" | "u32") {
        return Err(Error::new_spanned(lit, "expected a u32 length literal"));
    }
    lit.base10_parse()
        .map_err(|_| Error::new_spanned(lit, "expected a u32 length literal"))
}

fn builtin_member_cfg(attrs: &[Attribute]) -> Vec<Attribute> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .cloned()
        .collect()
}

fn collect_builtin_members(items: &mut Vec<Item>, constructor: bool) -> Result<Vec<BuiltinMember>> {
    let mut keys = HashSet::new();
    let mut symbols = HashSet::new();
    let mut members = Vec::new();
    let mut push = |name: LitStr, prototype, cfg: &[Attribute], action| {
        let symbol = match &action {
            BuiltinMemberAction::Direct { symbol, .. } => symbol.as_deref(),
            _ => None,
        };
        // Symbol members have both an intrinsic name and a property identity.
        if !keys.insert((prototype, symbol.is_some(), name.value()))
            || symbol.is_some_and(|id| !symbols.insert((prototype, quote!(#id).to_string())))
        {
            return Err(Error::new_spanned(name, "duplicate builtin member name"));
        }
        members.push(BuiltinMember {
            name,
            prototype,
            cfg: cfg.to_vec(),
            action,
        });
        Ok(())
    };
    for mut item in std::mem::take(items) {
        if let Item::Macro(declaration) = &item
            && declaration.ident.is_none()
            && (declaration.mac.path.is_ident("static_calls")
                || declaration.mac.path.is_ident("prototype_calls"))
        {
            let prototype = declaration.mac.path.is_ident("prototype_calls");
            if prototype && !constructor {
                return Err(Error::new_spanned(
                    &declaration.mac.path,
                    "methods marker requires a constructor builtin",
                ));
            }
            let cfg = builtin_member_cfg(&declaration.attrs);
            for (name, action) in declaration.mac.parse_body_with(parse_builtin_calls)? {
                push(name, prototype, &cfg, action)?;
            }
            continue;
        }
        if let Some((prototype, cfg, actions)) = parse_builtin_marker(&mut item, constructor)? {
            for (name, action) in actions {
                push(name, prototype, &cfg, action)?;
            }
        }
        items.push(item);
    }
    Ok(members)
}

fn parse_builtin_marker(
    item: &mut Item,
    constructor: bool,
) -> Result<Option<BuiltinMarkerMembers>> {
    const MARKERS: &[&str] = &["function", "constant", "functions", "methods"];
    let attrs = match item {
        Item::Fn(item) => &mut item.attrs,
        Item::Const(item) => &mut item.attrs,
        other => {
            for (marker, diagnostic) in [
                ("function", "function marker requires a function"),
                ("functions", "functions marker requires a function"),
                ("methods", "methods marker requires a function"),
                ("constant", "constant marker requires a const item"),
            ] {
                reject_markers(item_attrs(other), &[marker], diagnostic)?;
            }
            return Ok(None);
        }
    };
    let Some((kind, marker)) = take_marker(attrs, MARKERS)? else {
        return Ok(None);
    };
    let prototype = kind == "methods";
    if prototype && !constructor {
        return Err(Error::new_spanned(
            marker,
            "methods marker requires a constructor builtin",
        ));
    }
    let cfg = builtin_member_cfg(item_attrs(item));
    let members = if matches!(kind.as_str(), "functions" | "methods") {
        let names = marker.parse_args_with(|input: ParseStream<'_>| {
            input.parse_terminated(
                |input| {
                    let ident: Ident = input.parse()?;
                    input.parse::<Token![:]>()?;
                    let length = builtin_length(&input.parse()?)?;
                    let mapped = if input.peek(Token![=]) {
                        input.parse::<Token![=]>()?;
                        Some(input.parse::<Path>()?)
                    } else {
                        None
                    };
                    let name = LitStr::new(&plain_ident(&ident), ident.span());
                    Ok((name, Some(length), mapped))
                },
                Token![,],
            )
        })?;
        if names.is_empty() {
            return Err(Error::new_spanned(
                marker,
                format!("{kind} marker requires at least one member"),
            ));
        }
        names.into_iter().collect::<Vec<_>>()
    } else {
        let opts = options(
            marker.parse_args()?,
            if kind == "constant" {
                &["name"]
            } else {
                &["name", "length"]
            },
        )?;
        let length = if kind == "constant" {
            None
        } else {
            let meta = required(&opts, "length", marker.span())?;
            let Expr::Lit(expr) = value(meta)? else {
                return Err(Error::new_spanned(meta, "expected a u32 length literal"));
            };
            let Lit::Int(lit) = &expr.lit else {
                return Err(Error::new_spanned(meta, "expected a u32 length literal"));
            };
            Some(builtin_length(lit)?)
        };
        vec![(
            nonempty_string(required(&opts, "name", marker.span())?)?,
            length,
            None,
        )]
    };
    let mut actions = Vec::new();
    for (member, length, mapped) in members {
        let action = match (&*item, kind.as_str(), length) {
            (Item::Const(item), "constant", _) => BuiltinMemberAction::Constant(item.ident.clone()),
            (Item::Fn(item), "function", Some(length)) => {
                let ident = &item.sig.ident;
                let context = builtin_call_context(item, 2);
                BuiltinMemberAction::Handler {
                    length,
                    call: quote!(|vm, host, roots, args, _| self::#ident(vm, #context &args)),
                }
            }
            (Item::Fn(item), "functions" | "methods", Some(length)) => {
                let ident = &item.sig.ident;
                let key = mapped.map_or_else(|| quote!(#member), |path| quote!(#path));
                let context = builtin_call_context(item, if prototype { 4 } else { 3 });
                BuiltinMemberAction::Handler {
                    length,
                    call: if prototype {
                        quote!(|vm, host, roots, args, receiver| self::#ident(vm, #context #key, receiver, &args))
                    } else {
                        quote!(|vm, host, roots, args, _| self::#ident(vm, #context #key, &args))
                    },
                }
            }
            (_, "constant", _) => {
                return Err(Error::new_spanned(
                    &marker,
                    "constant marker requires a const item",
                ));
            }
            _ => {
                return Err(Error::new_spanned(
                    &marker,
                    format!("{kind} marker requires a function"),
                ));
            }
        };
        actions.push((member, action));
    }
    Ok(Some((prototype, cfg, actions)))
}

fn builtin_call_context(item: &syn::ItemFn, hostless_arity: usize) -> TokenStream {
    // Host-bearing implementations take both host and roots between VM and member arguments.
    if item.sig.inputs.len() == hostless_arity {
        quote!()
    } else {
        quote!(host, roots,)
    }
}

type BuiltinMarkerMembers = (bool, Vec<Attribute>, Vec<(LitStr, BuiltinMemberAction)>);

pub(super) fn builtin_global(args: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let mut module: ItemMod = syn::parse2(item)?;
    let opts = options(args, &["name", "root", "kind"])?;
    let span = module.ident.span();
    let name = nonempty_string(required(&opts, "name", span)?)?;
    let root = nonempty_string(required(&opts, "root", span)?)?;
    let constructor = if let Some(kind) = opts.get("kind") {
        let kind = string(kind)?;
        if kind.value() != "constructor" {
            return Err(Error::new_spanned(kind, "expected `constructor` kind"));
        }
        true
    } else {
        false
    };
    let object = if constructor {
        quote!(constructor)
    } else {
        quote!(object)
    };
    let Some((_, items)) = &mut module.content else {
        return Err(Error::new_spanned(
            module,
            "builtin_global requires an inline module",
        ));
    };

    let members = collect_builtin_members(items, constructor)?;
    let mut install = Vec::new();
    let mut descriptors = Vec::new();
    for BuiltinMember {
        name: member,
        prototype,
        cfg,
        action,
    } in members
    {
        let target = if prototype {
            quote!(prototype)
        } else {
            object.clone()
        };
        let (getter, symbol) = match &action {
            BuiltinMemberAction::Direct { getter, symbol, .. } => (*getter, symbol.as_deref()),
            _ => (false, None),
        };
        let slot = match (prototype, symbol.is_some(), getter) {
            (false, false, false) => quote!(Static),
            (false, false, true) => quote!(StaticGetter),
            (false, true, false) => quote!(StaticSymbol),
            (false, true, true) => quote!(StaticSymbolGetter),
            (true, false, false) => quote!(Prototype),
            (true, false, true) => quote!(PrototypeGetter),
            (true, true, false) => quote!(PrototypeSymbol),
            (true, true, true) => quote!(PrototypeSymbolGetter),
        };
        let property = match symbol {
            Some(symbol) => quote!(crate::value::PropertyKey::Symbol(#symbol)),
            None if getter => quote!(crate::value::PropertyKey::from(#member)),
            None => quote!(#member),
        };
        let key = quote!(crate::intrinsic::IntrinsicKey::new(NAME, crate::intrinsic::Slot::#slot, #member));
        let (length, call) = match action {
            BuiltinMemberAction::Constant(ident) => {
                install.push(quote! {
                    #(#cfg)*
                    vm.heap.define_data(
                        #target,
                        #member,
                        crate::value::Value::number(self::#ident),
                        crate::heap::Attributes::FIXED,
                    ).expect("builtin stores constants");
                });
                continue;
            }
            BuiltinMemberAction::Handler { length, call } => (length, call),
            BuiltinMemberAction::Direct {
                length,
                binding,
                mut call,
                ..
            } => {
                if let Some(key) = binding {
                    let Expr::Closure(closure) = call.as_mut() else {
                        return Err(Error::new_spanned(
                            call,
                            "a bound builtin call requires a closure",
                        ));
                    };
                    let body = &closure.body;
                    closure.body = syn::parse_quote!({ let #key: &'static str = #member; #body });
                }
                (length, quote!(#call))
            }
        };
        install.push(if getter {
            quote! {
                #(#cfg)*
                crate::builtin::install_getter(vm, #target, #property, #key);
            }
        } else {
            quote! {
                #(#cfg)*
                {
                    let method = vm.alloc_intrinsic(#key);
                    vm.heap.define_data(
                        #target,
                        #property,
                        crate::value::Value::object(method),
                        crate::heap::Attributes::METHOD,
                    ).expect("builtin stores methods");
                }
            }
        });
        descriptors.push(quote! {
            #(#cfg)*
            crate::intrinsic::IntrinsicDef::call(#key, #length, #call),
        });
    }

    items.push(syn::parse_quote!(pub(crate) const NAME: &str = #name;));
    items.push(if constructor {
        syn::parse_quote!(pub(crate) const ROOT: &str = #root;)
    } else {
        syn::parse_quote!(const ROOT: &str = #root;)
    });
    if constructor {
        items.push(syn::parse_quote! {
            pub(crate) fn install_members(
                vm: &mut crate::vm::VmCore,
                constructor: crate::value::ObjectId,
                prototype: crate::value::ObjectId,
            ) {
                #(#install)*
            }
        });
    } else {
        items.push(syn::parse_quote! {
            const ROOTS: &[crate::builtins::RootSpec] = &[
                crate::builtins::RootSpec::new(ROOT, crate::builtins::RootKind::Plain, Some("object.prototype")),
            ];
        });
        items.push(syn::parse_quote! {
            fn install(vm: &mut crate::vm::VmCore, _: &crate::value::Value) {
                let object = vm.builtins.get(ROOT);
                #(#install)*
            }
        });
        items.push(syn::parse_quote! {
            pub(crate) const OWNER: crate::builtin::globals::Owner =
                crate::builtin::globals::Owner::new(ROOT, ROOTS, &[INTRINSICS], Some(install));
        });
    }
    items.push(if constructor {
        syn::parse_quote! {
            pub(crate) const INTRINSICS: &[crate::intrinsic::IntrinsicDef] = &[#(#descriptors)*];
        }
    } else {
        syn::parse_quote! {
            const INTRINSICS: &[crate::intrinsic::IntrinsicDef] = &[#(#descriptors)*];
        }
    });
    Ok(quote!(#module))
}

const EXPORT_MARKERS: &[&str] = &["function", "constructor", "constant"];
const MEMBER_MARKERS: &[&str] = &["method", "get", "set"];

pub(super) fn module_exports(args: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let mut item: ItemImpl = syn::parse2(item)?;
    generics(&item.generics)?;
    if !args.is_empty() || item.trait_.is_some() {
        return Err(Error::new_spanned(
            &item.self_ty,
            "module_exports requires an inherent impl and no options",
        ));
    }
    let module = &item.self_ty;
    let mut exports = Vec::new();
    let mut keys = HashSet::new();
    let mut names = Vec::new();
    for member in &mut item.items {
        let attrs = match member {
            ImplItem::Fn(item) => &mut item.attrs,
            ImplItem::Const(item) => &mut item.attrs,
            ImplItem::Type(item) => &mut item.attrs,
            ImplItem::Macro(item) => &mut item.attrs,
            _ => continue,
        };
        reject_markers(
            attrs,
            MEMBER_MARKERS,
            "member markers require a methods impl",
        )?;
        let Some((kind, attr)) = take_marker(attrs, EXPORT_MARKERS)? else {
            continue;
        };
        let constructable = kind == "constructor";
        let allowed: &[_] = if constructable {
            &["name", "class"]
        } else {
            &["name"]
        };
        let opts = options(attr.parse_args()?, allowed)?;
        let name = string(required(&opts, "name", attr.span())?)?;
        if !keys.insert(name.value()) {
            return Err(Error::new_spanned(name, "duplicate module export name"));
        }
        let (attrs, export) = match member {
            ImplItem::Fn(item) if kind != "constant" => {
                let (call, stages) =
                    lower_binding(&mut item.sig, &mut item.block, &kind, None, Some(module))?;
                let sig = &item.sig;
                let ident = &sig.ident;
                let class = if let Some(class) = opts.get("class") {
                    let class_path = path(class)?;
                    let class_ty = Type::Path(syn::TypePath {
                        qself: None,
                        path: class_path.clone(),
                    });
                    if !matches!(&call.output, Output::Instance(ty) if same_type(ty, &class_ty)) {
                        return Err(Error::new_spanned(
                            &sig.output,
                            "constructor must return JsResult<Instance<Class>> for its declared class",
                        ));
                    }
                    quote!(Some(<#class_path as ::yield_vm::HostClass>::TAG))
                } else {
                    quote!(None)
                };
                let adapter = call.adapter(
                    quote!(<#module>::#ident),
                    None,
                    stages.is_some(),
                    Some(module),
                );
                let signature = call.signature();
                let stages = stages.map_or_else(|| quote!(None), |stages| quote!(Some(#stages)));
                let register = quote! {
                    ::yield_vm::__macro_support::module_function(#name, #constructable, #class,
                        |cx| ::yield_vm::__macro_support::with_module::<#module>(cx, __yield_call),
                        #signature, #stages)
                };
                (
                    &item.attrs,
                    quote!({
                        #adapter
                        #register
                    }),
                )
            }
            ImplItem::Const(item) if kind == "constant" => {
                let ident = &item.ident;
                (
                    &item.attrs,
                    quote!(::yield_vm::__macro_support::constant(#name, <#module>::#ident)),
                )
            }
            _ => {
                return Err(Error::new_spanned(
                    attr,
                    "function and constructor mark functions; constant marks a const item",
                ));
            }
        };
        let cfg = attrs
            .iter()
            .filter(|attr| attr.path().is_ident("cfg"))
            .collect::<Vec<_>>();
        names.push(quote!(#(#cfg)* #name));
        exports.push(quote!(#(#cfg)* #export));
    }
    let cfg = item.attrs.iter().filter(|attr| attr.path().is_ident("cfg"));
    Ok(quote! {
        #item
        #(#cfg)*
        impl ::yield_vm::ModuleExports for #module {
            const EXPORT_NAMES: &'static [&'static str] = &[#(#names),*];

            fn exports() -> ::std::vec::Vec<::yield_vm::__macro_support::ExportDecl> {
                ::std::vec![#(#exports),*]
            }
        }
    })
}

pub(super) struct Slot {
    pub name: Ident,
    pub list: bool,
}

// Only reject handle types that are visible in the field's type syntax. This
// is not an inspection of aliases, user-defined structures or codec bytes.
fn contains_handle(ty: &Type) -> bool {
    match ty {
        Type::Path(ty) => ty
            .qself
            .as_ref()
            .is_some_and(|qself| contains_handle(&qself.ty))
            || ty.path.segments.last().is_some_and(|segment| {
                matches!(
                    segment.ident.to_string().as_str(),
                    "JsValue" | "Instance" | "This"
                )
            }) || ty.path.segments.iter().any(|segment| {
            match &segment.arguments {
                PathArguments::AngleBracketed(args) => args.args.iter().any(|arg| match arg {
                    GenericArgument::Type(ty) => contains_handle(ty),
                    GenericArgument::AssocType(assoc) => contains_handle(&assoc.ty),
                    _ => false,
                }),
                PathArguments::Parenthesized(args) => {
                    args.inputs.iter().any(contains_handle)
                        || matches!(&args.output, ReturnType::Type(_, ty) if contains_handle(ty))
                }
                PathArguments::None => false,
            }
        }),
        Type::Array(ty) => contains_handle(&ty.elem),
        Type::Group(ty) => contains_handle(&ty.elem),
        Type::Paren(ty) => contains_handle(&ty.elem),
        Type::Ptr(ty) => contains_handle(&ty.elem),
        Type::Reference(ty) => contains_handle(&ty.elem),
        Type::Slice(ty) => contains_handle(&ty.elem),
        Type::Tuple(ty) => ty.elems.iter().any(contains_handle),
        _ => false,
    }
}

pub(super) fn extract_slots(item: &mut ItemStruct) -> Result<Vec<Slot>> {
    let mut slots = Vec::new();
    let mut names = HashSet::new();
    let fields = match &mut item.fields {
        Fields::Named(fields) => &mut fields.named,
        Fields::Unnamed(fields) => &mut fields.unnamed,
        Fields::Unit => return Ok(slots),
    };
    for mut field in std::mem::take(fields) {
        if let Some((kind, attr)) = take_marker(&mut field.attrs, &["js_slot", "js_list"])? {
            if !matches!(attr.meta, Meta::Path(_)) {
                return Err(Error::new_spanned(attr, "slot attributes take no options"));
            }
            let Some(name) = field.ident else {
                return Err(Error::new_spanned(
                    attr,
                    "slot attributes require named fields",
                ));
            };
            if !names.insert(plain_ident(&name)) {
                return Err(Error::new_spanned(name, "duplicate slot name"));
            }
            let list = kind == "js_list";
            if list && !wrapped(&field.ty, "Vec").is_some_and(|ty| simple(ty, "JsValue")) {
                return Err(Error::new_spanned(
                    field.ty,
                    "js_list field must be Vec<JsValue>",
                ));
            }
            if !list && !simple(&field.ty, "JsValue") {
                return Err(Error::new_spanned(
                    field.ty,
                    "js_slot field must be JsValue",
                ));
            }
            slots.push(Slot { name, list });
        } else {
            if contains_handle(&field.ty) {
                return Err(Error::new_spanned(
                    field.ty,
                    "JavaScript handles belong in slots, not in payload fields",
                ));
            }
            fields.push(field);
        }
    }
    if !slots.is_empty() && fields.is_empty() {
        item.fields = Fields::Unit;
        item.semi_token = Some(Default::default());
    }
    Ok(slots)
}

pub(super) fn slot_decls(slots: &[Slot]) -> TokenStream {
    let declarations = slots.iter().map(|slot| {
        let name = plain_ident(&slot.name);
        let kind = if slot.list { quote!(JsList) } else { quote!(Js) };
        quote!(::yield_vm::binding::SlotDecl { name: #name, kind: ::yield_vm::binding::SlotKind::#kind })
    });
    quote!(&[#(#declarations),*])
}

fn slot_trait(item: &ItemStruct, slots: &[Slot]) -> Result<TokenStream> {
    if slots.is_empty() {
        return Ok(TokenStream::new());
    }
    let class = &item.ident;
    let vis = &item.vis;
    let trait_name = format_ident!("{}Slots", class);
    let mut names = HashSet::new();
    let mut declarations = Vec::new();
    let mut instance_impl = Vec::new();
    let mut this_impl = Vec::new();
    for (index, slot) in slots.iter().enumerate() {
        let name = &slot.name;
        let mut add = |name: Ident,
                       operation: Ident,
                       mutable: bool,
                       args: &[(&str, TokenStream)],
                       result: TokenStream|
         -> Result<()> {
            if !names.insert(plain_ident(&name)) {
                return Err(Error::new_spanned(
                    name,
                    "duplicate slot helper method name",
                ));
            }
            let mutability = mutable.then(|| quote!(mut));
            let arg_names = args
                .iter()
                .map(|(name, _)| format_ident!("{}", name))
                .collect::<Vec<_>>();
            let arg_types = args.iter().map(|(_, ty)| ty);
            let sig = quote!(fn #name(&self, cx: &#mutability ::yield_vm::binding::CallCx<'_> #(, #arg_names: #arg_types)*) -> ::yield_vm::JsResult<#result>);
            declarations.push(quote!(#sig;));
            instance_impl.push(quote!(#sig { ::yield_vm::Instance::#operation(self, cx, #index #(, #arg_names)*) }));
            this_impl
                .push(quote!(#sig { #trait_name::#name(&self.instance(), cx #(, #arg_names)*) }));
            Ok(())
        };
        let value = quote!(::yield_vm::JsValue);
        if slot.list {
            add(
                format_ident!("{}_len", name),
                format_ident!("list_len"),
                false,
                &[],
                quote!(usize),
            )?;
            add(
                format_ident!("{}_get", name),
                format_ident!("list_get"),
                false,
                &[("position", quote!(usize))],
                value.clone(),
            )?;
            add(
                format_ident!("{}_push", name),
                format_ident!("list_push"),
                true,
                &[("value", value)],
                quote!(()),
            )?;
            add(
                format_ident!("{}_clear", name),
                format_ident!("list_clear"),
                true,
                &[],
                quote!(()),
            )?;
        } else {
            add(
                name.clone(),
                format_ident!("slot"),
                false,
                &[],
                value.clone(),
            )?;
            add(
                format_ident!("set_{}", name),
                format_ident!("set_slot"),
                true,
                &[("value", value)],
                quote!(()),
            )?;
        }
    }
    Ok(quote! {
        #vis trait #trait_name { #(#declarations)* }
        impl #trait_name for ::yield_vm::Instance<#class> { #(#instance_impl)* }
        impl #trait_name for ::yield_vm::This<#class> { #(#this_impl)* }
    })
}

pub(super) fn host_class(args: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let mut item: ItemStruct = syn::parse2(item)?;
    generics(&item.generics)?;
    let opts = options(
        args,
        &["tag", "methods", "parent", "event_target", "event_parent"],
    )?;
    let span = item.ident.span();
    let tag = string(required(&opts, "tag", span)?)?;
    let parent = opts.get("parent").map(path).transpose()?.map(|parent| {
        quote! {
            const PARENT: Option<&'static str> = Some(<#parent as ::yield_vm::HostClass>::TAG);
        }
    });
    let tables = match opts.get("methods") {
        Some(Meta::Path(_)) => Some(quote! {
            const METHODS: &'static [::yield_vm::__macro_support::MethodDecl] = Self::__YIELD_METHODS;
            const ACCESSORS: &'static [::yield_vm::__macro_support::AccessorDecl] = Self::__YIELD_ACCESSORS;
        }),
        Some(other) => return Err(Error::new_spanned(other, "expected bare `methods` marker")),
        None => None,
    };
    let slots = extract_slots(&mut item)?;
    let codec = (!item.fields.is_empty()).then(|| {
        quote! {
            const CODEC: Option<::yield_vm::binding::ClassCodec> =
                Some(::yield_vm::binding::ClassCodec::of::<Self>());
        }
    });
    let event_target = match opts.get("event_target") {
        Some(Meta::Path(_)) => true,
        Some(other) => {
            return Err(Error::new_spanned(
                other,
                "expected bare `event_target` marker",
            ));
        }
        None => false,
    };
    let event_parent = if let Some(meta) = opts.get("event_parent") {
        let field = path(meta)?;
        let field = field
            .get_ident()
            .ok_or_else(|| Error::new_spanned(meta, "expected an own `js_slot` field name"))?;
        let name = plain_ident(field);
        let index = slots
            .iter()
            .position(|slot| !slot.list && plain_ident(&slot.name) == name)
            .ok_or_else(|| {
                Error::new_spanned(field, "event parent must name an own `js_slot` field")
            })?;
        Some(quote!(const EVENT_PARENT: Option<usize> = Some(#index);))
    } else {
        None
    };
    let event_target = (event_target || event_parent.is_some()).then(|| {
        quote!(
            const EVENT_TARGET: bool = true;
        )
    });
    let declarations = slot_decls(&slots);
    let helpers = slot_trait(&item, &slots)?;
    let class = &item.ident;
    Ok(quote! {
        #item
        #helpers
        impl ::yield_vm::HostClass for #class {
            const TAG: &'static str = #tag;
            const SLOTS: &'static [::yield_vm::binding::SlotDecl] = #declarations;
            #parent
            #event_target
            #event_parent
            #tables
            #codec
        }
    })
}

fn member_key(opts: &HashMap<String, Meta>, span: Span) -> Result<((bool, String), TokenStream)> {
    match (opts.get("name"), opts.get("symbol")) {
        (Some(name), None) => {
            let name = string(name)?;
            Ok((
                (false, name.value()),
                quote!(::yield_vm::__macro_support::MemberName::Name(#name)),
            ))
        }
        (None, Some(symbol)) => {
            let symbol_path = path(symbol)?;
            let Some(variant) = symbol_path.get_ident() else {
                return Err(Error::new_spanned(
                    symbol,
                    "expected a WellKnownSymbol variant name",
                ));
            };
            let key = plain_ident(variant);
            if !matches!(
                key.as_str(),
                "Iterator"
                    | "ToPrimitive"
                    | "Species"
                    | "Match"
                    | "MatchAll"
                    | "Replace"
                    | "Search"
                    | "Split"
                    | "HasInstance"
                    | "ToStringTag"
                    | "Dispose"
                    | "AsyncDispose"
            ) {
                return Err(Error::new_spanned(
                    variant,
                    "unknown WellKnownSymbol variant",
                ));
            }
            Ok((
                (true, key),
                quote!(::yield_vm::__macro_support::MemberName::Symbol(::yield_vm::WellKnownSymbol::#variant)),
            ))
        }
        _ => Err(Error::new(
            span,
            "expected exactly one of `name` or `symbol`",
        )),
    }
}

struct Entry {
    adapter: TokenStream,
    signature: TokenStream,
    cfg: Vec<Meta>,
}

impl Entry {
    fn accessor_half(&self) -> TokenStream {
        let Entry {
            adapter,
            signature,
            cfg,
        } = self;
        let present = quote!(Some((__yield_call as ::yield_vm::binding::HostCall, #signature)));
        if cfg.is_empty() {
            quote!({ #adapter #present })
        } else {
            quote!({
                #[cfg(all(#(#cfg),*))]
                let half = { #adapter #present };
                #[cfg(not(all(#(#cfg),*)))]
                let half = None;
                half
            })
        }
    }
}

struct Accessor {
    key: TokenStream,
    get: Option<Entry>,
    set: Option<Entry>,
}

impl Accessor {
    fn tokens(&self) -> TokenStream {
        let key = &self.key;
        let halves = self.get.iter().chain(&self.set).collect::<Vec<_>>();
        let cfg = if halves.iter().any(|half| half.cfg.is_empty()) {
            None
        } else {
            let alternatives = halves.iter().map(|half| {
                let cfg = &half.cfg;
                quote!(all(#(#cfg),*))
            });
            Some(quote!(#[cfg(any(#(#alternatives),*))]))
        };
        let half = |entry: &Option<Entry>| {
            entry
                .as_ref()
                .map_or_else(|| quote!(None), Entry::accessor_half)
        };
        let get = half(&self.get);
        let set = half(&self.set);
        quote!(#cfg ::yield_vm::__macro_support::accessor(#key, #get, #set))
    }
}

pub(super) fn methods(args: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let class: Type = syn::parse2::<syn::TypePath>(args).map(Type::Path)?;
    let mut item: ItemImpl = syn::parse2(item)?;
    generics(&item.generics)?;
    if item.trait_.is_some() || !same_type(&class, &item.self_ty) {
        return Err(Error::new_spanned(
            &item.self_ty,
            "methods requires an inherent impl of the declared class",
        ));
    }
    let mut methods = Vec::new();
    let mut accessors: Vec<Accessor> = Vec::new();
    // None identifies a method; Some(index) identifies a paired accessor.
    let mut keys = HashMap::new();
    for member in &mut item.items {
        let method = match member {
            ImplItem::Fn(method) => method,
            ImplItem::Const(constant) => {
                if constant.ident == "__YIELD_METHODS" || constant.ident == "__YIELD_ACCESSORS" {
                    return Err(Error::new_spanned(
                        &constant.ident,
                        "reserved binding table name",
                    ));
                }
                reject_markers(
                    &constant.attrs,
                    MEMBER_MARKERS,
                    "method markers require a function",
                )?;
                reject_markers(
                    &constant.attrs,
                    EXPORT_MARKERS,
                    "export markers require a module_exports impl",
                )?;
                continue;
            }
            ImplItem::Type(item) => {
                reject_markers(
                    &item.attrs,
                    MEMBER_MARKERS,
                    "method markers require a function",
                )?;
                continue;
            }
            ImplItem::Macro(item) => {
                reject_markers(
                    &item.attrs,
                    MEMBER_MARKERS,
                    "method markers require a function",
                )?;
                continue;
            }
            _ => continue,
        };
        if method.sig.ident == "__YIELD_METHODS" || method.sig.ident == "__YIELD_ACCESSORS" {
            return Err(Error::new_spanned(
                &method.sig.ident,
                "reserved binding table name",
            ));
        }
        reject_markers(
            &method.attrs,
            EXPORT_MARKERS,
            "export markers require a module_exports impl",
        )?;
        let Some((kind, attr)) = take_marker(&mut method.attrs, MEMBER_MARKERS)? else {
            continue;
        };
        let opts = options(attr.parse_args()?, &["name", "symbol"])?;
        let (id, key) = member_key(&opts, attr.span())?;
        let (call, stages) = lower_binding(
            &mut method.sig,
            &mut method.block,
            &kind,
            Some(&class),
            None,
        )?;
        if kind == "get" && !call.optional.is_empty() {
            return Err(Error::new_spanned(
                &method.sig,
                "getter must take no JavaScript parameters",
            ));
        }
        if kind == "get" && matches!(call.output, Output::Step) {
            return Err(Error::new_spanned(
                &method.sig,
                "getter cannot return JsResult<HostStep>",
            ));
        }
        if kind == "set" && (call.optional != [false] || !matches!(call.output, Output::Void)) {
            return Err(Error::new_spanned(
                &method.sig,
                "setter must take one required JsValue and return JsResult<()>",
            ));
        }
        let name = &method.sig.ident;
        let adapter = call.adapter(
            quote!(<#class>::#name),
            Some(&class),
            stages.is_some(),
            None,
        );
        let signature = call.signature();
        let cfg = method
            .attrs
            .iter()
            .filter(|attr| attr.path().is_ident("cfg"));
        if kind == "method" {
            if keys.insert(id, None).is_some() {
                return Err(Error::new_spanned(
                    attr,
                    "duplicate or conflicting member key",
                ));
            }
            let register = if let Some(stages) = stages {
                quote!(::yield_vm::__macro_support::method_cps(#key, __yield_call, #signature, #stages))
            } else {
                quote!(::yield_vm::__macro_support::method(#key, __yield_call, #signature))
            };
            methods.push(quote! {
                #(#cfg)* {
                    #adapter
                    #register
                }
            });
        } else {
            let index = match keys.entry(id).or_insert_with(|| {
                let index = accessors.len();
                accessors.push(Accessor {
                    key,
                    get: None,
                    set: None,
                });
                Some(index)
            }) {
                Some(index) => *index,
                None => {
                    return Err(Error::new_spanned(
                        attr,
                        "duplicate or conflicting member key",
                    ));
                }
            };
            let accessor = &mut accessors[index];
            let half = if kind == "get" {
                &mut accessor.get
            } else {
                &mut accessor.set
            };
            if half.is_some() {
                return Err(Error::new_spanned(
                    attr,
                    if kind == "get" {
                        "duplicate getter"
                    } else {
                        "duplicate setter"
                    },
                ));
            }
            let cfg = cfg.map(Attribute::parse_args).collect::<Result<_>>()?;
            *half = Some(Entry {
                adapter,
                signature,
                cfg,
            });
        }
    }
    let accessors = accessors.iter().map(Accessor::tokens);
    Ok(quote! {
        #item
        impl #class {
            const __YIELD_METHODS: &'static [::yield_vm::__macro_support::MethodDecl] = &[#(#methods),*];
            const __YIELD_ACCESSORS: &'static [::yield_vm::__macro_support::AccessorDecl] = &[#(#accessors),*];
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic(result: Result<TokenStream>, expected: &str) {
        let error = result
            .expect_err("declaration should be rejected")
            .to_string();
        assert!(
            error.contains(expected),
            "expected {expected:?}, got {error:?}"
        );
    }

    #[test]
    fn spawn_function_keeps_original_js_signature() {
        let expanded = module_exports(
            quote!(),
            quote! {
                impl X {
                    #[function(name = "spawn")]
                    fn start(ctx: &mut ModuleCx<'_, Self>, option: Option<JsValue>) -> JsResult<JsValue> {
                        let op = Self::operation(ctx, option)?;
                        ctx.spawn(async move {
                            ctx.effect(op).await?;
                            Ok(())
                        })
                    }
                    #[function(name = "promise")]
                    fn promise(cx: &mut ModuleCx<'_, Self>) -> JsResult<JsValue> {
                        cx.promise(operation())
                    }
                }
            },
        )
        .unwrap()
        .to_string();
        assert_eq!(expanded.matches("module_function").count(), 2);
        assert_eq!(expanded.matches("CpsStage {").count(), 1);
        assert!(expanded.contains("ModuleExports for X"));
        assert_eq!(expanded.matches("with_module :: < X >").count(), 3);
        assert!(expanded.contains("ResumeCall :: Module"));
        assert!(expanded.contains("< X > :: operation"));
        assert!(!expanded.contains("Self :: operation"));
        assert_eq!(expanded.matches("ReturnKind :: Value").count(), 2);
        assert!(expanded.contains("ArgKind :: OptionalValue"));
        assert!(expanded.contains("< X > :: promise (cx ,) . map"));
        assert!(expanded.contains("< X > :: start (cx , arg_0)"));
        assert!(!expanded.contains("< X > :: start (cx , arg_0) . map"));
        assert!(expanded.contains("__spawn"));
    }

    #[test]
    fn spawn_method_registers_stages_and_preserves_this() {
        let expanded = methods(
            quote!(C),
            quote! {
                impl C {
                    #[method(name = "start")]
                    fn start(cx: &mut CallCx<'_>, this: This<Self>) -> JsResult<JsValue> {
                        cx.spawn(async move {
                            cx.effect(operation()).await?;
                            Self::finish(cx, this)
                        })
                    }
                }
            },
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("method_cps"));
        assert!(expanded.contains("ReturnKind :: Value"));
        assert!(expanded.contains("cx . this :: < C >"));
        assert!(expanded.contains(":: finish"));
        assert!(!expanded.contains("Self :: finish"));
    }

    #[test]
    fn rejects_spawn_in_constructors_and_accessors() {
        let expected = "spawn is only supported on synchronous #[function] and #[method]";
        diagnostic(
            module_exports(
                quote!(),
                quote! {
                    impl X {
                        #[constructor(name = "C", class = C)]
                        fn new(cx: &mut ModuleCx<'_, Self>) -> JsResult<Instance<C>> {
                            cx.spawn(async move { Ok(()) })
                        }
                    }
                },
            ),
            expected,
        );
        for (marker, inputs, output) in [
            (quote!(get), quote!(), quote!(JsValue)),
            (quote!(set), quote!(, value: JsValue), quote!(())),
        ] {
            diagnostic(
                methods(
                    quote!(C),
                    quote! {
                        impl C {
                            #[#marker(name = "x")]
                            fn x(cx: &mut CallCx<'_>, this: This<Self> #inputs) -> JsResult<#output> {
                                cx.spawn(async move { Ok(()) })
                            }
                        }
                    },
                ),
                expected,
            );
        }
    }

    #[test]
    fn spawn_requires_value_output_and_move_block() {
        for (output, background, expected) in [
            (
                quote!(HostStep),
                quote!(async move { Ok(()) }),
                "spawn bindings must return JsResult<JsValue>",
            ),
            (
                quote!(JsValue),
                quote!(async { Ok(()) }),
                "expected an async move block",
            ),
        ] {
            diagnostic(
                module_exports(
                    quote!(),
                    quote! {
                        impl X {
                            #[function(name = "start")]
                            fn start(cx: &mut ModuleCx<'_, Self>) -> JsResult<#output> {
                                cx.spawn(#background)
                            }
                        }
                    },
                ),
                expected,
            );
        }
    }

    #[test]
    fn builtin_global_rejects_invalid_declarations() {
        let args = quote!(name = "JSON", root = "json");
        diagnostic(
            builtin_global(
                args.clone(),
                quote! {
                    mod namespace {
                        #[function(name = "parse", length = 2)]
                        fn first() {}
                        #[function(name = "parse", length = 2)]
                        fn second() {}
                    }
                },
            ),
            "duplicate builtin member name",
        );
        diagnostic(
            builtin_global(
                args.clone(),
                quote!(
                    mod namespace;
                ),
            ),
            "inline module",
        );
        diagnostic(
            builtin_global(
                args,
                quote! {
                    mod namespace {
                        #[function(name = "parse", length = 2)]
                        struct Parse;
                    }
                },
            ),
            "function marker requires a function",
        );
        for (args, message) in [
            (quote!(root = "json"), "missing `name` option"),
            (quote!(name = "JSON"), "missing `root` option"),
            (
                quote!(name = "JSON", root = "json", extra = "x"),
                "unknown attribute option",
            ),
            (
                quote!(name = "JSON", root = "json", kind = "namespace"),
                "expected `constructor` kind",
            ),
            (
                quote!(name = "JSON", root = "json", root = "other"),
                "duplicate attribute option",
            ),
            (
                quote!(name = "", root = "json"),
                "expected a nonempty string",
            ),
            (
                quote!(name = "JSON", root = ""),
                "expected a nonempty string",
            ),
        ] {
            diagnostic(builtin_global(args, quote! { mod namespace {} }), message);
        }
    }

    #[test]
    fn builtin_global_validates_constants_and_function_lists() {
        let args = quote!(name = "Math", root = "math");
        for (items, message) in [
            (
                quote! {
                    #[constant(name = "abs")]
                    const ABS: f64 = 1.0;
                    #[functions(abs: 1, acos: 1)]
                    fn dispatch() {}
                },
                "duplicate builtin member name",
            ),
            (
                quote!(
                    #[constant(name = "PI")]
                    fn pi() {}
                ),
                "constant marker requires a const item",
            ),
            (
                quote!(
                    #[functions(abs: 1)]
                    const ABS: f64 = 1.0;
                ),
                "functions marker requires a function",
            ),
            (
                quote!(
                    #[functions()]
                    fn dispatch() {}
                ),
                "functions marker requires at least one member",
            ),
            (
                quote!(
                    #[functions(abs: 1)]
                    #[function(name = "abs", length = 1)]
                    fn dispatch() {}
                ),
                "conflicting or repeated binding attributes",
            ),
        ] {
            diagnostic(
                builtin_global(args.clone(), quote!(mod namespace { #items })),
                message,
            );
        }
    }

    #[test]
    fn builtin_global_accepts_empty_namespace() {
        let expanded = builtin_global(
            quote!(name = "JSON", root = "json"),
            quote! { mod namespace {} },
        )
        .expect("empty builtin namespace should expand")
        .to_string();
        assert!(expanded.contains("pub (crate) const OWNER"));
        assert!(expanded.contains("Owner :: new (ROOT , ROOTS , & [INTRINSICS] , Some (install))"));
    }

    #[test]
    fn builtin_global_validates_constructor_methods() {
        let args = quote!(name = "String", root = "string", kind = "constructor");
        builtin_global(
            args.clone(),
            quote! {
                mod members {
                    #[functions(shared: 2 = Static::Shared)]
                    fn call_static() {}
                    #[methods(shared: 1 = Method::Shared)]
                    fn call_method() {}
                }
            },
        )
        .expect("static and prototype members may share a name");
        for (items, message) in [
            (
                quote! {
                    #[methods()]
                    fn call_method() {}
                },
                "methods marker requires at least one member",
            ),
            (
                quote! {
                    #[methods(charAt: 1)]
                    const CHAR_AT: f64 = 0.0;
                },
                "methods marker requires a function",
            ),
        ] {
            diagnostic(
                builtin_global(args.clone(), quote!(mod members { #items })),
                message,
            );
        }
        diagnostic(
            builtin_global(
                quote!(name = "String", root = "string"),
                quote! {
                    mod members {
                        #[methods(charAt: 1)]
                        fn call_method() {}
                    }
                },
            ),
            "methods marker requires a constructor builtin",
        );
    }

    #[test]
    fn builtin_global_accepts_empty_constructor() {
        builtin_global(
            quote!(name = "String", root = "string", kind = "constructor"),
            quote! { mod members {} },
        )
        .expect("empty builtin constructor should expand");
    }

    #[test]
    fn builtin_global_validates_direct_calls() {
        let constructor = quote!(name = "Example", root = "example", kind = "constructor");
        for items in [
            quote! {
                #[function(name = "shared", length = 1)]
                fn shared() {}
                static_calls! { get shared: 0 => example; }
            },
            quote! {
                prototype_calls! { symbol(MATCH_SYMBOL) r#match: 1 => example; }
                prototype_calls! { get symbol(MATCH_SYMBOL) other: 0 => example; }
            },
        ] {
            diagnostic(
                builtin_global(constructor.clone(), quote! { mod members { #items } }),
                "duplicate builtin member name",
            );
        }
        diagnostic(
            builtin_global(
                quote!(name = "Example", root = "example"),
                quote! { mod members { prototype_calls! { f: 0 => example; } } },
            ),
            "methods marker requires a constructor builtin",
        );
        diagnostic(
            builtin_global(constructor, quote! { mod members { static_calls! {} } }),
            "expected at least one builtin member",
        );
    }

    #[test]
    fn builtin_global_accepts_grouped_direct_calls_with_binding() {
        let expanded = builtin_global(
            quote!(name = "Example", root = "example", kind = "constructor"),
            quote! {
                mod members {
                    static_calls! { create: 2 => create_handler; }
                    #[cfg(feature = "example")]
                    prototype_calls! {
                        then: 2 | r#match: 1 as key => |vm, host, roots, args, receiver| method(vm, host, roots, args, receiver, key);
                        other: 0 => other_handler;
                    }
                }
            },
        )
        .expect("grouped builtin calls should expand")
        .to_string();
        assert!(expanded.contains("Slot :: Prototype , \"then\") , 2u32 ,"));
        assert!(expanded.contains("Slot :: Prototype , \"match\") , 1u32 ,"));
    }

    #[test]
    fn builtin_global_requires_explicit_lengths() {
        let args = quote!(name = "Example", root = "example", kind = "constructor");
        for (items, message) in [
            (
                quote!(
                    #[function(name = "f")]
                    fn f() {}
                ),
                "missing `length` option",
            ),
            (
                quote!(
                    #[function(name = "f", length = -1)]
                    fn f() {}
                ),
                "expected a u32 length literal",
            ),
            (
                quote!(
                    #[functions(f)]
                    fn dispatch() {}
                ),
                "expected `:`",
            ),
            (
                quote!(
                    #[methods(f = Method::F)]
                    fn dispatch() {}
                ),
                "expected `:`",
            ),
            (quote!(static_calls! { f => handler; }), "expected `:`"),
            (
                quote!(prototype_calls! { get f: 1 => handler; }),
                "a builtin getter must have length 0",
            ),
        ] {
            diagnostic(
                builtin_global(args.clone(), quote!(mod members { #items })),
                message,
            );
        }
    }

    #[test]
    fn builtin_global_expands_getters_and_symbol_members() {
        let expanded = builtin_global(
            quote!(name = "String", root = "string", kind = "constructor"),
            quote! {
                mod members {
                    static_calls! {
                        match: 1 => call;
                        get symbol(crate::heap::SPECIES_SYMBOL) species: 0 => species;
                        get: 2 as key => |_, _, _, _, _| handler(key);
                    }
                    #[cfg(feature = "example")]
                    prototype_calls! {
                        r#match: 1 => call;
                        symbol(MATCH_SYMBOL) r#match: 1 => symbol_call;
                        get source: 0 | flags: 0 as key => |_, _, _, _, _| handler(key);
                    }
                }
            },
        )
        .unwrap()
        .to_string();
        for expected in [
            "Slot :: StaticSymbolGetter , \"species\"",
            "Slot :: PrototypeSymbol , \"match\"",
            "Slot :: Prototype , \"match\"",
            "Slot :: PrototypeGetter , \"source\"",
            "Slot :: PrototypeGetter , \"flags\"",
            "Slot :: Static , \"get\"",
            "install_getter (vm , constructor , crate :: value :: PropertyKey :: Symbol",
            "install_getter (vm , prototype , crate :: value :: PropertyKey :: from",
        ] {
            assert!(
                expanded.contains(expected),
                "missing {expected}: {expanded}"
            );
        }
        assert!(!expanded.contains("const OWNER"));
    }

    #[test]
    fn lowers_async_step_functions_and_methods() {
        let function = module_exports(
            quote!(),
            quote! {
                impl X {
                    #[function(name = "f")]
                    async fn f(cx: &mut ModuleCx<'_, Self>, f: JsValue) -> JsResult<HostStep> {
                        let outcome = cx.call(f, JsValue::undefined(), Vec::new()).await;
                        finish(cx, outcome)
                    }
                    async fn helper() {}
                }
            },
        )
        .unwrap()
        .to_string();
        assert!(function.contains("fn f"));
        assert!(!function.contains("async fn f"));
        assert!(function.contains("async fn helper"));
        assert!(function.contains("__linked_call"));
        assert!(function.contains("module_function"));
        assert!(function.contains("ResumeCall :: Module"));
        assert_eq!(function.matches("with_module :: < X >").count(), 2);
        assert!(!function.contains(". await"));

        let method = methods(
            quote!(C),
            quote! {
                impl C {
                    #[method(name = "f")]
                    async fn f(cx: &mut CallCx<'_>, this: This<Self>, f: JsValue) -> JsResult<HostStep> {
                        let outcome = cx.call(f, JsValue::undefined(), Vec::new()).await;
                        Self::finish(cx, this, outcome)
                    }
                }
            },
        )
        .unwrap()
        .to_string();
        assert!(method.contains("fn f"));
        assert!(!method.contains("async fn f"));
        assert!(!method.contains("Self :: finish"));
        assert!(method.contains("request_call"));
        assert!(!method.contains(". await"));
    }

    #[test]
    fn rejects_async_constructors_and_accessors() {
        let expected = "async is only supported on #[function] and #[method]";
        diagnostic(
            module_exports(
                quote!(),
                quote! {
                    impl X {
                        #[constructor(name = "C", class = C)]
                        async fn new(cx: &mut ModuleCx<'_, Self>) -> JsResult<Instance<C>> { make(cx) }
                    }
                },
            ),
            expected,
        );
        diagnostic(
            methods(
                quote!(C),
                quote! {
                    impl C {
                        #[get(name = "f")]
                        async fn f(cx: &mut CallCx<'_>, this: This<C>) -> JsResult<HostStep> {
                            finish(cx, this)
                        }
                    }
                },
            ),
            expected,
        );
        diagnostic(
            methods(
                quote!(C),
                quote! {
                    impl C {
                        #[set(name = "f")]
                        async fn f(cx: &mut CallCx<'_>, this: This<C>, value: JsValue) -> JsResult<()> {
                            set(cx, this, value)
                        }
                    }
                },
            ),
            expected,
        );
    }

    #[test]
    fn rejects_async_method_with_wrong_output() {
        diagnostic(
            methods(
                quote!(C),
                quote! {
                    impl C {
                        #[method(name = "f")]
                        async fn f(cx: &mut CallCx<'_>, this: This<C>) -> JsResult<JsValue> {
                            get(cx, this)
                        }
                    }
                },
            ),
            "async bindings must return JsResult<HostStep>",
        );
    }

    #[test]
    fn module_options_and_export_conflicts() {
        diagnostic(
            module_exports(
                quote!(),
                quote!(
                    mod x {}
                ),
            ),
            "expected `impl`",
        );
        diagnostic(
            module_exports(quote!(name = "x"), quote!(impl X {})),
            "module_exports requires an inherent impl and no options",
        );
        diagnostic(
            module_exports(quote!(), quote!(impl Trait for X {})),
            "module_exports requires an inherent impl and no options",
        );
        diagnostic(
            module_exports(
                quote!(),
                quote!(
                    impl X {
                        #[function(name = "f")]
                        const F: u32 = 1;
                    }
                ),
            ),
            "function and constructor mark functions; constant marks a const item",
        );
        diagnostic(
            methods(
                quote!(C),
                quote!(impl C {
                    #[method(name = "f")] const F: u32 = 1;
                }),
            ),
            "method markers require a function",
        );
        diagnostic(
            module_exports(
                quote!(),
                quote!(
                    impl X {
                        #[function(class = C)]
                        fn f(cx: &mut ModuleCx<'_, Self>) -> JsResult<()> {
                            todo!()
                        }
                    }
                ),
            ),
            "unknown attribute option",
        );
        diagnostic(
            module_exports(
                quote!(),
                quote!(
                    impl X {
                        #[constant(name = "f")]
                        const C: u32 = 1;
                        #[function(name = "f")]
                        fn f(cx: &mut ModuleCx<'_, Self>) -> JsResult<()> {
                            todo!()
                        }
                    }
                ),
            ),
            "duplicate module export name",
        );
    }

    #[test]
    fn typed_export_signature_errors() {
        diagnostic(
            module_exports(
                quote!(),
                quote!(
                    impl X {
                        #[constructor(name = "C", class = C)]
                        fn new(cx: &mut ModuleCx<'_, Self>) -> JsResult<Instance<D>> {
                            todo!()
                        }
                    }
                ),
            ),
            "constructor must return",
        );
        diagnostic(
            module_exports(
                quote!(),
                quote!(
                    impl X {
                        #[function(name = "f")]
                        async fn f(cx: &mut ModuleCx<'_, Self>) -> JsResult<()> {
                            todo!()
                        }
                    }
                ),
            ),
            "async bindings must return JsResult<HostStep>",
        );
        diagnostic(
            module_exports(
                quote!(),
                quote!(
                    impl X {
                        #[function(name = "f")]
                        fn f(cx: &ModuleCx<'_, Self>) -> JsResult<()> {
                            todo!()
                        }
                    }
                ),
            ),
            "first parameter must be &mut ModuleCx<'_, Self>",
        );
        diagnostic(
            module_exports(
                quote!(),
                quote!(
                    impl X {
                        #[function(name = "f")]
                        fn f<T>(cx: &mut ModuleCx<'_, Self>) -> JsResult<T> {
                            todo!()
                        }
                    }
                ),
            ),
            "cannot be generic",
        );
        diagnostic(
            module_exports(
                quote!(),
                quote!(
                    impl X {
                        #[function(name = "f")]
                        fn f(cx: &mut ModuleCx<'_, Self>, x: Option<String>) -> JsResult<()> {
                            todo!()
                        }
                    }
                ),
            ),
            "JavaScript parameters",
        );
    }

    #[test]
    fn slot_and_payload_errors() {
        for key in [quote!(encode), quote!(decode)] {
            diagnostic(
                host_class(
                    quote!(tag = "C", #key = callback),
                    quote!(
                        struct C;
                    ),
                ),
                "unknown attribute option",
            );
        }
        diagnostic(
            host_class(
                quote!(tag = "C"),
                quote!(
                    struct C {
                        payload: Option<Vec<::yield_vm::Instance<C>>>,
                    }
                ),
            ),
            "JavaScript handles belong in slots",
        );
        diagnostic(
            host_class(
                quote!(tag = "C"),
                quote!(
                    struct C(#[js_slot] JsValue);
                ),
            ),
            "require named fields",
        );
        diagnostic(
            host_class(
                quote!(tag = "C"),
                quote!(
                    struct C {
                        #[js_list]
                        queue: JsValue,
                    }
                ),
            ),
            "js_list field must be Vec<JsValue>",
        );
        diagnostic(
            host_class(
                quote!(tag = "C"),
                quote!(
                    struct C {
                        #[js_slot]
                        field: JsValue,
                        #[js_slot]
                        field: JsValue,
                    }
                ),
            ),
            "duplicate slot name",
        );
        diagnostic(
            host_class(
                quote!(tag = "C"),
                quote!(
                    struct C {
                        #[js_slot]
                        field: JsValue,
                        #[js_slot]
                        set_field: JsValue,
                    }
                ),
            ),
            "duplicate slot helper method name",
        );
    }

    #[test]
    fn method_receiver_and_accessor_errors() {
        diagnostic(
            methods(
                quote!(C),
                quote!(impl C {
                    #[method(name = "f")] fn f(&self, cx: &mut CallCx<'_>) -> JsResult<()> { todo!() }
                }),
            ),
            "Rust self receivers",
        );
        diagnostic(
            methods(
                quote!(C),
                quote!(impl C {
                    #[method(name = "f")] fn f(cx: &mut CallCx<'_>, this: This<D>) -> JsResult<()> { todo!() }
                }),
            ),
            "second parameter must be This<Class>",
        );
        diagnostic(
            methods(
                quote!(C),
                quote!(impl C {
                    #[get(name = "f")] fn f(cx: &mut CallCx<'_>, this: This<C>, arg: JsValue) -> JsResult<()> { todo!() }
                }),
            ),
            "getter must take no JavaScript parameters",
        );
        diagnostic(
            methods(
                quote!(C),
                quote!(impl C {
                    #[set(name = "f")] fn f(cx: &mut CallCx<'_>, this: This<C>, arg: Option<JsValue>) -> JsResult<()> { todo!() }
                }),
            ),
            "setter must take one required JsValue",
        );
        diagnostic(
            methods(
                quote!(C),
                quote!(impl C {
                    #[get(symbol = ToPrimitive)] fn g(cx: &mut CallCx<'_>, this: This<C>) -> JsResult<()> { todo!() }
                    #[get(symbol = ToPrimitive)] fn h(cx: &mut CallCx<'_>, this: This<C>) -> JsResult<()> { todo!() }
                }),
            ),
            "duplicate getter",
        );
        diagnostic(
            methods(
                quote!(C),
                quote!(impl C {
                    #[get(name = "f")] fn g(cx: &mut CallCx<'_>, this: This<C>) -> JsResult<()> { todo!() }
                    #[method(name = "f")] fn f(cx: &mut CallCx<'_>, this: This<C>) -> JsResult<()> { todo!() }
                }),
            ),
            "conflicting member key",
        );
        diagnostic(
            methods(
                quote!(C),
                quote!(impl C {
                    #[method(symbol = IsConcatSpreadable)] fn f(cx: &mut CallCx<'_>, this: This<C>) -> JsResult<()> { todo!() }
                }),
            ),
            "unknown WellKnownSymbol variant",
        );
    }

    #[test]
    fn extracts_only_slots_in_source_order() {
        let mut item = syn::parse_quote! {
            struct C {
                #[js_list] queue: Vec<JsValue>,
                payload: u32,
                #[js_slot] value: JsValue,
            }
        };
        let slots = extract_slots(&mut item).unwrap();
        assert_eq!(
            slots
                .iter()
                .map(|slot| (slot.name.to_string(), slot.list))
                .collect::<Vec<_>>(),
            [("queue".to_owned(), true), ("value".to_owned(), false)]
        );
        let Fields::Named(fields) = item.fields else {
            panic!("expected payload fields")
        };
        assert_eq!(fields.named.len(), 1);
        let field = &fields.named[0];
        assert_eq!(field.ident.as_ref().unwrap(), "payload");
        assert!(simple(&field.ty, "u32"));
    }
}
