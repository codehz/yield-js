use proc_macro2::TokenStream;
use quote::quote;
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    Error, Expr, Ident, ItemStruct, LitInt, LitStr, Path, Result, Token, bracketed, parenthesized,
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

struct Service {
    id: LitStr,
    version: LitInt,
    value: Expr,
}

impl Parse for Service {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let contents;
        parenthesized!(contents in input);
        let id = contents.parse()?;
        contents.parse::<Token![,]>()?;
        let version = contents.parse()?;
        contents.parse::<Token![,]>()?;
        let value = contents.parse()?;
        if contents.peek(Token![,]) {
            contents.parse::<Token![,]>()?;
        }
        if !contents.is_empty() {
            return Err(contents.error("expected `(id, version, value)`"));
        }
        Ok(Self { id, version, value })
    }
}

struct Registry {
    modules: Vec<Path>,
    classes: Vec<syn::TypePath>,
    states: Vec<syn::TypePath>,
    operations: Vec<syn::TypePath>,
    services: Vec<Service>,
}

impl Parse for Registry {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let span = input.span();
        let (mut modules, mut classes, mut states, mut operations, mut services) =
            (None, None, None, None, None);
        entries(input, |key, input| match key.to_string().as_str() {
            "modules" => set(&mut modules, &key, || list(input)),
            "classes" => set(&mut classes, &key, || list(input)),
            "states" => set(&mut states, &key, || list(input)),
            "operations" => set(&mut operations, &key, || list(input)),
            "services" => set(&mut services, &key, || list(input)),
            _ => Err(Error::new(key.span(), "unknown host_bindings section")),
        })?;
        Ok(Self {
            modules: required(modules, span, "modules")?,
            classes: required(classes, span, "classes")?,
            states: required(states, span, "states")?,
            operations: required(operations, span, "operations")?,
            services: required(services, span, "services")?,
        })
    }
}

pub(super) fn host_bindings(input: TokenStream) -> Result<TokenStream> {
    let Registry {
        modules,
        classes,
        states,
        operations,
        services,
    } = syn::parse2(input)?;
    let services = services.into_iter().map(|Service { id, version, value }| {
        quote!(::yield_vm::__macro_support::service(#id, #version, #value))
    });
    Ok(quote! {
        ::yield_vm::__macro_support::link(
            [#(#modules::__yield_module()),*],
            [#(::yield_vm::__macro_support::class::<#classes>()),*],
            [#(::yield_vm::__macro_support::state::<#states>()),*],
            [#(::yield_vm::__macro_support::operation::<#operations>()),*],
            [#(#services),*],
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            host_bindings(quote!(modules: [], classes: [], states: [], operations: [])),
            "missing `services`",
        );
        diagnostic(
            host_bindings(quote!(modules: [], modules: [])),
            "duplicate `modules`",
        );
        diagnostic(
            host_bindings(quote!(exports: [])),
            "unknown host_bindings section",
        );
    }

    #[test]
    fn host_bindings_checks_service_tuple_syntax() {
        diagnostic(
            host_bindings(quote!(services: [(ID, 1, value)])),
            "expected string literal",
        );
        diagnostic(
            host_bindings(quote!(services: [("id", VERSION, value)])),
            "expected integer literal",
        );
        diagnostic(
            host_bindings(quote!(services: [("id", 1, value, extra)])),
            "expected `(id, version, value)`",
        );
    }

    #[test]
    fn host_bindings_accepts_empty_lists_and_service_expressions() {
        assert!(
            host_bindings(
                quote!(modules: [], classes: [], states: [], operations: [], services: [],)
            )
            .is_ok()
        );
        assert!(
            host_bindings(quote! {
                services: [("id", 1, make_service(1, 2),),],
                operations: [work::Wait,],
                states: [data::Counter],
                classes: [types::Point],
                modules: [api::v1,],
            })
            .is_ok()
        );
    }
}
