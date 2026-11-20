use std::collections::{HashMap, HashSet};

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{
    Attribute, Error, Expr, Fields, FnArg, GenericArgument, Ident, ImplItem, Item, ItemImpl,
    ItemMod, ItemStruct, Lit, LitInt, LitStr, Meta, Path, PathArguments, Result, ReturnType,
    Signature, Token, Type, parse::Parser, punctuated::Punctuated, spanned::Spanned,
};

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

fn schema(meta: &Meta) -> Result<LitInt> {
    if let Expr::Lit(expr) = value(meta)?
        && let Lit::Int(lit) = &expr.lit
    {
        lit.base10_parse::<u32>()?;
        return Ok(lit.clone());
    }
    Err(Error::new_spanned(meta, "expected a u32 schema literal"))
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
}

struct Call {
    optional: Vec<bool>,
    output: Output,
}

impl Call {
    fn parse(sig: &Signature, receiver: Option<&Type>) -> Result<Self> {
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
        let Some(cx) = inputs.next().transpose()? else {
            return Err(Error::new_spanned(
                sig,
                "first parameter must be &mut CallCx<'_>",
            ));
        };
        let valid_cx = match cx {
            Type::Reference(reference) if reference.mutability.is_some() && reference.lifetime.is_none() => {
                last(&reference.elem).is_some_and(|segment| {
                    segment.ident == "CallCx" && matches!(&segment.arguments,
                        PathArguments::AngleBracketed(args) if args.args.len() == 1
                        && matches!(args.args.first(), Some(GenericArgument::Lifetime(lt)) if lt.ident == "_"))
                })
            }
            _ => false,
        };
        if !valid_cx {
            return Err(Error::new_spanned(
                cx,
                "first parameter must be &mut CallCx<'_>",
            ));
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
                    "expected JsResult<JsValue>, JsResult<()> or JsResult<Instance<Class>>",
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
        };
        quote! {
            ::yield_vm::__macro_support::CallSignature {
                args: &[#(::yield_vm::__macro_support::ArgKind::#args),*],
                result: ::yield_vm::__macro_support::ReturnKind::#result,
            }
        }
    }

    fn adapter(&self, target: TokenStream, receiver: Option<&Type>) -> TokenStream {
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
            Output::Value => call,
            Output::Void => quote!({ #call?; Ok(::yield_vm::JsValue::undefined()) }),
            Output::Instance(_) => quote!({ let instance = #call?; Ok(instance.value(cx)) }),
        };
        quote! {
            fn __yield_call(cx: &mut ::yield_vm::binding::CallCx<'_>) -> ::yield_vm::JsResult<::yield_vm::JsValue> {
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

const EXPORT_MARKERS: &[&str] = &["function", "constructor", "constant"];
const MEMBER_MARKERS: &[&str] = &["method", "get", "set"];

pub(super) fn host_module(args: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let mut module: ItemMod = syn::parse2(item)?;
    let opts = options(args, &["name"])?;
    let module_name = string(required(&opts, "name", module.ident.span())?)?;
    let Some((_, items)) = &mut module.content else {
        return Err(Error::new_spanned(
            module,
            "host_module requires an inline module",
        ));
    };
    let mut exports = Vec::new();
    let mut keys = HashSet::new();
    for item in items {
        let (attrs, ident, sig) = match item {
            Item::Fn(item) => (&mut item.attrs, &item.sig.ident, Some(&item.sig)),
            Item::Const(item) => (&mut item.attrs, &item.ident, None),
            other => {
                let attrs = match other {
                    Item::Enum(item) => &item.attrs,
                    Item::ExternCrate(item) => &item.attrs,
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
                    _ => continue,
                };
                reject_markers(
                    attrs,
                    EXPORT_MARKERS,
                    "export markers require a function or const item",
                )?;
                continue;
            }
        };
        if sig.is_some() && ident == "__yield_module" {
            return Err(Error::new_spanned(
                ident,
                "reserved module registration function name",
            ));
        }
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
        let export = match sig {
            Some(sig) if kind != "constant" => {
                let call = Call::parse(sig, None)?;
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
                let adapter = call.adapter(quote!(self::#ident), None);
                let signature = call.signature();
                quote!({
                    #adapter
                    ::yield_vm::__macro_support::function(#name, #constructable, #class, __yield_call, #signature)
                })
            }
            None if kind == "constant" => {
                quote!(::yield_vm::__macro_support::constant(#name, self::#ident))
            }
            _ => {
                return Err(Error::new_spanned(
                    attr,
                    "function and constructor mark functions; constant marks a const item",
                ));
            }
        };
        let cfg = attrs.iter().filter(|attr| attr.path().is_ident("cfg"));
        exports.push(quote!(#(#cfg)* #export));
    }
    let (_, items) = module.content.as_mut().expect("inline module");
    items.push(syn::parse_quote! {
        #[doc(hidden)]
        pub fn __yield_module() -> ::yield_vm::__macro_support::ModuleDecl {
            ::yield_vm::__macro_support::module(#module_name, ::std::vec![#(#exports),*])
        }
    });
    Ok(quote!(#module))
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
        &["tag", "methods", "parent", "schema", "encode", "decode"],
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
    let payload = match (opts.get("schema"), opts.get("encode"), opts.get("decode")) {
        (None, None, None) => None,
        (Some(schema_opt), Some(encode), Some(decode)) => {
            let schema = schema(schema_opt)?;
            let encode = path(encode)?;
            let decode = path(decode)?;
            Some(quote! {
                const PAYLOAD_SCHEMA: Option<u32> = Some(#schema);

                fn encode_payload(&self, out: &mut ::std::vec::Vec<u8>) -> ::core::result::Result<(), ::yield_vm::binding::CodecError> {
                    let codec: fn(&Self, &mut ::std::vec::Vec<u8>) -> ::core::result::Result<(), ::yield_vm::binding::CodecError> = #encode;
                    codec(self, out)
                }

                fn decode_payload(input: &[u8], cx: &mut ::yield_vm::binding::DecodeCx<'_>) -> ::core::result::Result<Self, ::yield_vm::binding::CodecError> {
                    let codec: fn(&[u8], &mut ::yield_vm::binding::DecodeCx<'_>) -> ::core::result::Result<Self, ::yield_vm::binding::CodecError> = #decode;
                    codec(input, cx)
                }
            })
        }
        _ => {
            return Err(Error::new(
                span,
                "`schema`, `encode`, and `decode` must be specified together",
            ));
        }
    };
    let slots = extract_slots(&mut item)?;
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
            #tables
            #payload
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
                    "export markers require a host_module",
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
            "export markers require a host_module",
        )?;
        let Some((kind, attr)) = take_marker(&mut method.attrs, MEMBER_MARKERS)? else {
            continue;
        };
        let opts = options(attr.parse_args()?, &["name", "symbol"])?;
        let (id, key) = member_key(&opts, attr.span())?;
        let call = Call::parse(&method.sig, Some(&class))?;
        if kind == "get" && !call.optional.is_empty() {
            return Err(Error::new_spanned(
                &method.sig,
                "getter must take no JavaScript parameters",
            ));
        }
        if kind == "set" && (call.optional != [false] || !matches!(call.output, Output::Void)) {
            return Err(Error::new_spanned(
                &method.sig,
                "setter must take one required JsValue and return JsResult<()>",
            ));
        }
        let name = &method.sig.ident;
        let adapter = call.adapter(quote!(<#class>::#name), Some(&class));
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
            methods.push(quote! {
                #(#cfg)* {
                    #adapter
                    ::yield_vm::__macro_support::method(#key, __yield_call, #signature)
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
    fn module_options_and_export_conflicts() {
        diagnostic(
            host_module(
                quote!(name = "x"),
                quote!(
                    mod x;
                ),
            ),
            "inline module",
        );
        diagnostic(
            host_module(
                quote!(name = "x", nmae = "y"),
                quote!(
                    mod x {}
                ),
            ),
            "unknown attribute option",
        );
        diagnostic(
            host_module(
                quote!(name = "x", name = "y"),
                quote!(
                    mod x {}
                ),
            ),
            "duplicate attribute option",
        );
        diagnostic(
            host_module(
                quote!(name = "x"),
                quote!(
                    mod x {
                        #[function(name = "f")]
                        struct F;
                    }
                ),
            ),
            "export markers require a function or const item",
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
            host_module(
                quote!(name = "x"),
                quote!(
                    mod x {
                        #[function(class = C)]
                        fn f(cx: &mut CallCx<'_>) -> JsResult<()> {
                            todo!()
                        }
                    }
                ),
            ),
            "unknown attribute option",
        );
        diagnostic(
            host_module(
                quote!(name = "x"),
                quote!(
                    mod x {
                        #[constant(name = "f")]
                        const C: u32 = 1;
                        #[function(name = "f")]
                        fn f(cx: &mut CallCx<'_>) -> JsResult<()> {
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
            host_module(
                quote!(name = "x"),
                quote!(
                    mod x {
                        #[constructor(name = "C", class = C)]
                        fn new(cx: &mut CallCx<'_>) -> JsResult<Instance<D>> {
                            todo!()
                        }
                    }
                ),
            ),
            "constructor must return",
        );
        diagnostic(
            host_module(
                quote!(name = "x"),
                quote!(
                    mod x {
                        #[function(name = "f")]
                        async fn f(cx: &mut CallCx<'_>) -> JsResult<()> {
                            todo!()
                        }
                    }
                ),
            ),
            "synchronous safe Rust function",
        );
        diagnostic(
            host_module(
                quote!(name = "x"),
                quote!(
                    mod x {
                        #[function(name = "f")]
                        fn f(cx: &CallCx<'_>) -> JsResult<()> {
                            todo!()
                        }
                    }
                ),
            ),
            "first parameter must be &mut CallCx",
        );
        diagnostic(
            host_module(
                quote!(name = "x"),
                quote!(
                    mod x {
                        #[function(name = "f")]
                        fn f<T>(cx: &mut CallCx<'_>) -> JsResult<T> {
                            todo!()
                        }
                    }
                ),
            ),
            "cannot be generic",
        );
        diagnostic(
            host_module(
                quote!(name = "x"),
                quote!(
                    mod x {
                        #[function(name = "f")]
                        fn f(cx: &mut CallCx<'_>, x: Option<String>) -> JsResult<()> {
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
        diagnostic(
            host_class(
                quote!(tag = "C", schema = 1),
                quote!(
                    struct C;
                ),
            ),
            "must be specified together",
        );
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
