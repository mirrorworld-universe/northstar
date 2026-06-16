use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, quote};
use syn::{Attribute, Fields, ItemStruct, Meta, parse_macro_input};

#[proc_macro_attribute]
pub fn delegate(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemStruct);
    expand_delegate(input)
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}

fn expand_delegate(input: ItemStruct) -> syn::Result<TokenStream2> {
    let struct_name = &input.ident;
    let struct_attrs = &input.attrs;
    let vis = &input.vis;
    let generics = &input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let fields = match &input.fields {
        Fields::Named(fields) => &fields.named,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "#[delegate] supports only structs with named fields",
            ));
        }
    };

    let mut emitted_fields = Vec::new();
    let mut methods = Vec::new();
    let mut has_owner_program = false;
    let mut has_portal_program = false;
    let mut has_session = false;
    let mut has_system_program = false;

    for field in fields {
        let field_name = field.ident.as_ref().ok_or_else(|| {
            syn::Error::new_spanned(field, "#[delegate] supports only named fields")
        })?;
        let field_ty = &field.ty;
        let field_vis = &field.vis;
        let stripped_attrs = strip_del_marker(&field.attrs)?;

        if field_name == "owner_program" {
            has_owner_program = true;
        } else if field_name == "portal_program" {
            has_portal_program = true;
        } else if field_name == "session" {
            has_session = true;
        } else if field_name == "system_program" {
            has_system_program = true;
        }

        if has_del_marker(&field.attrs)? {
            let buffer_field = syn::Ident::new(&format!("buffer_{field_name}"), field_name.span());
            let delegation_record_field = syn::Ident::new(
                &format!("delegation_record_{field_name}"),
                field_name.span(),
            );
            let delegate_method =
                syn::Ident::new(&format!("delegate_{field_name}"), field_name.span());
            let undelegate_method =
                syn::Ident::new(&format!("undelegate_{field_name}"), field_name.span());

            emitted_fields.push(quote! {
                /// CHECK: temporary owner-program buffer used while handing account ownership to Portal
                #[account(
                    mut,
                    seeds = [b"northstar-buffer", #field_name.key().as_ref()],
                    bump,
                    seeds::program = crate::id()
                )]
                pub #buffer_field: UncheckedAccount<'info>,
            });
            emitted_fields.push(quote! {
                /// CHECK: Portal delegation record PDA
                #[account(
                    mut,
                    seeds = [b"delegation", #field_name.key().as_ref()],
                    bump,
                    seeds::program = portal_program.key()
                )]
                pub #delegation_record_field: UncheckedAccount<'info>,
            });

            methods.push(quote! {
                pub fn #delegate_method(
                    &self,
                    payer: &anchor_lang::prelude::Signer<'info>,
                    seeds: &[&[u8]],
                    config: northstar_anchor::cpi::DelegateConfig,
                ) -> anchor_lang::solana_program::entrypoint::ProgramResult {
                    northstar_anchor::cpi::delegate_account(
                        northstar_anchor::cpi::DelegateAccounts {
                            payer: payer.to_account_info(),
                            pda: self.#field_name.to_account_info(),
                            owner_program: self.owner_program.to_account_info(),
                            buffer: self.#buffer_field.to_account_info(),
                            delegation_record: self.#delegation_record_field.to_account_info(),
                            portal_program: self.portal_program.to_account_info(),
                            session: self.session.to_account_info(),
                            system_program: self.system_program.to_account_info(),
                        },
                        seeds,
                        config,
                    )
                }

                pub fn #undelegate_method(
                    &self,
                    authority: &anchor_lang::prelude::Signer<'info>,
                    seeds: &[&[u8]],
                ) -> anchor_lang::solana_program::entrypoint::ProgramResult {
                    northstar_anchor::cpi::undelegate_account(
                        northstar_anchor::cpi::UndelegateAccounts {
                            authority: authority.to_account_info(),
                            pda: self.#field_name.to_account_info(),
                            owner_program: self.owner_program.to_account_info(),
                            buffer: self.#buffer_field.to_account_info(),
                            delegation_record: self.#delegation_record_field.to_account_info(),
                            portal_program: self.portal_program.to_account_info(),
                            session: self.session.to_account_info(),
                            system_program: self.system_program.to_account_info(),
                        },
                        seeds,
                    )
                }
            });
        }

        emitted_fields.push(quote! {
            #(#stripped_attrs)*
            #field_vis #field_name: #field_ty,
        });
    }

    if !has_owner_program {
        emitted_fields.push(quote! {
            /// CHECK: owner program of the delegated PDA
            #[account(address = crate::id())]
            pub owner_program: UncheckedAccount<'info>,
        });
    }
    if !has_portal_program {
        emitted_fields.push(quote! {
            /// CHECK: Northstar Portal program
            pub portal_program: UncheckedAccount<'info>,
        });
    }
    if !has_session {
        emitted_fields.push(quote! {
            /// CHECK: Northstar Portal session PDA
            #[account(
                seeds = [b"session"],
                bump,
                seeds::program = portal_program.key()
            )]
            pub session: UncheckedAccount<'info>,
        });
    }
    if !has_system_program {
        emitted_fields.push(quote! {
            pub system_program: Program<'info, System>,
        });
    }

    Ok(quote! {
        #(#struct_attrs)*
        #vis struct #struct_name #generics {
            #(#emitted_fields)*
        }

        impl #impl_generics #struct_name #ty_generics #where_clause {
            #(#methods)*
        }
    })
}

fn has_del_marker(attrs: &[Attribute]) -> syn::Result<bool> {
    Ok(attrs
        .iter()
        .filter(|attr| attr.path().is_ident("account"))
        .filter_map(account_attr_tokens)
        .any(|tokens| {
            tokens
                .split(',')
                .any(|token| token.split_whitespace().any(|part| part == "del"))
        }))
}

fn strip_del_marker(attrs: &[Attribute]) -> syn::Result<Vec<TokenStream2>> {
    let mut out = Vec::with_capacity(attrs.len());
    for attr in attrs {
        if !attr.path().is_ident("account") {
            out.push(attr.to_token_stream());
            continue;
        }

        let Some(tokens) = account_attr_tokens(attr) else {
            out.push(attr.to_token_stream());
            continue;
        };
        let stripped = tokens
            .split(',')
            .map(str::trim)
            .filter(|token| *token != "del")
            .collect::<Vec<_>>()
            .join(", ");
        out.push(syn::parse_str::<TokenStream2>(&format!(
            "#[account({stripped})]"
        ))?);
    }
    Ok(out)
}

fn account_attr_tokens(attr: &Attribute) -> Option<String> {
    match &attr.meta {
        Meta::List(list) => Some(list.tokens.to_string()),
        _ => None,
    }
}
