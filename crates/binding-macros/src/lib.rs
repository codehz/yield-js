//! Typed Rust authoring macros for yield-vm's host bindings and VM builtins.

use proc_macro::TokenStream;

mod cps;
mod registry;
mod typed;

fn expand(result: syn::Result<proc_macro2::TokenStream>) -> TokenStream {
    result.unwrap_or_else(syn::Error::into_compile_error).into()
}

#[proc_macro_attribute]
pub fn host_module(args: TokenStream, item: TokenStream) -> TokenStream {
    expand(typed::host_module(args.into(), item.into()))
}

/// Declares an internal VM namespace and its builtin function members.
#[proc_macro_attribute]
pub fn builtin_global(args: TokenStream, item: TokenStream) -> TokenStream {
    expand(typed::builtin_global(args.into(), item.into()))
}

#[proc_macro_attribute]
pub fn host_class(args: TokenStream, item: TokenStream) -> TokenStream {
    expand(typed::host_class(args.into(), item.into()))
}

#[proc_macro_attribute]
pub fn methods(args: TokenStream, item: TokenStream) -> TokenStream {
    expand(typed::methods(args.into(), item.into()))
}

#[proc_macro_attribute]
pub fn operation(args: TokenStream, item: TokenStream) -> TokenStream {
    expand(registry::operation(args.into(), item.into()))
}

#[proc_macro_attribute]
pub fn host_state(args: TokenStream, item: TokenStream) -> TokenStream {
    expand(registry::host_state(args.into(), item.into()))
}

#[proc_macro]
pub fn host_bindings(input: TokenStream) -> TokenStream {
    expand(registry::host_bindings(input.into()))
}
