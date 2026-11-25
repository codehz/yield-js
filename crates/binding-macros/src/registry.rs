use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path as FsPath, PathBuf};

use proc_macro2::{Span as MacroSpan, TokenStream};
use quote::quote;
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    Error, Expr, Ident, ItemStruct, LitStr, Path, Result, Token, TypePath, braced, bracketed,
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

fn set<T>(slot: &mut Option<T>, key: &Ident, parse: impl FnOnce() -> Result<T>) -> Result<()> {
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
    let (mut name, mut recovery, mut rejection) = (None, None, None);
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
            _ => Err(Error::new(key.span(), "unknown operation option")),
        })
    })
    .parse2(args)?;
    let name = required(name, span, "name")?;
    let recovery = required(recovery, span, "recovery")?;
    let rejection = rejection.map(|value| quote!(const REJECTION: &'static str = #value;));

    Ok(quote! {
        #item
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
            "services" => set(&mut services, &key, || {
                input.parse::<Token![=]>()?;
                list::<ServiceDependency>(input)
            }),
            _ => Err(Error::new(key.span(), "unknown host_module option")),
        })
    })
    .parse2(args)?;
    let module = required(module, span, "name")?;
    let classes = classes.unwrap_or_default();
    let operations = operations.unwrap_or_default();
    let dependencies = (!classes.is_empty() || !operations.is_empty()).then(|| {
        quote! {
            fn register_dependencies(deps: &mut ::yield_vm::__macro_support::ModuleDependencies) {
                #(deps.class::<#classes>();)*
                #(deps.operation::<#operations>();)*
            }
        }
    });
    let services = services.unwrap_or_default();
    let service_dependencies = (!services.is_empty()).then(|| {
        let declarations = services.iter().map(|service| {
            let ServiceDependency { key, optional } = service;
            quote!(::yield_vm::__macro_support::ServiceDependency::of::<#key>(#optional))
        });
        quote! {
            const SERVICES: &'static [::yield_vm::__macro_support::ServiceDependency] = &[
                #(#declarations),*
            ];
        }
    });
    let slots = extract_slots(&mut item)?;
    let empty = match &item.fields {
        syn::Fields::Unit => Some(quote!(Self)),
        syn::Fields::Named(fields) if fields.named.is_empty() => Some(quote!(Self {})),
        syn::Fields::Unnamed(fields) if fields.unnamed.is_empty() => Some(quote!(Self())),
        _ => None,
    };
    let decls = slot_decls(&slots);
    let ty = &item.ident;
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
            impl ::yield_vm::Encode for #ty {
                fn encode(&self, _: &mut ::std::vec::Vec<u8>, _: &::yield_vm::Services) -> ::core::result::Result<(), ::yield_vm::binding::CodecError> {
                    Ok(())
                }
            }
        });
        decode = (!derives.contains("Decode")).then(|| quote! {
            impl ::yield_vm::Decode for #ty {
                fn decode(_: &mut &[u8], _: &::yield_vm::Services) -> ::core::result::Result<Self, ::yield_vm::binding::CodecError> {
                    Ok(#empty)
                }
            }
        });
    }

    Ok(quote! {
        #default
        #item
        #constants
        impl ::yield_vm::HostModule for #ty {
            const NAME: &'static str = #module;
            const SLOTS: &'static [::yield_vm::SlotDecl] = #decls;
            #dependencies
            #service_dependencies
        }
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

struct ServiceDependency {
    key: TypePath,
    optional: bool,
}

impl Parse for ServiceDependency {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(Self {
            key: input.parse()?,
            optional: input.parse::<Option<Token![?]>>()?.is_some(),
        })
    }
}

/// One `services:` entry. `Key? => option` registers the value only when the
/// option is `Some`, letting callers describe optional platform services
/// inline instead of assembling `Services` before the link.
struct ServiceBinding {
    key: TypePath,
    optional: bool,
    value: Expr,
}

impl Parse for ServiceBinding {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let key = input.parse()?;
        let optional = input.parse::<Option<Token![?]>>()?.is_some();
        input.parse::<Token![=>]>()?;
        Ok(Self {
            key,
            optional,
            value: input.parse()?,
        })
    }
}

fn service_bindings(input: ParseStream<'_>) -> Result<Vec<ServiceBinding>> {
    let contents;
    braced!(contents in input);
    Ok(contents
        .parse_terminated(ServiceBinding::parse, Token![,])?
        .into_iter()
        .collect())
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
    let inserts = services.iter().map(|binding| {
        let key = &binding.key;
        let value = &binding.value;
        if binding.optional {
            quote! {
                if let ::core::option::Option::Some(__yield_service) = #value {
                    __yield_services.insert::<#key>(__yield_service)?;
                }
            }
        } else {
            quote! {
                __yield_services.insert::<#key>(#value)?;
            }
        }
    });
    let mutable = (!modules.is_empty()).then(|| quote!(mut));
    Ok(quote! {
        {
            #declaration_validation
            match ::yield_vm::__macro_support::services(|__yield_services| {
                #(#inserts)*
                ::core::result::Result::Ok(())
            }) {
                ::core::result::Result::Ok(__yield_services) => {
                    let #mutable __yield_deps = <::yield_vm::__macro_support::ModuleDependencies as ::core::default::Default>::default();
                    let __yield_modules = [#(::yield_vm::__macro_support::module::<#modules>(&mut __yield_deps)),*];
                    ::yield_vm::__macro_support::link(
                        __yield_modules,
                        __yield_deps,
                        __yield_services,
                    )
                }
                ::core::result::Result::Err(__yield_error) => {
                    ::core::result::Result::Err(__yield_error)
                }
            }
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
            quote!(name = "counter", classes = [], operations = []),
            quote!(
                operations = [work::Wait,],
                name = "counter",
                classes = [crate::Point,]
            ),
        ] {
            host_module(args, item.clone()).unwrap();
        }
    }

    #[test]
    fn host_module_rejects_duplicate_dependency_keys() {
        let item = quote!(
            struct Counter;
        );
        for key in ["classes", "operations"] {
            let ident = Ident::new(key, MacroSpan::call_site());
            diagnostic(
                host_module(
                    quote!(name = "counter", #ident = [], #ident = []),
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
                Clock => tick(),
                Timer? => timer,
                Console? => ::core::option::Option::None,
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
        let [clock, timer, console] = registry.services.as_slice() else {
            panic!("expected three service bindings");
        };
        assert!(clock.key.path.is_ident("Clock") && !clock.optional);
        assert!(timer.key.path.is_ident("Timer") && timer.optional);
        assert!(console.key.path.is_ident("Console") && console.optional);
    }

    #[test]
    fn host_reports_malformed_service_bindings() {
        let error = host(quote!(
            dts: "tests/dts/empty.d.ts",
            modules: [],
            services: { Clock tick() }
        ))
        .unwrap_err()
        .to_string();
        assert!(error.contains("expected `=>`"), "{error}");
    }
}
