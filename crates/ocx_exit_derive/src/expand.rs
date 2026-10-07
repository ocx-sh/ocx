// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Turns a parsed `#[derive(Classify)]` item into its two trait impls.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{Attribute, Data, DeriveInput, Fields, GenericArgument, Ident, LitStr, Member, PathArguments, Type};

use crate::spec::{FieldPath, Form, Row, Spec, member_name};

/// One enum variant, or the whole struct, with the form that classifies it.
struct Arm {
    /// `None` for a struct.
    variant: Option<Ident>,
    fields: Fields,
    form: Form,
}

/// A row the type declares, after deduplication by slug.
struct Declared {
    slug: String,
    code: String,
    summary: String,
    row: Row,
}

pub(crate) fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let type_spec = parse_exit(&input.attrs, input.ident.span())?;
    let family_name = type_spec
        .as_ref()
        .and_then(|spec| spec.family.as_ref())
        .map_or_else(|| input.ident.to_string(), LitStr::value);
    let reserved = type_spec.as_ref().map(|spec| spec.reserved.clone()).unwrap_or_default();
    let type_form = type_spec.and_then(|spec| spec.form);

    let arms = collect_arms(&input, type_form.as_ref())?;
    let rows = collect_rows(&arms, &reserved)?;
    let row_const = |slug: &LitStr| {
        let slug = slug.value();
        let index = rows
            .iter()
            .position(|declared| declared.slug == slug)
            .unwrap_or_default();
        format_ident!("__EXIT_ROW_{index}")
    };

    let name = &input.ident;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    let family = LitStr::new(&family_name, Span::call_site());

    let row_consts = rows.iter().enumerate().map(|(index, declared)| {
        let const_name = format_ident!("__EXIT_ROW_{index}");
        let Row {
            code, slug, summary, ..
        } = &declared.row;
        quote! {
            #[doc(hidden)]
            const #const_name: ::ocx_exit::DetailEntry = ::ocx_exit::DetailEntry {
                slug: #slug,
                exit_code: ::ocx_exit::ExitCode::#code,
                family: #family,
                summary: #summary,
            };
        }
    });

    let mut with_consts = Vec::new();
    let mut classify_arms = Vec::new();
    let mut detail_arms = Vec::new();
    let mut delegates = false;
    for (position, arm) in arms.iter().enumerate() {
        let base = arm_base(arm);
        match &arm.form {
            Form::Fixed(row) => {
                let index = row_const(&row.slug);
                let code = &row.code;
                let classify = if row.defers {
                    quote!(::std::option::Option::None)
                } else {
                    quote!(::std::option::Option::Some(::ocx_exit::ExitCode::#code))
                };
                classify_arms.push(quote!(#base { .. } => #classify));
                detail_arms.push(quote!(#base { .. } => ::ocx_exit::Detail::Fixed(&Self::#index)));
            }
            Form::Delegate(path) => {
                delegates = true;
                let (member, rest) = delegate_target(arm, path.as_ref())?;
                let pattern = quote!(#base { #member: __exit_field, .. });
                classify_arms.push(quote!(#pattern => __exit_field #(.#rest)*.classify()));
                detail_arms.push(quote!(#pattern => __exit_field #(.#rest)*.kind_detail()));
            }
            Form::Chain { field: None, fallback } => {
                let index = row_const(&fallback.slug);
                classify_arms.push(quote!(#base { .. } => ::std::option::Option::None));
                detail_arms
                    .push(quote!(#base { .. } => ::ocx_exit::Detail::Chain { fallback: &Self::#index, from: self }));
            }
            Form::Chain {
                field: Some(path),
                fallback,
            } => {
                let (member, rest) = delegate_target(arm, Some(path))?;
                let index = row_const(&fallback.slug);
                let code = &fallback.code;
                let pattern = quote!(#base { #member: __exit_field, .. });
                let place = if rest.is_empty() {
                    quote!(*__exit_field)
                } else {
                    quote!(__exit_field #(.#rest)*)
                };
                classify_arms.push(quote!(#pattern => ::std::option::Option::Some(::ocx_exit::ExitCode::#code)));
                detail_arms
                    .push(quote!(#pattern => ::ocx_exit::Detail::Chain { fallback: &Self::#index, from: &#place }));
            }
            Form::With { path, rows: arm_rows } => {
                let const_name = format_ident!("__EXIT_WITH_{position}");
                let count = arm_rows.len();
                let entries = arm_rows.iter().map(|row| {
                    let index = row_const(&row.slug);
                    let defers = row.defers;
                    quote!(::ocx_exit::Row::__declared(&Self::#index, #defers))
                });
                with_consts.push(quote! {
                    #[doc(hidden)]
                    const #const_name: [::ocx_exit::Row; #count] = [#(#entries),*];
                });
                classify_arms.push(quote!(#base { .. } => ::ocx_exit::Pick::code(#path(self, Self::#const_name))));
                detail_arms.push(quote!(#base { .. } => ::ocx_exit::Pick::detail(#path(self, Self::#const_name))));
            }
        }
    }

    let details = struct_delegate_details(&input, &arms, &reserved)?.unwrap_or_else(|| {
        let indices = (0..rows.len()).map(|index| format_ident!("__EXIT_ROW_{index}"));
        quote!(&[#(Self::#indices),*])
    });
    // Only a delegating arm calls a trait method, so the imports exist only then: an unused one is a warning.
    let classify_use = delegates.then(|| {
        quote!(
            use ::ocx_exit::ClassifyExitCode as _;
        )
    });
    let detail_use = delegates.then(|| {
        quote!(
            use ::ocx_exit::ClassifyErrorKind as _;
        )
    });

    Ok(quote! {
        impl #impl_generics #name #type_generics #where_clause {
            #(#row_consts)*
            #(#with_consts)*
        }

        impl #impl_generics ::ocx_exit::ClassifyExitCode for #name #type_generics #where_clause {
            fn classify(&self) -> ::std::option::Option<::ocx_exit::ExitCode> {
                #classify_use
                match self {
                    #(#classify_arms,)*
                }
            }
        }

        impl #impl_generics ::ocx_exit::ClassifyErrorKind for #name #type_generics #where_clause {
            const DETAILS: &'static [::ocx_exit::DetailEntry] = #details;

            fn kind_detail(&self) -> ::ocx_exit::Detail<'_> {
                #detail_use
                match self {
                    #(#detail_arms,)*
                }
            }
        }
    })
}

/// The single `#[exit(...)]` among `attrs`, if any.
fn parse_exit(attrs: &[Attribute], span: Span) -> syn::Result<Option<Spec>> {
    let mut found = attrs.iter().filter(|attr| attr.path().is_ident("exit"));
    let Some(first) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err(syn::Error::new(span, "at most one `#[exit(...)]` per item"));
    }
    first.parse_args::<Spec>().map(Some)
}

fn collect_arms(input: &DeriveInput, type_form: Option<&Form>) -> syn::Result<Vec<Arm>> {
    match &input.data {
        Data::Enum(data) => {
            if data.variants.is_empty() {
                return Err(syn::Error::new(
                    input.ident.span(),
                    "an enum with no variants has nothing to classify",
                ));
            }
            data.variants
                .iter()
                .map(|variant| {
                    let own = parse_exit(&variant.attrs, variant.ident.span())?;
                    if own.as_ref().is_some_and(|spec| spec.family.is_some()) {
                        return Err(syn::Error::new(
                            variant.ident.span(),
                            "`family` belongs on the type, not a variant",
                        ));
                    }
                    if own.as_ref().is_some_and(|spec| !spec.reserved.is_empty()) {
                        return Err(syn::Error::new(
                            variant.ident.span(),
                            "`reserve` belongs on the type, not a variant",
                        ));
                    }
                    let form = own
                        .and_then(|spec| spec.form)
                        .or_else(|| type_form.cloned())
                        .ok_or_else(|| {
                            syn::Error::new(
                                variant.ident.span(),
                                format!(
                                    "variant `{}` has no `#[exit(...)]`: every variant declares its exit code and slug",
                                    variant.ident
                                ),
                            )
                        })?;
                    Ok(Arm {
                        variant: Some(variant.ident.clone()),
                        fields: variant.fields.clone(),
                        form,
                    })
                })
                .collect()
        }
        Data::Struct(data) => {
            let form = type_form.cloned().ok_or_else(|| {
                syn::Error::new(
                    input.ident.span(),
                    format!(
                        "struct `{}` has no `#[exit(...)]`: declare its exit code and slug",
                        input.ident
                    ),
                )
            })?;
            Ok(vec![Arm {
                variant: None,
                fields: data.fields.clone(),
                form,
            }])
        }
        Data::Union(_) => Err(syn::Error::new(input.ident.span(), "a union is not an error type")),
    }
}

/// Every declared row in arm order, then the type's reserved rows, deduplicated by slug; a slug declared again must
/// agree.
fn collect_rows(arms: &[Arm], reserved: &[Row]) -> syn::Result<Vec<Declared>> {
    let mut declared: Vec<Declared> = Vec::new();
    let per_arm = arms.iter().map(|arm| match &arm.form {
        Form::Fixed(row) => vec![row],
        Form::Chain { fallback, .. } => vec![fallback],
        Form::With { rows, .. } => rows.iter().collect(),
        Form::Delegate(_) => Vec::new(),
    });
    for rows in per_arm.chain(std::iter::once(reserved.iter().collect())) {
        for row in rows {
            let slug = row.slug.value();
            let code = row.code.to_string();
            let summary = row.summary.value();
            match declared.iter().find(|existing| existing.slug == slug) {
                Some(existing) if existing.code != code || existing.summary != summary => {
                    return Err(syn::Error::new(
                        row.slug.span(),
                        format!("slug `{slug}` is declared twice with a different exit code or summary"),
                    ));
                }
                Some(_) => {}
                None => declared.push(Declared {
                    slug,
                    code,
                    summary,
                    row: row.clone(),
                }),
            }
        }
    }
    Ok(declared)
}

/// `Self::Variant` for a variant, `Self` for a struct: the path a pattern starts with.
fn arm_base(arm: &Arm) -> TokenStream {
    match &arm.variant {
        Some(variant) => quote!(Self::#variant),
        None => quote!(Self),
    }
}

/// The field a delegating arm binds and the fields to read through after it.
fn delegate_target(arm: &Arm, path: Option<&FieldPath>) -> syn::Result<(Member, Vec<Member>)> {
    let span = arm.variant.as_ref().map_or_else(Span::call_site, Ident::span);
    let path = match path {
        Some(path) => path.clone(),
        None => {
            let mut fields = arm.fields.members();
            match (fields.next(), fields.next()) {
                (Some(only), None) => FieldPath(vec![only]),
                _ => {
                    return Err(syn::Error::new(
                        span,
                        "`delegate` and `chain` without a field need exactly one field: name it with `= field`",
                    ));
                }
            }
        }
    };
    let mut members = path.0.into_iter();
    let Some(first) = members.next() else {
        return Err(syn::Error::new(span, "an empty field path"));
    };
    if !arm
        .fields
        .members()
        .any(|member| member_name(&member) == member_name(&first))
    {
        return Err(syn::Error::new(
            span,
            format!("no field `{}` to delegate to", member_name(&first)),
        ));
    }
    Ok((first, members.collect()))
}

/// A struct's `#[exit(delegate = field)]` answers `DETAILS` with its field type's table.
fn struct_delegate_details(input: &DeriveInput, arms: &[Arm], reserved: &[Row]) -> syn::Result<Option<TokenStream>> {
    let (Data::Struct(data), [arm]) = (&input.data, arms) else {
        return Ok(None);
    };
    let Form::Delegate(path) = &arm.form else {
        return Ok(None);
    };
    if let Some(row) = reserved.first() {
        return Err(syn::Error::new(
            row.slug.span(),
            "a delegating struct's `DETAILS` is its field type's, so it has no `reserve` rows of its own",
        ));
    }
    let (member, rest) = delegate_target(arm, path.as_ref())?;
    if !rest.is_empty() {
        return Err(syn::Error::new(
            input.span(),
            "a struct delegates to one of its own fields: its `DETAILS` is that field type's",
        ));
    }
    let ty: &Type = data
        .fields
        .iter()
        .zip(data.fields.members())
        .find(|(_, candidate)| member_name(candidate) == member_name(&member))
        .map(|(field, _)| &field.ty)
        .ok_or_else(|| syn::Error::new(input.span(), "no field to delegate to"))?;
    let ty = pointee(ty);
    Ok(Some(quote!(<#ty as ::ocx_exit::ClassifyErrorKind>::DETAILS)))
}

/// `T` for `Box<T>` and `Arc<T>`: the pointer types implement neither trait, delegation derefs to `T`.
fn pointee(ty: &Type) -> &Type {
    let Type::Path(path) = ty else {
        return ty;
    };
    let Some(segment) = path.path.segments.last() else {
        return ty;
    };
    if segment.ident != "Box" && segment.ident != "Arc" {
        return ty;
    }
    match &segment.arguments {
        PathArguments::AngleBracketed(arguments) => match arguments.args.first() {
            Some(GenericArgument::Type(inner)) => inner,
            _ => ty,
        },
        _ => ty,
    }
}
