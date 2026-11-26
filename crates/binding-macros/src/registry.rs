use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path as FsPath, PathBuf};

use proc_macro2::{Span as MacroSpan, TokenStream};
use quote::quote;
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    Error, Expr, Ident, ItemStruct, LitStr, Path, Result, Token, Type, TypePath, Visibility,
    braced, bracketed,
};

use super::typed::{extract_slots, slot_decls};

fn entries(
    input: ParseStream<'_>,
    mut entry: impl FnMut(Ident, ParseStream<'_>) -> Result<()>,
) -> Result<()> {
    while !input.is_empty() {
        entry(input.call(Ident::parse_any)?, input)?;
        if input.is_empty() {
            break;
        }
        input.parse::<Token![,]>()?;
    }
    Ok(())
}

pub(super) fn set<T>(
    slot: &mut Option<T>,
    key: &Ident,
    parse: impl FnOnce() -> Result<T>,
) -> Result<()> {
    if slot.is_some() {
        return Err(Error::new(key.span(), format!("duplicate `{key}`")));
    }
    *slot = Some(parse()?);
    Ok(())
}

fn required<T>(slot: Option<T>, span: proc_macro2::Span, key: &str) -> Result<T> {
    slot.ok_or_else(|| Error::new(span, format!("missing `{key}`")))
}

fn attribute_value<T: Parse>(input: ParseStream<'_>) -> Result<T> {
    input.parse::<Token![=]>()?;
    input.parse()
}

fn check_struct(item: &ItemStruct) -> Result<()> {
    if !item.generics.params.is_empty() || item.generics.where_clause.is_some() {
        return Err(Error::new(
            item.generics.span(),
            "generic structs are not supported",
        ));
    }
    Ok(())
}

pub(super) fn operation(args: TokenStream, item: TokenStream) -> Result<TokenStream> {
    use syn::parse::Parser;

    let item: ItemStruct = syn::parse2(item)?;
    check_struct(&item)?;
    let ty = &item.ident;
    let span = ty.span();
    let (mut name, mut recovery, mut rejection, mut services) = (None, None, None, None);
    (|input: ParseStream<'_>| {
        entries(input, |key, input| match key.to_string().as_str() {
            "name" => set(&mut name, &key, || attribute_value::<LitStr>(input)),
            "recovery" => set(&mut recovery, &key, || {
                let value: Ident = attribute_value(input)?;
                let variant = match value.to_string().as_str() {
                    "pause" => "Pause",
                    "resume" => "Resume",
                    "reissue" => "Reissue",
                    "reject" => "Reject",
                    _ => {
                        return Err(Error::new(
                            value.span(),
                            "expected `pause`, `resume`, `reissue`, or `reject`",
                        ));
                    }
                };
                Ok(Ident::new(variant, value.span()))
            }),
            "rejection" => set(&mut rejection, &key, || attribute_value::<LitStr>(input)),
            "services" => set(&mut services, &key, || service_fields(input)),
            _ => Err(Error::new(key.span(), "unknown operation option")),
        })
    })
    .parse2(args)?;
    let name = required(name, span, "name")?;
    let recovery = required(recovery, span, "recovery")?;
    let rejection = rejection.map(|value| quote!(const REJECTION: &'static str = #value;));
    let services = service_owner(ty, &item.vis, services.unwrap_or_default());

    Ok(quote! {
        #item
        #services
        impl ::yield_vm::Operation for #ty {
            const NAME: &'static str = #name;
            const RECOVERY: ::yield_vm::Recovery = ::yield_vm::Recovery::#recovery;
            #rejection
        }
    })
}

pub(super) fn host_module(args: TokenStream, item: TokenStream) -> Result<TokenStream> {
    use syn::parse::Parser;

    let mut item: ItemStruct = syn::parse2(item)?;
    check_struct(&item)?;
    let span = item.ident.span();
    let mut module = None;
    let mut classes = None;
    let mut operations = None;
    let mut services = None;
    (|input: ParseStream<'_>| {
        entries(input, |key, input| match key.to_string().as_str() {
            "name" => set(&mut module, &key, || attribute_value::<LitStr>(input)),
            "classes" => set(&mut classes, &key, || {
                input.parse::<Token![=]>()?;
                list::<TypePath>(input)
            }),
            "operations" => set(&mut operations, &key, || {
                input.parse::<Token![=]>()?;
                list::<TypePath>(input)
            }),
            "services" => set(&mut services, &key, || service_fields(input)),
            _ => Err(Error::new(key.span(), "unknown host_module option")),
        })
    })
    .parse2(args)?;
    let module = required(module, span, "name")?;
    let classes = classes.unwrap_or_default();
    let operations = operations.unwrap_or_default();
    let abi = quote!(::yield_vm::__macro_support);
    let layout = Ident::new("__YieldLayout", MacroSpan::mixed_site());
    let indices = |prefix: &str, len| -> Vec<_> {
        (0..len)
            .map(|i| Ident::new(&format!("{prefix}{i}"), MacroSpan::mixed_site()))
            .collect()
    };
    let ic = indices("__YieldClassIndex", classes.len());
    let io = indices("__YieldOperationIndex", operations.len());
    let slots = extract_slots(&mut item)?;
    let empty = match &item.fields {
        syn::Fields::Unit => Some(quote!(Self)),
        syn::Fields::Named(fields) if fields.named.is_empty() => Some(quote!(Self {})),
        syn::Fields::Unnamed(fields) if fields.unnamed.is_empty() => Some(quote!(Self())),
        _ => None,
    };
    let decls = slot_decls(&slots);
    let ty = &item.ident;
    let services = service_owner(ty, &item.vis, services.unwrap_or_default());
    let registration = quote! {
        // SAFETY: every dependency view is built from the supplied layout.
        unsafe impl<#layout #(, #ic)* #(, #io)*> #abi::RegisterConsumers<#layout, ((#(#ic,)*), (#(#io,)*))> for #ty
        where
            #(#classes: #abi::BuildView<#layout, #ic>,)*
            #(#operations: #abi::BuildView<#layout, #io>,)*
        {
            fn register_consumers(layout: &#layout, deps: &mut #abi::ModuleDependencies) {
                #(deps.class::<#classes, #layout, #ic>(layout);)*
                #(deps.operation::<#operations, #layout, #io>(layout);)*
            }
        }
    };
    let mut names = std::collections::HashSet::new();
    let mut constants = Vec::new();
    for (index, slot) in slots.iter().enumerate() {
        let name = format!("SLOT_{}", slot.name.unraw().to_string().to_uppercase());
        if !names.insert(name.clone()) {
            return Err(Error::new(
                slot.name.span(),
                format!("duplicate slot constant `{name}`"),
            ));
        }
        let mut ident = syn::parse_str::<Ident>(&name).map_err(|_| {
            Error::new(
                slot.name.span(),
                "slot name cannot form a constant identifier",
            )
        })?;
        ident.set_span(slot.name.span());
        constants.push(quote!(pub const #ident: usize = #index;));
    }
    let constants = (!constants.is_empty()).then(|| quote!(impl #ty { #(#constants)* }));
    let (mut default, mut encode, mut decode) = (None, None, None);
    if let Some(empty) = empty {
        let owner = if ty == "__YieldOwner" {
            Ident::new("__YieldOwner_", MacroSpan::mixed_site())
        } else {
            Ident::new("__YieldOwner", MacroSpan::mixed_site())
        };
        let mut derives = BTreeSet::new();
        for attr in &item.attrs {
            if attr.path().is_ident("derive") {
                let paths = attr.parse_args_with(
                    syn::punctuated::Punctuated::<Path, Token![,]>::parse_terminated,
                )?;
                derives.extend(paths.iter().filter_map(|path| {
                    path.segments
                        .last()
                        .map(|segment| segment.ident.to_string())
                }));
            }
        }
        default = (!derives.contains("Default")).then(|| quote!(#[derive(Default)]));
        encode = (!derives.contains("Encode")).then(|| quote! {
            impl<#owner> ::yield_vm::Encode<#owner> for #ty {
                fn encode(&self, _: &mut ::std::vec::Vec<u8>, _: &::yield_vm::Services<'_, #owner>) -> ::core::result::Result<(), ::yield_vm::binding::CodecError> {
                    Ok(())
                }
            }
        });
        decode = (!derives.contains("Decode")).then(|| quote! {
            impl<#owner> ::yield_vm::Decode<#owner> for #ty {
                fn decode(_: &mut &[u8], _: &::yield_vm::Services<'_, #owner>) -> ::core::result::Result<Self, ::yield_vm::binding::CodecError> {
                    Ok(#empty)
                }
            }
        });
    }

    Ok(quote! {
        #default
        #item
        #services
        #constants
        impl ::yield_vm::HostModule for #ty {
            const NAME: &'static str = #module;
            const SLOTS: &'static [::yield_vm::SlotDecl] = #decls;
        }
        #registration
        #encode
        #decode
    })
}

fn list<T: Parse>(input: ParseStream<'_>) -> Result<Vec<T>> {
    let contents;
    bracketed!(contents in input);
    Ok(contents
        .parse_terminated(T::parse, Token![,])?
        .into_iter()
        .collect())
}

pub(super) fn service_fields(input: ParseStream<'_>) -> Result<Vec<ServiceField>> {
    input.parse::<Token![=]>()?;
    let fields: Vec<ServiceField> = list(input)?;
    let mut names = BTreeSet::new();
    for field in &fields {
        if !names.insert(field.name.unraw().to_string()) {
            return Err(Error::new(field.name.span(), "duplicate service accessor"));
        }
    }
    Ok(fields)
}

pub(super) struct ServiceField {
    name: Ident,
    ty: Type,
}

impl Parse for ServiceField {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let name = input.parse()?;
        input.parse::<Token![:]>()?;
        Ok(Self {
            name,
            ty: input.parse()?,
        })
    }
}

pub(super) fn service_owner(
    ty: &Ident,
    vis: &Visibility,
    fields: Vec<ServiceField>,
) -> TokenStream {
    use syn::fold::{self, Fold};

    // Preserve `Self` from the accessor's impl when using the type outside it.
    struct OwnerType<'a>(&'a Ident);
    impl Fold for OwnerType<'_> {
        fn fold_path(&mut self, mut path: Path) -> Path {
            if path.leading_colon.is_none()
                && let Some(segment) = path.segments.first_mut()
                && segment.ident == "Self"
            {
                segment.ident = self.0.clone();
            }
            fold::fold_path(self, path)
        }
    }

    let abi = quote!(::yield_vm::__macro_support);
    let layout = Ident::new("__YieldLayout", MacroSpan::mixed_site());
    let layout_value = Ident::new("__yield_layout", MacroSpan::mixed_site());
    let indices: Vec<_> = (0..fields.len())
        .map(|index| Ident::new(&format!("__YieldIndex{index}"), MacroSpan::mixed_site()))
        .collect();
    let lifetime = syn::Lifetime::new("'__yield_service", MacroSpan::mixed_site());
    let names: Vec<_> = fields.iter().map(|field| &field.name).collect();
    let types: Vec<_> = fields
        .iter()
        .map(|field| OwnerType(ty).fold_type(field.ty.clone()))
        .collect();
    let (view, declaration, build, bounds) = if fields.is_empty() {
        (quote!(()), quote!(), quote!(), quote!())
    } else {
        let view = Ident::new(
            &format!("__YieldServiceView_{}", ty.unraw()),
            MacroSpan::mixed_site(),
        );
        (
            quote!(#view),
            quote! {
                #[doc(hidden)]
                #[allow(non_camel_case_types)]
                #vis struct #view {
                    #(#names: ::core::ptr::NonNull<#types>),*
                }
            },
            quote!(#view {
                #(#names: ::core::ptr::NonNull::from(<#layout as #abi::Provide<#types, #indices>>::provide(#layout_value))),*
            }),
            quote!(where #(#layout: #abi::Provide<#types, #indices>),*),
        )
    };
    let accessors = fields.iter().map(|ServiceField { name, ty }| {
        quote! {
            #vis fn #name<#lifetime>(services: &::yield_vm::Services<#lifetime, Self>) -> &#lifetime #ty {
                // SAFETY: the binding owns this view and its layout for the borrow.
                unsafe { services.__view().#name.as_ref() }
            }
        }
    });
    let accessors = (!fields.is_empty()).then(|| quote!(impl #ty { #(#accessors)* }));
    quote! {
        #declaration
        // SAFETY: accessors only project shared fields of this owner's view.
        unsafe impl #abi::ServiceOwner for #ty { type View = #view; }
        // SAFETY: pointers are shared projections from the retained stable layout.
        unsafe impl<#layout #(, #indices)*> #abi::BuildView<#layout, (#(#indices,)*)> for #ty
        #bounds
        {
            fn build_view(#layout_value: &#layout) -> Self::View { #build }
        }
        #accessors
    }
}

const PLATFORM_SERVICES: [&str; 9] = [
    "console",
    "crypto",
    "diagnostics",
    "fetch",
    "gc_roots",
    "subtle_crypto",
    "time",
    "timer",
    "user_agent",
];

enum ServiceBinding {
    Platform { name: Ident, value: Expr },
    Custom { ty: Box<Type>, value: Expr },
}

impl ServiceBinding {
    fn parse(input: ParseStream<'_>, seen: &mut BTreeSet<String>) -> Result<Self> {
        let ahead = input.fork();
        if let Ok(name) = ahead.call(Ident::parse_any)
            && ahead.peek(Token![:])
            && !ahead.peek(Token![::])
        {
            let key = name.unraw().to_string();
            if !PLATFORM_SERVICES.contains(&key.as_str()) {
                return Err(Error::new(
                    name.span(),
                    format!("unknown platform service: {key}"),
                ));
            }
            if !seen.insert(key.clone()) {
                return Err(Error::new(
                    name.span(),
                    format!("duplicate platform service: {key}"),
                ));
            }
            let name = input.call(Ident::parse_any)?;
            input.parse::<Token![:]>()?;
            return Ok(Self::Platform {
                name,
                value: input.parse()?,
            });
        }
        let ty = Box::new(input.parse()?);
        input.parse::<Token![=>]>()?;
        Ok(Self::Custom {
            ty,
            value: input.parse()?,
        })
    }
}

fn service_bindings(input: ParseStream<'_>) -> Result<Vec<ServiceBinding>> {
    let contents;
    braced!(contents in input);
    let mut seen = BTreeSet::new();
    let mut bindings = Vec::new();
    while !contents.is_empty() {
        bindings.push(ServiceBinding::parse(&contents, &mut seen)?);
        if !contents.is_empty() {
            contents.parse::<Token![,]>()?;
        }
    }
    Ok(bindings)
}

struct Registry {
    dts: Option<LitStr>,
    modules: Vec<TypePath>,
    services: Vec<ServiceBinding>,
}

impl Parse for Registry {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let span = input.span();
        let (mut dts, mut modules, mut services) = (None, None, None);
        entries(input, |key, input| match key.to_string().as_str() {
            "dts" => set(&mut dts, &key, || {
                input.parse::<Token![:]>()?;
                input.parse()
            }),
            "modules" => set(&mut modules, &key, || {
                input.parse::<Token![:]>()?;
                list(input)
            }),
            "services" => set(&mut services, &key, || {
                input.parse::<Token![:]>()?;
                service_bindings(input)
            }),
            _ => Err(Error::new(key.span(), "unknown host section")),
        })?;
        let modules = modules.unwrap_or_default();
        if !modules.is_empty() && dts.is_none() {
            return Err(Error::new(span, "missing `dts`"));
        }
        Ok(Self {
            dts,
            modules,
            services: required(services, span, "services")?,
        })
    }
}

pub(super) fn host(input: TokenStream, origin: Option<PathBuf>) -> Result<TokenStream> {
    let Registry {
        dts,
        modules,
        services,
    } = syn::parse2(input)?;
    let declaration_validation = if let Some(dts) = dts {
        let (path, declarations) = read_declarations(&dts, origin.as_deref())?;
        let include = LitStr::new(&path.to_string_lossy(), dts.span());
        let checks = declaration_checks(&declarations, &modules);
        quote! {
            const _: &str = include_str!(#include);
            #(#checks)*
        }
    } else {
        TokenStream::new()
    };
    let mut initializers = Vec::new();
    let mut custom_values = Vec::new();
    let mut custom_types = Vec::new();
    let mut platform_values = BTreeMap::new();
    for (index, binding) in services.into_iter().enumerate() {
        let local = Ident::new(&format!("__yield_service{index}"), MacroSpan::mixed_site());
        match binding {
            ServiceBinding::Platform { name, value } => {
                initializers.push(quote!(let #local = #value;));
                platform_values.insert(name.unraw().to_string(), local);
            }
            ServiceBinding::Custom { ty, value } => {
                // Coerce at the user type before inferring the layout's parameters.
                initializers.push(quote!(let #local: #ty = #value;));
                custom_values.push(local);
                custom_types.push(ty);
            }
        }
    }
    let platform_fields = PLATFORM_SERVICES.into_iter().map(|name| {
        let field = Ident::new(name, MacroSpan::mixed_site());
        let value = match platform_values.get(name) {
            Some(local) => quote!(::core::option::Option::Some(#local)),
            None if name == "user_agent" => quote!(::core::option::Option::None),
            None => quote!(::core::option::Option::None::<::yield_vm::host::Absent>),
        };
        quote!(#field: #value)
    });
    let abi = quote!(::yield_vm::__macro_support);
    let mutable = (!modules.is_empty()).then(|| quote!(mut));
    let deps = Ident::new("__yield_deps", MacroSpan::mixed_site());
    let modules_var = Ident::new("__yield_modules", MacroSpan::mixed_site());
    let storage = Ident::new("__yield_storage", MacroSpan::mixed_site());
    let borrowed = Ident::new("__yield_borrowed", MacroSpan::mixed_site());
    let register = quote! {
        let #mutable #deps = #abi::ModuleDependencies::default();
        let #modules_var = [#(<#modules as #abi::RegisterFor<_, _>>::register_for(#borrowed, &mut #deps)),*];
        (#modules_var, #deps)
    };
    let assembly = if custom_values.is_empty() {
        quote! {{
            let #borrowed = &();
            (#abi::ServiceStorage::default(), { #register })
        }}
    } else {
        let generated = |prefix: &str| -> Vec<Ident> {
            (0..custom_values.len())
                .map(|index| Ident::new(&format!("{prefix}{index}"), MacroSpan::mixed_site()))
                .collect()
        };
        let fields = generated("__s");
        let params = generated("__T");
        let layout = Ident::new("__YieldServiceLayout", MacroSpan::mixed_site());
        let providers = fields
            .iter()
            .zip(&params)
            .enumerate()
            .map(|(index, (field, ty))| {
                quote! {
                    impl<#(#params),*> ::yield_vm::__macro_support::Provide<
                        #ty, ::yield_vm::__macro_support::Slot<#index>
                    > for #layout<#(#params),*> {
                        fn provide(&self) -> &#ty { &self.#field }
                    }
                }
            });
        let instance = Ident::new("__yield_layout", MacroSpan::mixed_site());
        quote! {{
            struct #layout<#(#params),*> {
                #(#fields: #params),*
            }
            #(#providers)*
            impl<#(#params: 'static),*> #layout<#(#params),*> {
                fn with_views<R>(self, build: impl FnOnce(&Self) -> R) -> (#abi::ServiceStorage, R) {
                    // SAFETY: descriptors use the exact types and offsets of this layout.
                    unsafe {
                        #abi::ServiceStorage::with_layout(::std::boxed::Box::new(self), [
                            #(#abi::ServiceField::of::<#params>(::core::mem::offset_of!(Self, #fields))),*
                        ], build)
                    }
                }
            }
            let #instance = #layout { #(#fields: #custom_values),* };
            #(::yield_vm::__macro_support::assert_unique::<#custom_types, _, _>(&#instance);)*
            #instance.with_views(|#borrowed| { #register })
        }}
    };
    Ok(quote! {
        {
            #declaration_validation
            #(#initializers)*
            let (#storage, (#modules_var, #deps)) = #assembly;
            let __yield_platform = ::yield_vm::host::Platform { #(#platform_fields),* };
            // SAFETY: every view is paired with its owner and the stable layout
            // used to build it (the empty layout is a promoted unit value).
            unsafe { #abi::link(#modules_var, #deps, #storage, __yield_platform) }
        }
    })
}

fn resolve_path(literal: &LitStr, origin: Option<&FsPath>) -> PathBuf {
    let value = literal.value();
    let declared = FsPath::new(&value);
    if declared.is_absolute() {
        return declared.to_path_buf();
    }
    let base = origin
        .and_then(FsPath::parent)
        .map(FsPath::to_path_buf)
        .or_else(|| std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    let resolved = base.join(declared);
    // `local_file` can be relative to the compiler's working directory, and
    // `include_str!` would re-resolve such a path against the invoking file.
    std::path::absolute(&resolved).unwrap_or(resolved)
}

fn read_declarations(
    literal: &LitStr,
    origin: Option<&FsPath>,
) -> Result<(PathBuf, BTreeMap<String, BTreeSet<String>>)> {
    let path = resolve_path(literal, origin);
    let source = std::fs::read_to_string(&path)
        .map_err(|error| Error::new(literal.span(), format!("{}: {error}", path.display())))?;
    let declarations = yield_host_dts::parse(&source).map_err(|error| {
        let (line, column) = line_column(&source, error.start);
        Error::new(
            literal.span(),
            format!("{}:{line}:{column}: {}", path.display(), error.message),
        )
    })?;
    Ok((path, declarations))
}

fn line_column(source: &str, offset: u32) -> (usize, usize) {
    let prefix = &source[..(offset as usize).min(source.len())];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let start = prefix.rfind('\n').map_or(0, |index| index + 1);
    (line, prefix[start..].chars().count() + 1)
}

fn declaration_checks(
    declarations: &BTreeMap<String, BTreeSet<String>>,
    modules: &[TypePath],
) -> Vec<TokenStream> {
    let mut checks = Vec::new();
    for (module, names) in declarations {
        for name in names {
            let module = LitStr::new(module, MacroSpan::call_site());
            let name = LitStr::new(name, MacroSpan::call_site());
            let message = format!("missing host export: {}:{}", module.value(), name.value());
            checks.push(quote! {
                const _: () = {
                    const fn __yield_str_eq(left: &str, right: &str) -> bool {
                        let left = left.as_bytes();
                        let right = right.as_bytes();
                        if left.len() != right.len() {
                            return false;
                        }
                        let mut index = 0;
                        while index < left.len() {
                            if left[index] != right[index] {
                                return false;
                            }
                            index += 1;
                        }
                        true
                    }
                    let modules: &[(&str, &[&str])] = &[
                        #((<#modules as ::yield_vm::HostModule>::NAME,
                           <#modules as ::yield_vm::ModuleExports>::EXPORT_NAMES)),*
                    ];
                    let mut module_index = 0;
                    let mut found = false;
                    while module_index < modules.len() {
                        if __yield_str_eq(modules[module_index].0, #module) {
                            let names = modules[module_index].1;
                            let mut name_index = 0;
                            while name_index < names.len() {
                                if __yield_str_eq(names[name_index], #name) {
                                    found = true;
                                }
                                name_index += 1;
                            }
                        }
                        module_index += 1;
                    }
                    if !found {
                        panic!(#message);
                    }
                };
            });
        }
    }
    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(input: TokenStream) -> Result<TokenStream> {
        super::host(input, None)
    }

    fn diagnostic(result: Result<TokenStream>, expected: &str) {
        assert_eq!(result.unwrap_err().to_string(), expected);
    }

    #[test]
    fn operation_reports_missing_invalid_and_duplicate_recovery() {
        let item = quote!(
            struct Wait;
        );
        diagnostic(
            operation(quote!(name = "wait"), item.clone()),
            "missing `recovery`",
        );
        diagnostic(
            operation(quote!(recovery = retry), item.clone()),
            "expected `pause`, `resume`, `reissue`, or `reject`",
        );
        diagnostic(
            operation(quote!(recovery = pause, recovery = resume), item),
            "duplicate `recovery`",
        );
    }

    #[test]
    fn operation_rejects_legacy_codec_options() {
        for key in [quote!(encode), quote!(decode)] {
            diagnostic(
                operation(
                    quote!(name = "wait", recovery = pause, #key = callback),
                    quote!(
                        struct Wait;
                    ),
                ),
                "unknown operation option",
            );
        }
    }

    #[test]
    fn host_module_rejects_legacy_state_options() {
        for key in [quote!(init), quote!(encode), quote!(decode)] {
            diagnostic(
                host_module(
                    quote!(name = "counter", #key = callback),
                    quote!(
                        struct Counter;
                    ),
                ),
                "unknown host_module option",
            );
        }
    }

    #[test]
    fn host_module_reports_slot_constant_collisions() {
        diagnostic(
            host_module(
                quote!(name = "counter"),
                quote!(
                    struct Counter {
                        #[js_slot]
                        count: JsValue,
                        #[js_list]
                        COUNT: Vec<JsValue>,
                    }
                ),
            ),
            "duplicate slot constant `SLOT_COUNT`",
        );
    }

    #[test]
    fn host_module_accepts_optional_dependency_lists() {
        let item = quote!(
            struct Counter;
        );
        for args in [
            quote!(name = "counter"),
            quote!(name = "counter", classes = [], operations = [],),
            quote!(
                operations = [work::Wait,],
                name = "counter",
                classes = [crate::Point,],
                services = [timers: crate::Timers],
            ),
        ] {
            host_module(args, item.clone()).unwrap();
        }
    }

    #[test]
    fn host_module_rejects_legacy_service_lists() {
        assert!(
            host_module(
                quote!(name = "counter", services = [Clock]),
                quote!(
                    struct Counter;
                ),
            )
            .is_err()
        );
    }

    #[test]
    fn host_module_rejects_duplicate_dependency_keys() {
        let item = quote!(
            struct Counter;
        );
        for key in ["classes", "operations", "services"] {
            let ident = Ident::new(key, MacroSpan::call_site());
            let value = quote!([]);
            diagnostic(
                host_module(
                    quote!(name = "counter", #ident = #value, #ident = #value),
                    item.clone(),
                ),
                &format!("duplicate `{key}`"),
            );
        }
    }

    #[test]
    fn host_requires_services_and_dts_for_nonempty_modules() {
        for (input, error) in [
            (quote!(modules: [X], services: {}), "missing `dts`"),
            (quote!(dts: "a.d.ts", modules: []), "missing `services`"),
            (quote!(dts: "a.d.ts", dts: "b.d.ts"), "duplicate `dts`"),
            (quote!(modules: [], modules: []), "duplicate `modules`"),
            (quote!(services: {}, services: {}), "duplicate `services`"),
        ] {
            diagnostic(host(input), error);
        }
    }

    #[test]
    fn host_reads_and_validates_the_declaration_file() {
        let error = host(quote!(
            dts: "tests/dts/missing.d.ts",
            modules: [],
            services: {}
        ))
        .unwrap_err()
        .to_string();
        assert!(error.contains("missing.d.ts"), "{error}");
        assert!(
            error.contains("No such file") || error.contains("cannot find"),
            "{error}"
        );

        let error = host(quote!(
            dts: "tests/dts/reexport.d.ts",
            modules: [X],
            services: {}
        ))
        .unwrap_err()
        .to_string();
        assert!(error.contains("tests/dts/reexport.d.ts:2:5"), "{error}");
        assert!(error.contains("unsupported syntax: `export *`"), "{error}");
    }

    #[test]
    fn host_rejects_old_sections() {
        for key in [quote!(classes), quote!(operations), quote!(exports)] {
            diagnostic(
                host(quote!(dts: "a.d.ts", modules: [], services: {}, #key: [])),
                "unknown host section",
            );
        }
    }

    #[test]
    fn host_accepts_empty_lists_and_service_bindings() {
        host(quote!(services: {})).unwrap();
        host(quote!(modules: [], services: {})).unwrap();
        assert!(
            host(quote!(
                dts: "tests/dts/empty.d.ts",
                modules: [],
                services: {},
            ))
            .is_ok()
        );
        let registry: Registry = syn::parse2(quote! {
            dts: "tests/dts/empty.d.ts",
            services: {
                api::Clock => tick(),
                time: clock,
                Arc<dyn TimerService> => timer,
            },
            modules: [api::V1,],
        })
        .unwrap();
        let [module] = registry.modules.as_slice() else {
            panic!("expected one module type");
        };
        assert!(
            module
                .path
                .segments
                .iter()
                .map(|segment| &segment.ident)
                .eq(["api", "V1"])
        );
        let [
            ServiceBinding::Custom { ty: clock, .. },
            ServiceBinding::Platform { name, .. },
            ServiceBinding::Custom { ty: timer, .. },
        ] = registry.services.as_slice()
        else {
            panic!("expected custom, platform, and custom bindings in source order");
        };
        assert_eq!(quote!(#clock).to_string(), quote!(api::Clock).to_string());
        assert_eq!(name, "time");
        assert_eq!(
            quote!(#timer).to_string(),
            quote!(Arc<dyn TimerService>).to_string()
        );
    }

    #[test]
    fn host_reports_malformed_service_bindings() {
        for binding in [quote!(Clock tick()), quote!(Clock? => Some(17))] {
            diagnostic(host(quote!(services: { #binding })), "expected `=>`");
        }
    }

    #[test]
    fn host_reports_unknown_and_duplicate_platform_names() {
        diagnostic(
            host(quote!(services: { diagnostcs: sink })),
            "unknown platform service: diagnostcs",
        );
        diagnostic(
            host(quote!(services: { diagnostics: one, r#diagnostics: two })),
            "duplicate platform service: diagnostics",
        );
    }

    #[test]
    fn host_emits_named_values_and_explicit_empty_slots_without_a_layout() {
        let expansion = host(quote!(services: { diagnostics: sink(), user_agent: agent })).unwrap();
        syn::parse2::<Expr>(expansion.clone()).unwrap();
        let output = expansion.to_string();
        assert!(output.contains(&quote!(let __yield_service0 = sink();).to_string()));
        for field in [
            quote!(diagnostics: ::core::option::Option::Some(__yield_service0)),
            quote!(user_agent: ::core::option::Option::Some(__yield_service1)),
            quote!(console: ::core::option::Option::None::<::yield_vm::host::Absent>),
        ] {
            assert!(output.contains(&field.to_string()), "missing field {field}");
        }
        assert!(output.contains(&quote!(ServiceStorage::default()).to_string()));
        assert!(!output.contains("offset_of"));
    }

    #[test]
    fn host_emits_typed_coercions_and_layout_fields() {
        let expansion = host(quote!(services: {
            Self => self.clone(),
            Arc<dyn TimerService> => __yield_service0,
        }))
        .unwrap();
        syn::parse2::<Expr>(expansion.clone()).unwrap();
        let output = expansion.to_string();
        assert!(output.contains(&quote!(let __yield_service0: Self = self.clone();).to_string()));
        let coercion = quote!(let __yield_service1: Arc<dyn TimerService> = __yield_service0;);
        assert!(output.contains(&coercion.to_string()));
        assert!(output.contains(&quote!(offset_of!(Self, __s0)).to_string()));
        assert!(output.contains(&quote!(offset_of!(Self, __s1)).to_string()));

        let empty = host(quote!(services: {})).unwrap().to_string();
        assert!(empty.contains(&quote!(ServiceStorage::default()).to_string()));
        assert!(!empty.contains("offset_of"));
    }

    #[test]
    fn owner_service_types_keep_the_declaration_scope() {
        let args = quote!(services = [clock: <Self as api::Config>::Clock, log: api::Log<Self>]);
        for expanded in [
            operation(
                quote!(name = "wait", recovery = pause, #args),
                quote!(
                    pub struct Owner;
                ),
            ),
            host_module(
                quote!(name = "wait", #args),
                quote!(
                    pub struct Owner;
                ),
            ),
            super::super::typed::host_class(
                quote!(tag = "wait", #args),
                quote!(
                    pub struct Owner;
                ),
            ),
        ] {
            let output = expanded.unwrap().to_string();
            assert!(output.contains("ServiceOwner for Owner"));
            assert!(output.contains("pub fn clock"));
            assert_eq!(
                output.matches("< Self as api :: Config > :: Clock").count(),
                1
            );
        }
    }

    #[test]
    fn empty_module_has_an_owner_and_owner_generic_codecs() {
        let output = host_module(
            quote!(name = "empty"),
            quote!(
                struct Empty;
            ),
        )
        .unwrap()
        .to_string();
        assert!(output.contains("ServiceOwner for Empty"));
        for codec in ["Encode", "Decode"] {
            assert!(output.contains(&format!("{codec} < __YieldOwner > for Empty")));
        }
    }

    #[test]
    fn rejects_optional_and_duplicate_accessors() {
        for args in [
            quote!(services = [optional log: Log]),
            quote!(services = [log: Log, r#log: Log]),
        ] {
            assert!(
                operation(
                    quote!(name = "wait", recovery = pause, #args),
                    quote!(
                        struct Wait;
                    )
                )
                .is_err()
            );
        }
    }
}
