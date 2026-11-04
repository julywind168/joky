//! Field-order codec for the compiler's private, same-build standard library image.
use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Data, DeriveInput, Fields};

#[proc_macro_derive(Image)]
pub fn derive_image(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident;
    let mut generics = input.generics;
    for parameter in generics.type_params_mut() {
        parameter
            .bounds
            .push(syn::parse_quote!(crate::core::image::Image));
    }
    let (implementation, arguments, constraints) = generics.split_for_impl();
    let mut writers = Vec::new();
    let mut readers = Vec::new();
    let mut case = |path, fields: &Fields, tag: Option<u8>| {
        let names: Vec<_> = fields
            .iter()
            .enumerate()
            .map(|(i, field)| {
                field
                    .ident
                    .clone()
                    .unwrap_or_else(|| format_ident!("field_{i}"))
            })
            .collect();
        let reads = names.iter().map(|_| quote!(Image::read(input)));
        let (pattern, construct) = match fields {
            Fields::Named(_) => (quote!( { #(#names),* } ), quote!( { #(#names: #reads),* } )),
            Fields::Unnamed(_) => (quote!( ( #(#names),* ) ), quote!( ( #(#reads),* ) )),
            Fields::Unit => (quote!(), quote!()),
        };
        let tag_write = tag.map(|i| quote!(#i.write(output);));
        writers.push(quote!(#path #pattern => { #tag_write #(Image::write(#names, output);)* }));
        let value = quote!(#path #construct);
        readers.push(match tag {
            Some(i) => quote!(#i => #value),
            None => value,
        });
    };
    let read = match input.data {
        Data::Struct(data) => {
            case(quote!(Self), &data.fields, None);
            quote!(#(#readers)*)
        }
        Data::Enum(data) => {
            for (index, variant) in data.variants.iter().enumerate() {
                let ident = &variant.ident;
                case(
                    quote!(Self::#ident),
                    &variant.fields,
                    Some(index.try_into().expect("too many image variants")),
                );
            }
            quote!(match u8::read(input) { #(#readers,)* _ => panic!("invalid embedded image tag") })
        }
        Data::Union(_) => panic!("unsupported image union"),
    };
    quote! {
        impl #implementation crate::core::image::Image for #name #arguments #constraints {
            #[cfg(joky_bootstrap)]
            fn write(&self, output: &mut crate::core::image::Writer) {
                use crate::core::image::Image;
                match self { #(#writers),* }
            }
            #[cfg(not(joky_bootstrap))]
            fn read(input: &mut crate::core::image::Reader<'_>) -> Self {
                use crate::core::image::Image;
                #read
            }
        }
    }
    .into()
}
