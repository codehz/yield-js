use std::collections::{HashMap, HashSet};

use proc_macro2::{Ident, Span, TokenStream};
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Block, Error, Expr, FnArg, GenericArgument, Pat, PatIdent, PathArguments, Signature, Stmt, Type,
};

pub(super) fn lower(signature: &Signature, block: &Block) -> syn::Result<TokenStream> {
    let Some(FnArg::Typed(first)) = signature.inputs.first() else {
        return Err(Error::new_spanned(
            signature,
            "expected a named CallCx parameter",
        ));
    };
    let cx = plain_ident(&first.pat)
        .ok_or_else(|| Error::new_spanned(&first.pat, "expected a named CallCx parameter"))?;
    let cx_name = name(&cx.ident);
    let mut bindings = Vec::new();
    for input in signature.inputs.iter().skip(1) {
        let FnArg::Typed(input) = input else {
            return Err(Error::new_spanned(
                input,
                "expected a named binding parameter",
            ));
        };
        let pat = plain_ident(&input.pat)
            .ok_or_else(|| Error::new_spanned(&input.pat, "expected a named binding parameter"))?;
        reject_cx(pat, &cx_name)?;
        bindings.push(Binding {
            pat,
            kind: this_class(&input.ty).map_or(Kind::Value, Kind::Receiver),
        });
    }

    let mut stages = Vec::new();
    let mut start = 0;
    let mut incoming = None;
    let mut scan = Scan::new(&cx_name, false);
    for (index, stmt) in block.stmts.iter().enumerate() {
        if let Some(call) = suspension(stmt, &cx_name)? {
            for arg in call.args {
                scan.visit_expr(arg);
            }
            scan.finish()?;
            stages.push(Stage {
                statements: &block.stmts[start..index],
                incoming,
                free: scan.free,
                defined: scan.root,
                call: Some(call),
                binding_count: bindings.len(),
            });
            start = index + 1;
            incoming = Some(call.output);
            bindings.push(Binding {
                pat: call.output,
                kind: Kind::Outcome,
            });
            scan = Scan::new(&cx_name, true);
        } else if let Stmt::Local(local) = stmt {
            scan.local_initializer(local);
            let introduced = scan.pattern(&local.pat);
            scan.root
                .extend(introduced.iter().map(|pat| name(&pat.ident)));
            bindings.extend(introduced.into_iter().map(|pat| Binding {
                pat,
                kind: Kind::Value,
            }));
        } else {
            scan.visit_stmt(stmt);
        }
    }
    scan.finish()?;
    stages.push(Stage {
        statements: &block.stmts[start..],
        incoming,
        free: scan.free,
        defined: scan.root,
        call: None,
        binding_count: bindings.len(),
    });

    let cx_pat = &first.pat;
    let cx_ident = &cx.ident;
    let output = Ident::new("__yield_outcome", Span::mixed_site());
    let callee = Ident::new("__yield_callee", Span::mixed_site());
    let receiver = Ident::new("__yield_receiver", Span::mixed_site());
    let args = Ident::new("__yield_args", Span::mixed_site());
    let resume = Ident::new("__yield_resume", Span::mixed_site());
    let saved_value = Ident::new("__yield_saved", Span::mixed_site());
    let mut needed = HashMap::new();
    let mut body = TokenStream::new();
    for (index, stage) in stages.into_iter().enumerate().rev() {
        let statements = stage.statements;
        body = if let Some(call) = stage.call {
            let mut saved = Vec::new();
            let mut restore = Vec::new();
            let mut live = Vec::new();
            let mut seen = HashSet::new();
            // Select only the most recent declaration of a needed name, then
            // emit in source declaration order (not hash iteration order).
            for binding in bindings[..stage.binding_count].iter().rev() {
                let key = name(&binding.pat.ident);
                if let Some(&use_span) = needed.get(&key)
                    && seen.insert(key)
                {
                    live.push((binding, use_span));
                }
            }
            for (binding, use_span) in live.into_iter().rev() {
                let ident = &binding.pat.ident;
                let mutable = binding.pat.mutability;
                match binding.kind {
                    Kind::Value => {
                        let position = saved.len();
                        let span = ident.span();
                        saved.push(quote_spanned!(span=> {
                            let #saved_value: ::yield_vm::JsValue = #ident;
                            #saved_value
                        }));
                        restore.push(quote!(let #mutable #ident = #cx_ident.arg(#position)?;));
                    }
                    Kind::Receiver(class) => {
                        restore.push(quote!(let #mutable #ident = #cx_ident.this::<#class>()?;));
                    }
                    Kind::Outcome => {
                        return Err(Error::new(
                            use_span,
                            "GuestOutcome cannot cross another await; extract the owned JsValue first",
                        ));
                    }
                }
            }
            let [callee_expr, receiver_expr, args_expr] = call.args;
            let outcome_pat = call.output;
            // Keep user statements in a separate lexical block, so that even
            // local items (which closures need not capture) cannot leak past it.
            quote!({
                let #resume: fn(
                    &mut ::yield_vm::binding::CallCx<'_>,
                    ::yield_vm::GuestOutcome,
                ) -> ::yield_vm::JsResult<::yield_vm::HostStep> = |#cx_pat, #output| {
                    #(#restore)*
                    let #outcome_pat = #output;
                    #body
                };
                {
                    #(#statements)*
                    let #callee = #callee_expr;
                    let #receiver = #receiver_expr;
                    let #args = #args_expr;
                    #cx_ident.request_call(#callee, #receiver, #args, ::std::vec![#(#saved),*], #resume)
                }
            })
        } else if index == 0 {
            quote!(#block)
        } else {
            quote!({ #(#statements)* })
        };

        // Unsatisfied names pass through to the preceding boundary unless
        // this stage supplies a declaration. Initializer uses are already
        // represented in `free` and are added after removing those names.
        for defined in stage.defined {
            needed.remove(&defined);
        }
        needed.extend(stage.free);
        if let Some(outcome) = stage.incoming {
            needed.remove(&name(&outcome.ident));
        }
    }
    Ok(body)
}

struct Stage<'a> {
    statements: &'a [Stmt],
    incoming: Option<&'a PatIdent>,
    free: HashMap<String, Span>,
    defined: HashSet<String>,
    call: Option<Suspension<'a>>,
    binding_count: usize,
}

struct Binding<'a> {
    pat: &'a PatIdent,
    kind: Kind<'a>,
}

enum Kind<'a> {
    Value,
    Receiver(&'a Type),
    Outcome,
}

#[derive(Clone, Copy)]
struct Suspension<'a> {
    args: [&'a Expr; 3],
    output: &'a PatIdent,
}

fn suspension<'a>(stmt: &'a Stmt, cx: &str) -> syn::Result<Option<Suspension<'a>>> {
    let Stmt::Local(local) = stmt else {
        return Ok(None);
    };
    let Some(init) = &local.init else {
        return Ok(None);
    };
    let Expr::Await(awaited) = &*init.expr else {
        return Ok(None);
    };
    let Expr::MethodCall(call) = &*awaited.base else {
        return Err(Error::new_spanned(
            awaited,
            "expected `.await` on a call to the CallCx parameter's `.call` method",
        ));
    };
    if call.method != "call" || !is_cx(&call.receiver, cx) || call.turbofish.is_some() {
        return Err(Error::new_spanned(
            awaited,
            "expected `.await` on a call to the CallCx parameter's `.call` method",
        ));
    }
    let mut args = call.args.iter();
    let (Some(callee), Some(receiver), Some(argv), None) =
        (args.next(), args.next(), args.next(), args.next())
    else {
        return Err(Error::new_spanned(
            call,
            "`cx.call` requires exactly three arguments: callee, receiver, and args",
        ));
    };
    let Some(output) = plain_ident(&local.pat) else {
        return Err(Error::new_spanned(
            &local.pat,
            "await must bind a plain identifier (optionally `mut`)",
        ));
    };
    if init.diverge.is_some()
        || !local.attrs.is_empty()
        || !awaited.attrs.is_empty()
        || !call.attrs.is_empty()
    {
        return Err(Error::new_spanned(
            local,
            "await bindings cannot have attributes or a let-else clause",
        ));
    }
    reject_cx(output, cx)?;
    Ok(Some(Suspension {
        args: [callee, receiver, argv],
        output,
    }))
}

fn plain_ident(pat: &Pat) -> Option<&PatIdent> {
    match pat {
        Pat::Ident(pat) if pat.by_ref.is_none() && pat.subpat.is_none() && pat.attrs.is_empty() => {
            Some(pat)
        }
        _ => None,
    }
}

fn name(ident: &Ident) -> String {
    let spelling = ident.to_string();
    spelling
        .strip_prefix("r#")
        .map(str::to_owned)
        .unwrap_or(spelling)
}

fn reject_cx(pat: &PatIdent, cx: &str) -> syn::Result<()> {
    if name(&pat.ident) == cx {
        Err(Error::new_spanned(
            &pat.ident,
            "cannot redefine the CallCx parameter in a CPS function",
        ))
    } else {
        Ok(())
    }
}

fn value_path(expr: &Expr) -> Option<&Ident> {
    let Expr::Path(path) = expr else { return None };
    if path.qself.is_none() {
        path.path.get_ident()
    } else {
        None
    }
}

fn ungroup(mut expr: &Expr) -> &Expr {
    loop {
        expr = match expr {
            Expr::Group(group) => &group.expr,
            Expr::Paren(paren) => &paren.expr,
            _ => return expr,
        };
    }
}

fn is_cx(expr: &Expr, cx: &str) -> bool {
    value_path(expr).is_some_and(|ident| name(ident) == cx)
}

fn this_class(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    if path.qself.is_none()
        && segment.ident == "This"
        && arguments.args.len() == 1
        && let Some(GenericArgument::Type(class)) = arguments.args.first()
    {
        return Some(class);
    }
    None
}

// Analyze one stage in source order. `root` is kept separate from nested
// scopes so only top-level lets kill a later stage's pass-through name.
struct Scan<'a> {
    cx: &'a str,
    continuation: bool,
    error: Option<Error>,
    root: HashSet<String>,
    scopes: Vec<HashSet<String>>,
    free: HashMap<String, Span>,
    item_depth: usize,
}

impl<'a> Scan<'a> {
    fn new(cx: &'a str, continuation: bool) -> Self {
        Self {
            cx,
            continuation,
            error: None,
            root: HashSet::new(),
            scopes: Vec::new(),
            free: HashMap::new(),
            item_depth: 0,
        }
    }

    fn finish(&mut self) -> syn::Result<()> {
        self.error.take().map_or(Ok(()), Err)
    }

    fn fail(&mut self, error: Error) {
        if let Some(previous) = &mut self.error {
            previous.combine(error);
        } else {
            self.error = Some(error);
        }
    }

    fn scope(&mut self, visit: impl FnOnce(&mut Self)) {
        self.scopes.push(HashSet::new());
        visit(self);
        self.scopes.pop();
    }

    fn local_initializer(&mut self, local: &syn::Local) {
        if let Some(init) = &local.init {
            self.visit_expr(&init.expr);
            if let Some((_, diverge)) = &init.diverge {
                self.visit_expr(diverge);
            }
        }
    }

    fn pattern<'ast>(&mut self, pat: &'ast Pat) -> Vec<&'ast PatIdent> {
        struct Bindings<'ast>(Vec<&'ast PatIdent>);
        impl<'ast> Visit<'ast> for Bindings<'ast> {
            fn visit_pat_ident(&mut self, pat: &'ast PatIdent) {
                self.0.push(pat);
                visit::visit_pat_ident(self, pat);
            }

            fn visit_block(&mut self, _: &'ast Block) {
                // A const pattern can contain its own unrelated local lets.
            }
        }
        self.visit_pat(pat);
        let mut bindings = Bindings(Vec::new());
        bindings.visit_pat(pat);
        bindings.0
    }

    fn bind(&mut self, pat: &Pat) {
        let bindings = self.pattern(pat);
        let scope = self.scopes.last_mut().expect("nested lexical scope");
        scope.extend(bindings.into_iter().map(|pat| name(&pat.ident)));
    }

    fn condition(&mut self, expr: &Expr) {
        match ungroup(expr) {
            Expr::Let(expr) => {
                self.visit_expr(&expr.expr);
                self.bind(&expr.pat);
            }
            Expr::Binary(expr) if matches!(expr.op, syn::BinOp::And(_)) => {
                self.condition(&expr.left);
                self.condition(&expr.right);
            }
            expr => self.visit_expr(expr),
        }
    }
}

impl<'ast> Visit<'ast> for Scan<'_> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if self.item_depth == 0
            && let Some(ident) = value_path(expr)
        {
            let name = name(ident);
            if name != self.cx
                && !self.root.contains(&name)
                && !self.scopes.iter().rev().any(|scope| scope.contains(&name))
            {
                self.free.entry(name).or_insert_with(|| ident.span());
            }
        }
        visit::visit_expr(self, expr);
    }

    fn visit_expr_await(&mut self, expr: &'ast syn::ExprAwait) {
        self.fail(Error::new(expr.span(), "await is only supported as `let name = cx.call(callee, receiver, args).await;` at the top level"));
    }

    fn visit_expr_async(&mut self, expr: &'ast syn::ExprAsync) {
        self.fail(Error::new(
            expr.span(),
            "nested async blocks are not supported in CPS functions",
        ));
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if self.continuation {
            self.fail(Error::new_spanned(
                mac,
                "macro calls are not supported after await; move this call to a synchronous helper",
            ));
        }
    }

    fn visit_pat_ident(&mut self, pat: &'ast PatIdent) {
        if self.item_depth == 0
            && let Err(error) = reject_cx(pat, self.cx)
        {
            self.fail(error);
        }
        visit::visit_pat_ident(self, pat);
    }

    fn visit_item(&mut self, item: &'ast syn::Item) {
        self.item_depth += 1;
        visit::visit_item(self, item);
        self.item_depth -= 1;
    }

    fn visit_block(&mut self, block: &'ast Block) {
        self.scope(|scan| visit::visit_block(scan, block));
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        self.local_initializer(local);
        self.bind(&local.pat);
    }

    fn visit_expr_if(&mut self, expr: &'ast syn::ExprIf) {
        self.scope(|scan| {
            scan.condition(&expr.cond);
            scan.visit_block(&expr.then_branch);
        });
        if let Some((_, branch)) = &expr.else_branch {
            self.visit_expr(branch);
        }
    }

    fn visit_expr_while(&mut self, expr: &'ast syn::ExprWhile) {
        self.scope(|scan| {
            scan.condition(&expr.cond);
            scan.visit_block(&expr.body);
        });
    }

    fn visit_expr_for_loop(&mut self, expr: &'ast syn::ExprForLoop) {
        self.visit_expr(&expr.expr);
        self.scope(|scan| {
            scan.bind(&expr.pat);
            scan.visit_block(&expr.body);
        });
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        self.scope(|scan| {
            scan.bind(&arm.pat);
            if let Some((_, guard)) = &arm.guard {
                scan.condition(guard);
            }
            scan.visit_expr(&arm.body);
        });
    }

    fn visit_expr_closure(&mut self, expr: &'ast syn::ExprClosure) {
        if expr.asyncness.is_some() {
            self.fail(Error::new(
                expr.span(),
                "nested async closures are not supported in CPS functions",
            ));
            return;
        }
        self.scope(|scan| {
            for input in &expr.inputs {
                scan.bind(input);
            }
            scan.visit_return_type(&expr.output);
            scan.visit_expr(&expr.body);
        });
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if self.continuation
            && self.item_depth == 0
            && (call.method == "args" || call.method == "arg")
            && is_cx(ungroup(&call.receiver), self.cx)
        {
            self.fail(Error::new_spanned(
                call,
                "use named parameters instead of reading CallCx arguments after await",
            ));
        }
        visit::visit_expr_method_call(self, call);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand(block: Block) -> syn::Result<String> {
        let signature = syn::parse_quote!(
            async fn callback(ctx: &mut CallCx<'_>, keep: JsValue, dead: JsValue)
                -> JsResult<HostStep>
        );
        let tokens = lower(&signature, &block)?;
        syn::parse2::<Block>(tokens.clone())?;
        Ok(tokens.to_string())
    }

    #[test]
    fn forwards_live_bindings_across_each_boundary() {
        let body = expand(syn::parse_quote!({
            drop(dead);
            let first = ctx.call(callee(), receiver(), arguments()).await;
            consume(first);
            let second = ctx.call(callee(), receiver(), arguments()).await;
            finish(ctx, second, keep)
        }))
        .unwrap();
        assert_eq!(body.matches("let keep = ctx . arg").count(), 2);
        assert!(!body.contains("let dead = ctx . arg"));
    }

    #[test]
    fn nested_and_top_level_shadowing_do_not_save_original() {
        let body = expand(syn::parse_quote!({
            let first = ctx.call(callee(), receiver(), arguments()).await;
            let closure = |keep| consume(keep);
            for keep in entries() {
                closure(keep);
            }
            let keep = make_value(first);
            let second = ctx.call(callee(), receiver(), arguments()).await;
            finish(ctx, second, keep)
        }))
        .unwrap();
        assert_eq!(body.matches("let keep = ctx . arg").count(), 1);
    }

    #[test]
    fn rebinding_keeps_initializer_uses_live() {
        let body = expand(syn::parse_quote!({
            let first = ctx.call(callee(), receiver(), arguments()).await;
            let keep = update(keep, first);
            finish(ctx, keep)
        }))
        .unwrap();
        assert_eq!(body.matches("let keep = ctx . arg").count(), 1);
    }

    #[test]
    fn rebuilds_typed_receiver_without_saving_it() {
        let signature = syn::parse_quote!(
            async fn callback(cx: &mut CallCx<'_>, this: This<Self>) -> JsResult<HostStep>
        );
        let block = syn::parse_quote!({
            let outcome = cx.call(callee(), receiver(), arguments()).await;
            finish(cx, this, outcome)
        });
        let body = lower(&signature, &block).unwrap().to_string();
        assert!(body.contains("let this = cx . this :: < Self >"));
        assert!(!body.contains("__yield_saved"));
    }

    #[test]
    fn rejects_outcomes_that_need_to_cross_another_boundary() {
        let error = expand(syn::parse_quote!({
            let first = ctx.call(callee(), receiver(), arguments()).await;
            let second = ctx.call(callee(), receiver(), arguments()).await;
            finish(ctx, first, second)
        }))
        .unwrap_err();
        assert!(error.to_string().contains("GuestOutcome cannot cross"));
    }

    #[test]
    fn rejects_invalid_suspensions_and_continuation_constructs() {
        let cases: [(Block, &str); 6] = [
            (
                syn::parse_quote!({
                    if ready() {
                        let value = ctx.call(a, b, c).await;
                    }
                }),
                "top level",
            ),
            (
                syn::parse_quote!({
                    let value = other.call(a, b, c).await;
                }),
                "CallCx parameter",
            ),
            (
                syn::parse_quote!({
                    let value = ctx.call(a, b).await;
                }),
                "exactly three",
            ),
            (
                syn::parse_quote!({
                    let (value, _) = ctx.call(a, b, c).await;
                }),
                "plain identifier",
            ),
            (
                syn::parse_quote!({
                    let value = ctx.call(a, b, c).await;
                    inspect!(value);
                }),
                "macro calls",
            ),
            (
                syn::parse_quote!({
                    let value = ctx.call(a, b, c).await;
                    ctx.arg(0)
                }),
                "named parameters",
            ),
        ];
        for (block, expected) in cases {
            assert!(expand(block).unwrap_err().to_string().contains(expected));
        }
    }
}
