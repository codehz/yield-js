use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Index, Member, parse_quote};

pub(crate) fn derive(input: DeriveInput, encode: bool) -> syn::Result<TokenStream> {
    let name = input.ident;
    let Data::Struct(data) = input.data else {
        return Err(syn::Error::new(
            name.span(),
            "payload codecs can only be derived for structs",
        ));
    };
    let mut owner = quote::format_ident!("__YieldOwner", span = proc_macro2::Span::mixed_site());
    while owner == name
        || input.generics.params.iter().any(|param| match param {
            syn::GenericParam::Type(param) => param.ident == owner,
            syn::GenericParam::Const(param) => param.ident == owner,
            syn::GenericParam::Lifetime(_) => false,
        })
    {
        owner = quote::format_ident!("{owner}_");
    }
    let codec = if encode {
        quote!(::yield_vm::Encode<#owner>)
    } else {
        quote!(::yield_vm::Decode<#owner>)
    };
    let (_, ty_generics, _) = input.generics.split_for_impl();
    let mut generics = input.generics.clone();
    generics.params.push(parse_quote!(#owner));
    let mut members = Vec::new();
    let mut steps = Vec::new();
    for (index, field) in data.fields.iter().enumerate() {
        let ty = &field.ty;
        let (member, label) = match &field.ident {
            Some(ident) => (Member::Named(ident.clone()), ident.to_string()),
            None => (Member::Unnamed(Index::from(index)), index.to_string()),
        };
        generics
            .make_where_clause()
            .predicates
            .push(parse_quote!(#ty: #codec));
        let context = quote!(.map_err(|error| ::yield_vm::binding::CodecError::new(
            ::std::format!("field {}: {}", #label, error)
        ))?);
        steps.push(if encode {
            quote!(<#ty as #codec>::encode(&self.#member, out, services)#context)
        } else {
            quote!(<#ty as #codec>::decode(input, services)#context)
        });
        members.push(member);
    }
    let (impl_generics, _, where_clause) = generics.split_for_impl();
    let method = if encode {
        quote! {
            fn encode(&self, out: &mut ::std::vec::Vec<u8>, services: &::yield_vm::Services<'_, #owner>)
                -> ::core::result::Result<(), ::yield_vm::binding::CodecError>
            {
                #(#steps;)*
                Ok(())
            }
        }
    } else {
        let value = match data.fields {
            Fields::Named(_) => quote!(Self { #(#members: #steps),* }),
            Fields::Unnamed(_) => quote!(Self(#(#steps),*)),
            Fields::Unit => quote!(Self),
        };
        quote! {
            fn decode(input: &mut &[u8], services: &::yield_vm::Services<'_, #owner>)
                -> ::core::result::Result<Self, ::yield_vm::binding::CodecError>
            {
                Ok(#value)
            }
        }
    };
    Ok(quote! {
        impl #impl_generics #codec for #name #ty_generics #where_clause {
            #method
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_forward_the_owner_without_adding_it_to_the_payload_type() {
        let data: DeriveInput = parse_quote!(
            struct Payload<'a, __YieldOwner>(&'a __YieldOwner);
        );
        for (encode, trait_name) in [(true, "Encode"), (false, "Decode")] {
            let output = derive(data.clone(), encode).unwrap().to_string();
            assert!(output.contains(&format!(
                "{trait_name} < __YieldOwner_ > for Payload < 'a , __YieldOwner >"
            )));
            assert!(output.contains(&format!(
                "& 'a __YieldOwner : :: yield_vm :: {trait_name} < __YieldOwner_ >"
            )));
            assert!(output.contains("Services < '_ , __YieldOwner_ >"));
        }
    }
}
