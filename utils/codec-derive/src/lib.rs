use {
    quote::format_ident,
    std::ops::Not,
    syn::{
        punctuated::Punctuated,
        Meta, Token,
    },
};

extern crate proc_macro;
use {
    proc_macro::TokenStream,
    quote::quote,
    syn::{parse_macro_input, DeriveInput},
};

#[proc_macro_derive(Codec, attributes(codec))]
pub fn derive_codec(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let crate_name = syn::Ident::new("component_utils", proc_macro2::Span::call_site());

    match input.data {
        syn::Data::Struct(s) => derive_codec_struct(crate_name, input.ident, input.generics, s),
        syn::Data::Enum(e) => derive_codec_enum(crate_name, input.ident, input.generics, e),
        syn::Data::Union(_) => unimplemented!("Unions are not supported"),
    }
    .into()
}

fn derive_codec_enum(
    crate_name: proc_macro2::Ident,
    ident: proc_macro2::Ident,
    generics: syn::Generics,
    e: syn::DataEnum,
) -> proc_macro2::TokenStream {
    let variant_index = 0..e.variants.len() as u8;
    let variant_index2 = 0..e.variants.len() as u8;

    let destructure = e.variants.iter().map(|v| {
        let name = &v.ident;
        match &v.fields {
            syn::Fields::Named(n) => {
                let field_names = n.named.iter().map(|f| {
                    let name = &f.ident;
                    if FieldAttrFlags::new(&f.attrs).ignore {
                        quote! { #name: _ }
                    } else {
                        quote! { #name }
                    }
                });
                quote! { #name {#(#field_names),*} }
            }
            syn::Fields::Unnamed(u) => {
                let field_names = u.unnamed.iter().enumerate().map(|(i, f)| {
                    if FieldAttrFlags::new(&f.attrs).ignore {
                        format_ident!("_")
                    } else {
                        format_ident!("f{}", i)
                    }
                });
                quote! { #name (#(#field_names),*) }
            }
            syn::Fields::Unit => quote! { #name },
        }
    });

    let encode_variant = e.variants.iter().map(|v| match &v.fields {
        syn::Fields::Named(n) => {
            let field_names = n.named.iter().filter_map(|f| {
                FieldAttrFlags::new(&f.attrs)
                    .ignore
                    .not()
                    .then_some(&f.ident)
            });
            quote! { #(Codec::<'a>::encode(#field_names, buffer)?;)* }
        }
        syn::Fields::Unnamed(u) => {
            let field_names = u
                .unnamed
                .iter()
                .map(|f| FieldAttrFlags::new(&f.attrs))
                .enumerate()
                .filter_map(|(i, f)| f.ignore.not().then(|| format_ident!("f{}", i)));
            quote! { #(Codec::<'a>::encode(#field_names, buffer)?;)* }
        }
        syn::Fields::Unit => quote! {},
    });

    let decode_variant = e.variants.iter().map(|v| {
        let name = &v.ident;
        match &v.fields {
            syn::Fields::Named(n) => {
                let fields = n.named.iter().map(|f| {
                    let name = &f.ident;
                    if FieldAttrFlags::new(&f.attrs).ignore {
                        quote! { #name: Default::default() }
                    } else {
                        quote! { #name: Codec::decode(buffer)? }
                    }
                });
                quote! { #name {#(#fields),*} }
            }
            syn::Fields::Unnamed(u) => {
                let fields = u.unnamed.iter().map(|f| {
                    if FieldAttrFlags::new(&f.attrs).ignore {
                        quote! { Default::default() }
                    } else {
                        quote! { Codec::decode(buffer)? }
                    }
                });
                quote! { #name (#(#fields),*) }
            }
            syn::Fields::Unit => quote! { #name },
        }
    });

    quote! {
        impl<'a> #crate_name::Codec<'a> for #ident #generics {
            fn encode(&self, buffer: &mut impl #crate_name::Buffer) -> Option<()> {
                match self {
                    #(Self::#destructure => {
                        buffer.push(#variant_index)?;
                        #encode_variant
                    },)*
                }
                Some(())
            }

            fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
                let index = buffer.get(0)?;
                *buffer = &buffer[1..];

                match index {
                    #(#variant_index2 => Some(Self::#decode_variant),)*
                    _ => None,
                }
            }
        }
    }
}

fn derive_codec_struct(
    crate_name: proc_macro2::Ident,
    ident: proc_macro2::Ident,
    generics: syn::Generics,
    s: syn::DataStruct,
) -> proc_macro2::TokenStream {
    match s.fields {
        syn::Fields::Named(n) => derive_codec_named_struct(crate_name, ident, generics, n),
        syn::Fields::Unnamed(u) => derive_codec_unnamed_struct(crate_name, ident, generics, u),
        syn::Fields::Unit => derive_codec_unit_struct(crate_name, ident, generics),
    }
}

fn derive_codec_unnamed_struct(
    crate_name: proc_macro2::Ident,
    ident: proc_macro2::Ident,
    generics: syn::Generics,
    u: syn::FieldsUnnamed,
) -> proc_macro2::TokenStream {
    let flags = u
        .unnamed
        .iter()
        .map(|f| FieldAttrFlags::new(&f.attrs))
        .collect::<Vec<_>>();

    let field_names = flags.iter().enumerate().map(|(i, f)| {
        if f.ignore {
            format_ident!("_")
        } else {
            format_ident!("f{}", i)
        }
    });
    let used_fields = flags
        .iter()
        .enumerate()
        .filter_map(|(i, f)| f.ignore.not().then(|| format_ident!("f{}", i)));

    let decode_fields = flags.iter().map(|f| {
        if f.ignore {
            quote! { Default::default() }
        } else {
            quote! { Codec::decode(buffer)? }
        }
    });

    quote! {
        impl<'a> #crate_name::Codec<'a> for #ident #generics {
            fn encode(&self, buffer: &mut impl #crate_name::Buffer) -> Option<()> {
                let Self(#(#field_names,)*) = self;
                #(Codec::<'a>::encode(#used_fields, buffer)?;)*
                Some(())
            }

            fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
                Some(Self(#(#decode_fields,)*))
            }
        }
    }
}

fn derive_codec_named_struct(
    crate_name: proc_macro2::Ident,
    ident: proc_macro2::Ident,
    generics: syn::Generics,
    n: syn::FieldsNamed,
) -> proc_macro2::TokenStream {
    let flags = n
        .named
        .iter()
        .map(|f| FieldAttrFlags::new(&f.attrs))
        .collect::<Vec<_>>();

    let field_names = flags.iter().zip(&n.named).map(|(f, nf)| {
        let name = &nf.ident;
        if f.ignore {
            quote! { #name: _ }
        } else {
            quote! { #name }
        }
    });
    let used_fields = flags
        .iter()
        .zip(&n.named)
        .filter_map(|(f, nf)| f.ignore.not().then_some(&nf.ident));

    let decode_fields = flags.iter().zip(&n.named).map(|(f, nf)| {
        let name = &nf.ident;
        if f.ignore {
            quote! { #name: Default::default() }
        } else {
            quote! { #name: Codec::decode(buffer)? }
        }
    });

    quote! {
        impl<'a> #crate_name::Codec<'a> for #ident #generics {
            fn encode(&self, buffer: &mut impl #crate_name::Buffer) -> Option<()> {
                let Self { #(#field_names,)* } = self;
                #(Codec::<'a>::encode(#used_fields, buffer)?;)*
                Some(())
            }

            fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
                Some(Self { #(#decode_fields,)* })
            }
        }
    }
}

fn derive_codec_unit_struct(
    crate_name: proc_macro2::Ident,
    ident: proc_macro2::Ident,
    generics: syn::Generics,
) -> proc_macro2::TokenStream {
    quote! {
        impl<'a> #crate_name::Codec<'a> for #ident #generics {
            fn encode(&self, buffer: &mut impl #crate_name::Buffer) -> Option<()> {
                Some(())
            }

            fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
                Some(Self)
            }
        }
    }
}

#[derive(Default)]
struct FieldAttrFlags {
    ignore: bool,
}

impl FieldAttrFlags {
    fn new(attributes: &[syn::Attribute]) -> Self {
        attributes
            .iter()
            .filter(|a| a.path().is_ident("codec"))
            .filter_map(|a| match &a.meta {
                Meta::List(ml) => Some(ml),
                _ => None,
            })
            .flat_map(|ml| {
                ml.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                    .unwrap()
            })
            .fold(Self::default(), |s, m| Self {
                ignore: s.ignore || m.path().is_ident("skip"),
            })
    }
}
