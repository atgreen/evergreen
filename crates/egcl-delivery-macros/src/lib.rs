use proc_macro::TokenStream;
use quote::quote;
use syn::{Expr, Ident, ItemFn, Lit, LitStr, Pat, parse_macro_input, visit_mut::VisitMut};

/// Remove unselected builtin match arms before type checking and code generation.
/// The fallback remains intact, and aliases share one implementation arm.
#[proc_macro_attribute]
pub fn builtin_dispatch(attribute: TokenStream, input: TokenStream) -> TokenStream {
    let parameter = parse_macro_input!(attribute as Ident);
    let mut function = parse_macro_input!(input as ItemFn);
    let mut visitor = Dispatch {
        parameter,
        found: false,
        error: None,
    };
    visitor.visit_item_fn_mut(&mut function);
    if let Some(error) = visitor.error {
        return error.into_compile_error().into();
    }
    if !visitor.found {
        return syn::Error::new_spanned(
            function,
            "builtin dispatcher has no matching match expression",
        )
        .into_compile_error()
        .into();
    }
    quote!(#function).into()
}

struct Dispatch {
    parameter: Ident,
    found: bool,
    error: Option<syn::Error>,
}

impl VisitMut for Dispatch {
    fn visit_expr_match_mut(&mut self, expression: &mut syn::ExprMatch) {
        if !matches!(&*expression.expr, Expr::Path(path) if path.path.is_ident(&self.parameter)) {
            syn::visit_mut::visit_expr_match_mut(self, expression);
            return;
        }
        self.found = true;
        for arm in &mut expression.arms {
            let mut names = Vec::new();
            if let Err(error) = builtin_names(&arm.pat, &mut names) {
                self.error = Some(error);
                return;
            }
            if !names.is_empty() {
                arm.attrs.push(syn::parse_quote!(
                    #[cfg(any(not(egcl_specialized_builtins), #(egcl_builtin = #names),*))]
                ));
            }
        }
        // Inner matches implement the selected operation and are not dispatchers.
    }
}

fn builtin_names(pattern: &Pat, names: &mut Vec<LitStr>) -> syn::Result<()> {
    match pattern {
        Pat::Lit(literal) => match &literal.lit {
            Lit::Str(name) => names.push(name.clone()),
            _ => {
                return Err(syn::Error::new_spanned(
                    pattern,
                    "builtin name must be a string",
                ));
            }
        },
        Pat::Or(alternatives) => {
            for alternative in &alternatives.cases {
                builtin_names(alternative, names)?;
            }
        }
        Pat::Paren(group) => builtin_names(&group.pat, names)?,
        Pat::Ident(binding) => {
            if let Some((_, pattern)) = &binding.subpat {
                builtin_names(pattern, names)?;
            }
        }
        Pat::Wild(_) => {}
        _ => {
            return Err(syn::Error::new_spanned(
                pattern,
                "unsupported builtin dispatch pattern",
            ));
        }
    }
    Ok(())
}
