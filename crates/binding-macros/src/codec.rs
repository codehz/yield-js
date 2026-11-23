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
    let codec = if encode {
        quote!(::yield_vm::Encode)
    } else {
        quote!(::yield_vm::Decode)
    };
    let mut generics = input.generics;
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
            quote!(<#ty as #codec>::encode(&self.#member, out, bindings)#context)
        } else {
            quote!(<#ty as #codec>::decode(input, bindings)#context)
        });
        members.push(member);
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let method = if encode {
        quote! {
            fn encode(&self, out: &mut ::std::vec::Vec<u8>, bindings: &::yield_vm::Bindings)
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
            fn decode(input: &mut &[u8], bindings: &::yield_vm::Bindings)
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
