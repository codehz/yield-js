use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path as FsPath, PathBuf};

use proc_macro2::{Span as MacroSpan, TokenStream};
use quote::quote;
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    Error, Expr, Ident, ItemStruct, LitInt, LitStr, Path, Result, Token, TypePath, braced,
    bracketed,
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
    let (mut name, mut schema, mut recovery, mut rejection, mut encode, mut decode) =
        (None, None, None, None, None, None);
    (|input: ParseStream<'_>| {
        entries(input, |key, input| match key.to_string().as_str() {
            "name" => set(&mut name, &key, || attribute_value::<LitStr>(input)),
            "schema" => set(&mut schema, &key, || attribute_value::<LitInt>(input)),
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
            "encode" => set(&mut encode, &key, || attribute_value::<Path>(input)),
            "decode" => set(&mut decode, &key, || attribute_value::<Path>(input)),
            _ => Err(Error::new(key.span(), "unknown operation option")),
        })
    })
    .parse2(args)?;
    let name = required(name, span, "name")?;
    let schema = required(schema, span, "schema")?;
    let recovery = required(recovery, span, "recovery")?;
    let encode = required(encode, span, "encode")?;
    let decode = required(decode, span, "decode")?;
    let rejection = rejection.map(|value| quote!(const REJECTION: &'static str = #value;));

    Ok(quote! {
        #item
        impl ::yield_vm::Operation for #ty {
            const NAME: &'static str = #name;
            const SCHEMA: u32 = #schema;
            const RECOVERY: ::yield_vm::Recovery = ::yield_vm::Recovery::#recovery;
            #rejection

            fn encode(&self, out: &mut ::std::vec::Vec<u8>) -> ::core::result::Result<(), ::yield_vm::binding::CodecError> {
                let callback: fn(&Self, &mut ::std::vec::Vec<u8>) -> ::core::result::Result<(), ::yield_vm::binding::CodecError> = #encode;
                callback(self, out)
            }

            fn decode(input: &[u8], bindings: &::yield_vm::Bindings) -> ::core::result::Result<Self, ::yield_vm::binding::CodecError> {
                let callback: fn(&[u8], &::yield_vm::Bindings) -> ::core::result::Result<Self, ::yield_vm::binding::CodecError> = #decode;
                callback(input, bindings)
            }
        }
    })
}

pub(super) fn host_state(args: TokenStream, item: TokenStream) -> Result<TokenStream> {
    use syn::parse::Parser;

    let mut item: ItemStruct = syn::parse2(item)?;
    check_struct(&item)?;
    let span = item.ident.span();
    let (mut module, mut schema, mut init, mut encode, mut decode) = (None, None, None, None, None);
    (|input: ParseStream<'_>| {
        entries(input, |key, input| match key.to_string().as_str() {
            "module" => set(&mut module, &key, || attribute_value::<LitStr>(input)),
            "schema" => set(&mut schema, &key, || attribute_value::<LitInt>(input)),
            "init" => set(&mut init, &key, || attribute_value::<Path>(input)),
            "encode" => set(&mut encode, &key, || attribute_value::<Path>(input)),
            "decode" => set(&mut decode, &key, || attribute_value::<Path>(input)),
            _ => Err(Error::new(key.span(), "unknown host_state option")),
        })
    })
    .parse2(args)?;
    let module = required(module, span, "module")?;
    let schema = required(schema, span, "schema")?;
    let init = required(init, span, "init")?;
    let encode = required(encode, span, "encode")?;
    let decode = required(decode, span, "decode")?;
    let slots = extract_slots(&mut item)?;
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

    Ok(quote! {
        #item
        #constants
        impl ::yield_vm::HostModuleState for #ty {
            const MODULE: &'static str = #module;
            const SLOTS: &'static [::yield_vm::SlotDecl] = #decls;
            const PAYLOAD_SCHEMA: u32 = #schema;

            fn new() -> Self {
                let callback: fn() -> Self = #init;
                callback()
            }

            fn encode_payload(&self, out: &mut ::std::vec::Vec<u8>) -> ::core::result::Result<(), ::yield_vm::binding::CodecError> {
                let callback: fn(&Self, &mut ::std::vec::Vec<u8>) -> ::core::result::Result<(), ::yield_vm::binding::CodecError> = #encode;
                callback(self, out)
            }

            fn decode_payload(input: &[u8], cx: &mut ::yield_vm::binding::DecodeCx<'_>) -> ::core::result::Result<Self, ::yield_vm::binding::CodecError> {
                let callback: fn(&[u8], &mut ::yield_vm::binding::DecodeCx<'_>) -> ::core::result::Result<Self, ::yield_vm::binding::CodecError> = #decode;
                callback(input, cx)
            }
        }
    })
}

fn list<T: Parse>(input: ParseStream<'_>) -> Result<Vec<T>> {
    input.parse::<Token![:]>()?;
    let contents;
    bracketed!(contents in input);
    Ok(contents
        .parse_terminated(T::parse, Token![,])?
        .into_iter()
        .collect())
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
    dts: LitStr,
    modules: Vec<Path>,
    classes: Vec<syn::TypePath>,
    states: Vec<syn::TypePath>,
    operations: Vec<syn::TypePath>,
    services: Vec<ServiceBinding>,
}

impl Parse for Registry {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let span = input.span();
        let (mut dts, mut modules, mut classes, mut states, mut operations, mut services) =
            (None, None, None, None, None, None);
        entries(input, |key, input| match key.to_string().as_str() {
            "dts" => set(&mut dts, &key, || {
                input.parse::<Token![:]>()?;
                input.parse()
            }),
            "modules" => set(&mut modules, &key, || list(input)),
            "classes" => set(&mut classes, &key, || list(input)),
            "states" => set(&mut states, &key, || list(input)),
            "operations" => set(&mut operations, &key, || list(input)),
            "services" => set(&mut services, &key, || {
                input.parse::<Token![:]>()?;
                service_bindings(input)
            }),
            _ => Err(Error::new(key.span(), "unknown host_bindings section")),
        })?;
        Ok(Self {
            dts: required(dts, span, "dts")?,
            modules: required(modules, span, "modules")?,
            classes: required(classes, span, "classes")?,
            states: required(states, span, "states")?,
            operations: required(operations, span, "operations")?,
            services: required(services, span, "services")?,
        })
    }
}

pub(super) fn host_bindings(input: TokenStream, origin: Option<PathBuf>) -> Result<TokenStream> {
    let Registry {
        dts,
        modules,
        classes,
        states,
        operations,
        services,
    } = syn::parse2(input)?;
    let (path, declarations) = read_declarations(&dts, origin.as_deref())?;
    let include = LitStr::new(&path.to_string_lossy(), dts.span());
    let checks = declaration_checks(&declarations, &modules);
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
    Ok(quote! {
        {
            const _: &str = include_str!(#include);
            #(#checks)*
            match ::yield_vm::__macro_support::services(|__yield_services| {
                #(#inserts)*
                ::core::result::Result::Ok(())
            }) {
                ::core::result::Result::Ok(__yield_services) => {
                    ::yield_vm::__macro_support::link(
                        [#(#modules::__yield_module()),*],
                        [#(::yield_vm::__macro_support::class::<#classes>()),*],
                        [#(::yield_vm::__macro_support::state::<#states>()),*],
                        [#(::yield_vm::__macro_support::operation::<#operations>()),*],
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
    modules: &[Path],
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
                    let modules: &[(&str, &[&str])] = &[#(#modules::__YIELD_NAMES),*];
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

    fn host_bindings(input: TokenStream) -> Result<TokenStream> {
        super::host_bindings(input, None)
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
            operation(
                quote!(
                    name = "wait",
                    schema = 1,
                    encode = codec::encode,
                    decode = codec::decode
                ),
                item.clone(),
            ),
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
    fn host_state_reports_missing_init_and_slot_constant_collisions() {
        diagnostic(
            host_state(
                quote!(
                    module = "counter",
                    schema = 1,
                    encode = encode,
                    decode = decode
                ),
                quote!(
                    struct Counter;
                ),
            ),
            "missing `init`",
        );
        diagnostic(
            host_state(
                quote!(
                    module = "counter",
                    schema = 1,
                    init = init,
                    encode = encode,
                    decode = decode
                ),
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
    fn host_bindings_requires_all_sections_exactly_once() {
        diagnostic(
            host_bindings(quote!(
                dts: "tests/dts/empty.d.ts",
                modules: [],
                classes: [],
                states: [],
                operations: []
            )),
            "missing `services`",
        );
        diagnostic(
            host_bindings(quote!(dts: "a.d.ts", dts: "b.d.ts")),
            "duplicate `dts`",
        );
        diagnostic(
            host_bindings(quote!(exports: [])),
            "unknown host_bindings section",
        );
    }

    #[test]
    fn host_bindings_reads_and_validates_the_declaration_file() {
        let error = host_bindings(quote!(
            dts: "tests/dts/missing.d.ts",
            modules: [],
            classes: [],
            states: [],
            operations: [],
            services: {}
        ))
        .unwrap_err()
        .to_string();
        assert!(error.contains("missing.d.ts"), "{error}");
        assert!(
            error.contains("No such file") || error.contains("cannot find"),
            "{error}"
        );

        let error = host_bindings(quote!(
            dts: "tests/dts/reexport.d.ts",
            modules: [x],
            classes: [],
            states: [],
            operations: [],
            services: {}
        ))
        .unwrap_err()
        .to_string();
        assert!(error.contains("tests/dts/reexport.d.ts:2:5"), "{error}");
        assert!(error.contains("unsupported syntax: `export *`"), "{error}");
    }

    #[test]
    fn host_bindings_rejects_duplicate_services_sections() {
        diagnostic(
            host_bindings(quote!(services: {}, services: {})),
            "duplicate `services`",
        );
    }

    #[test]
    fn host_bindings_accepts_empty_lists_and_service_bindings() {
        assert!(
            host_bindings(quote!(
                dts: "tests/dts/empty.d.ts",
                modules: [],
                classes: [],
                states: [],
                operations: [],
                services: {},
            ))
            .is_ok()
        );
        assert!(
            host_bindings(quote! {
                dts: "tests/dts/empty.d.ts",
                services: {
                    Clock => tick(),
                    Timer? => timer,
                    Console? => ::core::option::Option::None,
                },
                operations: [work::Wait,],
                states: [data::Counter],
                classes: [types::Point],
                modules: [api::v1,],
            })
            .is_ok()
        );
    }

    #[test]
    fn host_bindings_reports_malformed_service_bindings() {
        let error = host_bindings(quote!(
            dts: "tests/dts/empty.d.ts",
            modules: [],
            classes: [],
            states: [],
            operations: [],
            services: { Clock tick() }
        ))
        .unwrap_err()
        .to_string();
        assert!(error.contains("expected `=>`"), "{error}");
    }
}
