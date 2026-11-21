use std::collections::{HashMap, HashSet, VecDeque};

use proc_macro2::{Ident, Span, TokenStream};
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Block, Error, Expr, FnArg, GenericArgument, Pat, PatIdent, PathArguments, Signature, Stmt, Type,
};

pub(super) fn lower(signature: &Signature, block: &Block) -> syn::Result<TokenStream> {
    lower_impl(signature, &[], block, None, false).map(|(body, _)| body)
}

pub(super) fn lower_method(
    signature: &Signature,
    block: &Block,
    class: &Type,
) -> syn::Result<TokenStream> {
    lower_impl(signature, &[], block, Some(class), false).map(|(body, _)| body)
}

pub(super) fn lower_spawn(
    signature: &Signature,
    prefix: &[Stmt],
    background: &Block,
    class: Option<&Type>,
) -> syn::Result<(TokenStream, TokenStream)> {
    lower_impl(signature, prefix, background, class, true)
}

// Each node runs one ordinary Rust fragment. Synchronous edges move live
// locals through inferred Rust slots; only real suspensions serialize them.
// Resume edges enter the same dispatch graph, so each join has one body.
fn lower_impl<'a>(
    signature: &'a Signature,
    prefix: &'a [Stmt],
    block: &'a Block,
    class: Option<&Type>,
    spawn: bool,
) -> syn::Result<(TokenStream, TokenStream)> {
    let Some(FnArg::Typed(first)) = signature.inputs.first() else {
        return Err(Error::new_spanned(
            signature,
            "expected a named CallCx parameter",
        ));
    };
    let cx = plain_ident(&first.pat)
        .ok_or_else(|| Error::new_spanned(&first.pat, "expected a named CallCx parameter"))?;
    let cx_ident = &cx.ident;
    let cx_name = name(cx_ident);
    let mut graph = Graph {
        cx: &cx_name,
        spawn,
        bindings: Vec::new(),
        nodes: Vec::new(),
        boundaries: Vec::new(),
        loops: Vec::new(),
    };
    let entry = graph.node();
    let mut env = Env::new();
    let mut args = Vec::new();
    let mut arg_types = Vec::new();
    let mut arg_pats = Vec::new();
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
        graph.bind(
            &mut env,
            pat,
            this_class(&input.ty).map_or(Kind::Value, Kind::Receiver),
        );
        args.push(&pat.ident);
        arg_pats.push(&input.pat);
        arg_types.push(&input.ty);
    }
    let mut prefix_scan = Scan::new(&cx_name);
    for stmt in prefix {
        if let Stmt::Local(local) = stmt {
            prefix_scan.local_initializer(local);
            for pat in prefix_scan.pattern(&local.pat) {
                graph.bind(&mut env, pat, Kind::Value);
            }
        } else {
            prefix_scan.visit_stmt(stmt);
        }
    }
    prefix_scan.finish()?;
    graph.sequence(&block.stmts, env, entry, Dest::Finish)?;
    if !spawn && graph.boundaries.is_empty() {
        return Ok((quote!(#block), quote!(&[])));
    }
    let live = graph.liveness()?;
    let r = Render {
        graph: &graph,
        live: &live,
        cx: cx_ident,
        runner: generated("run", 0),
        state: generated("state", 0),
        pending: generated("pending", 0),
    };
    let runner = &r.runner;
    let stage = generated("stage", 0);
    let inputs = generated("inputs", 0);
    let outcome = generated("outcome", 0);
    let entered = generated("entered", 0);
    let value = generated("prefix_result", 0);
    let state = &r.state;
    let pending = &r.pending;
    let cx_pat = &first.pat;
    let step = quote!(::yield_vm::HostStep);
    let context = quote!(&mut ::yield_vm::binding::CallCx<'_>);
    let guest = quote!(::yield_vm::GuestOutcome);
    let mut used_slots = std::collections::BTreeSet::new();
    for node_live in &live {
        used_slots.extend(node_live.keys().copied());
    }
    let saved_slots: HashSet<_> = graph
        .boundaries
        .iter()
        .flat_map(|boundary| r.saved(boundary).into_iter().map(|(id, _)| id))
        .collect();
    let declarations = used_slots.into_iter().map(|id| {
        let slot = r.slot(id);
        let ty = match graph.bindings[id].kind {
            Kind::Receiver(class) => Some(quote!(::yield_vm::binding::This<#class>)),
            Kind::JsValue => Some(quote!(::yield_vm::JsValue)),
            Kind::Outcome => Some(guest.clone()),
            Kind::Value => arg_types.get(id).map(|ty| quote!(#ty)).or_else(|| {
                saved_slots
                    .contains(&id)
                    .then(|| quote!(::yield_vm::JsValue))
            }),
        };
        let annotation = ty.map(|ty| quote!(: ::core::option::Option<#ty>));
        quote!(let mut #slot #annotation = ::core::option::Option::None;)
    });
    let entry_stores = live[entry].keys().map(|&id| r.store(id));
    let initialize = if spawn {
        quote! {
            let #value = (|| -> ::yield_vm::JsResult<::yield_vm::JsValue> {
                #(#prefix)*
                #(#entry_stores)*
                #entered = true;
                Ok(::yield_vm::JsValue::undefined())
            })()?;
            if !#entered { return Ok(#step::Return(#value)); }
        }
    } else {
        quote!(#(#entry_stores)*)
    };
    let restore = graph
        .boundaries
        .iter()
        .enumerate()
        .map(|(index, boundary)| {
            let mut position = 0usize;
            let mut restore = Vec::new();
            for (id, _) in r.saved(boundary) {
                let slot = r.slot(id);
                let rhs = match graph.bindings[id].kind {
                    Kind::Receiver(class) => quote!(#cx_ident.this::<#class>()?),
                    _ => {
                        let i = position;
                        position += 1;
                        quote!(#cx_ident.arg(#i)?)
                    }
                };
                restore.push(quote!(#slot = Some(#rhs);));
            }
            let result = if boundary.checked {
                quote!(#cx_ident.__outcome(#outcome)?)
            } else {
                quote!(#outcome)
            };
            let output = if let Some(id) = boundary
                .output
                .filter(|id| live[boundary.resume].contains_key(id))
            {
                let slot = r.slot(id);
                quote!(#slot = Some(#result);)
            } else if boundary.checked {
                quote!(let _ = #result;)
            } else {
                TokenStream::new()
            };
            let next = boundary.resume;
            quote!(#index => { #(#restore)* #output #next })
        });
    let arms = (0..graph.nodes.len()).map(|id| r.node(id));
    let dispatch_label = dispatch_label();
    let dispatch = quote! { #dispatch_label: loop { match #state { #(#arms,)* _ => unreachable!("invalid state-machine node"), } } };
    let run = if spawn {
        quote! {
            let mut #pending = None;
            (|| -> ::yield_vm::JsResult<()> { #dispatch })()?;
            let #value = match #pending {
                Some(step) => step,
                None => #step::Return(::yield_vm::JsValue::undefined()),
            };
            if #entered { #cx_ident.__spawn(#value) } else { Ok(#value) }
        }
    } else {
        dispatch
    };
    let helper = quote! {
        #[allow(unused_mut, unused_variables, unused_labels, unreachable_code)]
        fn #runner(#cx_pat: #context, #inputs: ::core::option::Option<(#(#arg_types,)*)>, #stage: usize, #outcome: #guest) -> ::yield_vm::JsResult<#step> {
            #(#declarations)*
            let mut #entered = false;
            let mut #state = if let Some((#(#arg_pats,)*)) = #inputs {
                #initialize
                #entry
            } else {
                match #stage { #(#restore,)* _ => unreachable!("invalid CPS stage"), }
            };
            #run
        }
    };
    let call = quote!(#runner(#cx_ident, Some((#(#args,)*)), 0, #guest::Returned(::yield_vm::JsValue::undefined())));
    let stages = graph
        .boundaries
        .iter()
        .enumerate()
        .map(|(index, boundary)| {
            let saved = r.saved(boundary);
            let slots = saved
                .iter()
                .filter(|(id, _)| !matches!(graph.bindings[*id].kind, Kind::Receiver(_)))
                .map(|&(id, _)| {
                    graph.bindings[id]
                        .ordinal
                        .expect("only source bindings cross a suspension")
                });
            let receiver = saved
                .iter()
                .find_map(|&(id, _)| match graph.bindings[id].kind {
                    Kind::Receiver(class) => {
                        Some(quote!(Some(<#class as ::yield_vm::HostClass>::TAG)))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| quote!(None));
            let kind = Ident::new(boundary.kind, Span::mixed_site());
            quote!(::yield_vm::__macro_support::CpsStage {
                resume: |cx, outcome| #runner(cx, None, #index, outcome),
                kind: ::yield_vm::__macro_support::CpsBoundary::#kind,
                slots: &[#(#slots),*],
                receiver: #receiver,
            })
        });
    let descriptor = if spawn {
        let stages_ident = generated("stages", 0);
        quote!({ #helper const #stages_ident: &[::yield_vm::__macro_support::CpsStage] = &[#(#stages),*]; #stages_ident })
    } else {
        quote!(&[])
    };
    Ok((
        replace_self(quote!({ #helper #call }), class),
        replace_self(descriptor, class),
    ))
}

fn replace_self(tokens: TokenStream, class: Option<&Type>) -> TokenStream {
    use proc_macro2::{Group, TokenTree};
    let Some(class) = class else { return tokens };
    let mut result = TokenStream::new();
    let mut tokens = tokens.into_iter().peekable();
    while let Some(token) = tokens.next() {
        match token {
            TokenTree::Group(group) => {
                let mut nested =
                    Group::new(group.delimiter(), replace_self(group.stream(), Some(class)));
                nested.set_span(group.span());
                result.extend([TokenTree::Group(nested)]);
            }
            TokenTree::Ident(ident) if ident == "Self" => {
                if matches!(tokens.peek(), Some(TokenTree::Punct(p)) if p.as_char() == ':') {
                    result.extend(quote_spanned!(ident.span()=> <#class>));
                } else {
                    result.extend(quote_spanned!(ident.span()=> #class));
                }
            }
            token => result.extend([token]),
        }
    }
    result
}

struct Render<'a, 'ast> {
    graph: &'a Graph<'ast>,
    live: &'a [Live],
    cx: &'a Ident,
    runner: Ident,
    state: Ident,
    pending: Ident,
}

impl Render<'_, '_> {
    fn slot(&self, id: BindingId) -> Ident {
        generated("slot", id)
    }

    fn take(&self, id: BindingId) -> TokenStream {
        let slot = self.slot(id);
        quote!(#slot.take().expect("live CPS binding"))
    }

    fn store(&self, id: BindingId) -> TokenStream {
        let ident = &self.graph.bindings[id].ident;
        let slot = self.slot(id);
        quote!(#slot = Some(#ident);)
    }

    fn saved(&self, boundary: &Boundary) -> Vec<(BindingId, Span)> {
        self.live[boundary.resume]
            .iter()
            .filter_map(|(&id, &span)| (Some(id) != boundary.output).then_some((id, span)))
            .collect()
    }

    fn edge(
        &self,
        to: NodeId,
        bindings: &[BindingId],
        active: &[BindingId],
        renames: &Renames,
    ) -> TokenStream {
        let state = &self.state;
        let stores = self.live[to].keys().filter_map(|&id| {
            if bindings.contains(&id) {
                let slot = self.slot(id);
                let orig = &self.graph.bindings[id].ident;
                let ident = renames.get(&name(orig)).unwrap_or(orig);
                Some(quote!(#slot = Some(#ident);))
            } else {
                active.contains(&id).then(|| self.store(id))
            }
        });
        quote!({ #(#stores)* #state = #to; })
    }

    fn renames(&self, bindings: &[BindingId], active: &[BindingId]) -> Renames {
        bindings
            .iter()
            .filter_map(|&id| {
                let ident = &self.graph.bindings[id].ident;
                active
                    .iter()
                    .any(|&outer| self.graph.bindings[outer].ident == *ident)
                    .then(|| (name(ident), generated("pattern", id)))
            })
            .collect()
    }

    fn node(&self, id: NodeId) -> TokenStream {
        let node = &self.graph.nodes[id];
        let code = &node.code;
        let active: Vec<_> = node
            .uses
            .keys()
            .copied()
            .chain(node.definitions.iter().copied())
            .collect();
        let load = node.uses.keys().map(|&id| {
            let binding = &self.graph.bindings[id];
            let ident = &binding.ident;
            let mutable = binding.mutable.then(|| quote!(mut));
            let take = self.take(id);
            quote!(let #mutable #ident = #take;)
        });
        let exit = match node.end.as_ref().expect("complete state graph") {
            End::Next(to) => {
                let renames = self.renames(&node.shadow, &active);
                self.edge(*to, &node.shadow, &active, &renames)
            }
            End::Finish(expr) => expr.clone(),
            End::If(cond, yes, no) => {
                let renames = self.renames(&yes.bindings, &active);
                let cond = Rename::default().condition(cond.as_ref().clone(), &renames);
                let yes = self.edge(yes.to, &yes.bindings, &active, &renames);
                let no = self.edge(no.to, &no.bindings, &active, &Renames::new());
                quote!(if #cond #yes else #no)
            }
            End::Match(expr, arms) => {
                let arms = arms.iter().map(|(arm, guard, edge)| {
                    let attrs = &arm.attrs;
                    let renames = self.renames(&edge.bindings, &active);
                    let mut rename = Rename::default();
                    let pat = rename.pattern(arm.pat.clone(), &renames);
                    let guard = guard.as_ref().map(|guard| {
                        let guard = rename.condition(guard.as_ref().clone(), &renames);
                        quote!(if #guard)
                    });
                    let edge = self.edge(edge.to, &edge.bindings, &active, &renames);
                    quote!(#(#attrs)* #pat #guard => #edge)
                });
                quote!(match #expr { #(#arms,)* })
            }
            End::Eval(expr, edge) => {
                let result = edge
                    .bindings
                    .first()
                    .map(|&id| &self.graph.bindings[id].ident);
                let eval = if let Some(result) = result {
                    quote!(let #result = #expr;)
                } else {
                    quote!(let _ = #expr;)
                };
                let edge = self.edge(edge.to, &edge.bindings, &active, &Renames::new());
                quote!(#eval #edge)
            }
            End::Await(stage, effect) => self.request(*stage, effect, &active),
        };
        let exit = if let Some(escapes) = &node.escapes {
            let (declaration, arms) = escapes.render(&self.graph.bindings);
            let label = &escapes.label;
            let dispatch = dispatch_label();
            let arms = arms.iter().map(|(pat, edge)| {
                let edge = self.edge(edge.to, &edge.bindings, &active, &Renames::new());
                quote!(#pat => #edge)
            });
            // A normal branch commits its edge and leaves immediately; only
            // the escape paths still have their locals available here.
            quote! {
                #declaration
                match #label: { #exit continue #dispatch; } { #(#arms,)* }
            }
        } else {
            exit
        };
        quote!(#id => { #(#load)* #code #exit })
    }

    fn request(&self, stage: usize, effect: &Effect<'_>, active: &[BindingId]) -> TokenStream {
        let cx = self.cx;
        let saved = self.saved(&self.graph.boundaries[stage]).into_iter().filter_map(|(id, _)| {
            let binding = &self.graph.bindings[id];
            if !matches!(binding.kind, Kind::Value | Kind::JsValue) { return None; }
            let ident = &binding.ident;
            let value = if active.contains(&id) { quote!(#ident) } else { self.take(id) };
            let slot = generated("saved", id);
            Some(quote_spanned!(ident.span()=> { let #slot: ::yield_vm::JsValue = #value; #slot }))
        });
        let saved = quote!(::std::vec![#(#saved),*]);
        let runner = &self.runner;
        let linked = self.graph.spawn;
        let index = stage as u32;
        let resume = quote!(|cx, outcome| #runner(cx, None, #stage, outcome));
        let request = match effect {
            Effect::Call(arguments) => {
                let [callee, receiver, args] = arguments;
                let f = generated("callee", stage);
                let this = generated("receiver", stage);
                let argv = generated("args", stage);
                let call = if linked {
                    quote!(#cx.__linked_call(#f, #this, #argv, #saved, #index))
                } else {
                    quote!(#cx.request_call(#f, #this, #argv, #saved, #resume))
                };
                quote!({ let #f = #callee; let #this = #receiver; let #argv = #args; #call })
            }
            Effect::Gc if linked => quote!(#cx.__linked_gc(#saved, #index)),
            Effect::Gc => quote!(#cx.request_gc(#saved, #resume)),
            Effect::External(op) => {
                let state = generated("operation", stage);
                quote!({ let #state = #op; #cx.__linked_effect(#state, #saved, #index) })
            }
        };
        if linked {
            let pending = &self.pending;
            quote!(#pending = Some(#request?); return Ok(());)
        } else {
            quote!(return #request;)
        }
    }
}

type BindingId = usize;
type NodeId = usize;
type Env = HashMap<String, BindingId>;
type Live = std::collections::BTreeMap<BindingId, Span>;

struct Binding<'a> {
    ident: Ident,
    mutable: bool,
    kind: Kind<'a>,
    ordinal: Option<u32>,
}

#[derive(Clone, Copy)]
enum Kind<'a> {
    Value,
    JsValue,
    Receiver(&'a Type),
    Outcome,
}

enum Effect<'a> {
    Call([Fragment<'a>; 3]),
    Gc,
    External(Fragment<'a>),
}

enum Fragment<'a> {
    Source(&'a Expr),
    Rewritten(Box<Expr>),
}

impl AsRef<Expr> for Fragment<'_> {
    fn as_ref(&self) -> &Expr {
        match self {
            Self::Source(expr) => expr,
            Self::Rewritten(expr) => expr,
        }
    }
}

impl quote::ToTokens for Fragment<'_> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        self.as_ref().to_tokens(tokens);
    }
}

struct Boundary {
    resume: NodeId,
    output: Option<BindingId>,
    checked: bool,
    kind: &'static str,
}

#[derive(Default)]
struct Node<'a> {
    code: TokenStream,
    uses: Live,
    definitions: Vec<BindingId>,
    shadow: Vec<BindingId>,
    deferred: Option<Error>,
    escapes: Option<Escapes>,
    end: Option<End<'a>>,
}
fn pattern_bindings(pat: &Pat) -> Vec<&PatIdent> {
    struct Bindings<'ast>(Vec<&'ast PatIdent>);
    impl<'ast> Visit<'ast> for Bindings<'ast> {
        fn visit_pat_ident(&mut self, pat: &'ast PatIdent) {
            self.0.push(pat);
            visit::visit_pat_ident(self, pat);
        }

        fn visit_block(&mut self, _: &'ast Block) {
            // Const patterns may contain unrelated declarations.
        }
    }
    let mut bindings = Bindings(Vec::new());
    bindings.visit_pat(pat);
    let mut seen = HashSet::new();
    bindings.0.retain(|pat| seen.insert(name(&pat.ident)));
    bindings.0
}

type Renames = HashMap<String, Ident>;

// A selector uses Rust's original pattern and guard semantics, but its
// bindings need hygienic names while we save an outer binding of the same
// name into another slot. The arm's node reintroduces the source names.
#[derive(Default)]
struct Rename(Renames);

impl Rename {
    fn pattern(&mut self, pat: Pat, renames: &Renames) -> Pat {
        use syn::fold::Fold;
        struct Pattern<'a>(&'a Renames);
        impl Fold for Pattern<'_> {
            fn fold_pat_ident(&mut self, mut pat: PatIdent) -> PatIdent {
                if let Some(ident) = self.0.get(&name(&pat.ident)) {
                    pat.ident = ident.clone();
                }
                syn::fold::fold_pat_ident(self, pat)
            }

            fn fold_field_pat(&mut self, field: syn::FieldPat) -> syn::FieldPat {
                let mut field = syn::fold::fold_field_pat(self, field);
                if matches!((&field.member, &*field.pat), (syn::Member::Named(member), Pat::Ident(pat)) if member != &pat.ident)
                {
                    field.colon_token.get_or_insert_with(Default::default);
                }
                field
            }
        }
        for pat in pattern_bindings(&pat) {
            let name = name(&pat.ident);
            if let Some(ident) = renames.get(&name) {
                self.0.insert(name, ident.clone());
            } else {
                self.0.remove(&name);
            }
        }
        Pattern(renames).fold_pat(pat)
    }

    fn condition(&mut self, expr: Expr, renames: &Renames) -> Expr {
        use syn::fold::Fold;
        match expr {
            Expr::Let(mut expr) => {
                expr.expr = Box::new(self.fold_expr(*expr.expr));
                expr.pat = Box::new(self.pattern(*expr.pat, renames));
                Expr::Let(expr)
            }
            Expr::Binary(mut expr) if matches!(expr.op, syn::BinOp::And(_)) => {
                expr.left = Box::new(self.condition(*expr.left, renames));
                expr.right = Box::new(self.condition(*expr.right, renames));
                Expr::Binary(expr)
            }
            Expr::Paren(mut expr) => {
                expr.expr = Box::new(self.condition(*expr.expr, renames));
                Expr::Paren(expr)
            }
            Expr::Group(mut expr) => {
                expr.expr = Box::new(self.condition(*expr.expr, renames));
                Expr::Group(expr)
            }
            expr => self.fold_expr(expr),
        }
    }

    fn scope<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let before = self.0.clone();
        let value = f(self);
        self.0 = before;
        value
    }
}

impl syn::fold::Fold for Rename {
    fn fold_expr(&mut self, mut expr: Expr) -> Expr {
        if let Some(ident) = value_path(&expr).and_then(|ident| self.0.get(&name(ident)))
            && let Expr::Path(path) = &mut expr
        {
            path.path.segments[0].ident = ident.clone();
        }
        syn::fold::fold_expr(self, expr)
    }

    fn fold_field_value(&mut self, field: syn::FieldValue) -> syn::FieldValue {
        let mut field = syn::fold::fold_field_value(self, field);
        if let syn::Member::Named(member) = &field.member
            && value_path(&field.expr).is_some_and(|ident| member != ident)
        {
            field.colon_token.get_or_insert_with(Default::default);
        }
        field
    }

    fn fold_item(&mut self, item: syn::Item) -> syn::Item {
        item
    }

    fn fold_block(&mut self, block: Block) -> Block {
        self.scope(|rename| syn::fold::fold_block(rename, block))
    }

    fn fold_local(&mut self, mut local: syn::Local) -> syn::Local {
        local.init = local.init.map(|init| self.fold_local_init(init));
        local.pat = self.pattern(local.pat, &Renames::new());
        local
    }

    fn fold_expr_closure(&mut self, mut expr: syn::ExprClosure) -> syn::ExprClosure {
        self.scope(|rename| {
            expr.inputs = expr
                .inputs
                .into_iter()
                .map(|pat| rename.pattern(pat, &Renames::new()))
                .collect();
            *expr.body = rename.fold_expr(*expr.body);
            expr
        })
    }

    fn fold_expr_for_loop(&mut self, mut expr: syn::ExprForLoop) -> syn::ExprForLoop {
        expr.expr = Box::new(self.fold_expr(*expr.expr));
        self.scope(|rename| {
            *expr.pat = rename.pattern(*expr.pat, &Renames::new());
            expr.body = rename.fold_block(expr.body);
            expr
        })
    }

    fn fold_expr_if(&mut self, mut expr: syn::ExprIf) -> syn::ExprIf {
        let cond = expr.cond;
        let then_branch = expr.then_branch;
        (expr.cond, expr.then_branch) = self.scope(|rename| {
            (
                Box::new(rename.condition(*cond, &Renames::new())),
                rename.fold_block(then_branch),
            )
        });
        expr.else_branch = expr
            .else_branch
            .map(|(token, branch)| (token, Box::new(self.fold_expr(*branch))));
        expr
    }

    fn fold_expr_while(&mut self, mut expr: syn::ExprWhile) -> syn::ExprWhile {
        self.scope(|rename| {
            *expr.cond = rename.condition(*expr.cond, &Renames::new());
            expr.body = rename.fold_block(expr.body);
            expr
        })
    }

    fn fold_arm(&mut self, mut arm: syn::Arm) -> syn::Arm {
        self.scope(|rename| {
            arm.pat = rename.pattern(arm.pat, &Renames::new());
            arm.guard = arm
                .guard
                .map(|(token, guard)| (token, Box::new(rename.condition(*guard, &Renames::new()))));
            *arm.body = rename.fold_expr(*arm.body);
            arm
        })
    }
}

enum End<'a> {
    Next(NodeId),
    Finish(TokenStream),
    Await(usize, Effect<'a>),
    If(Fragment<'a>, Edge, Edge),
    Match(Fragment<'a>, Vec<MatchEdge<'a>>),
    Eval(Box<Expr>, Edge),
}

type MatchEdge<'a> = (&'a syn::Arm, Option<Fragment<'a>>, Edge);

impl End<'_> {
    fn successors(&self, boundaries: &[Boundary], mut edge: impl FnMut(NodeId, &[BindingId])) {
        match self {
            Self::Next(to) => edge(*to, &[]),
            Self::If(_, yes, no) => {
                edge(yes.to, &yes.bindings);
                edge(no.to, &no.bindings);
            }
            Self::Match(_, arms) => {
                for (_, _, arm) in arms {
                    edge(arm.to, &arm.bindings);
                }
            }
            Self::Eval(_, next) => edge(next.to, &next.bindings),
            Self::Await(stage, _) => {
                let boundary = &boundaries[*stage];
                edge(boundary.resume, boundary.output.as_slice());
            }
            Self::Finish(_) => {}
        }
    }
}

impl Node<'_> {
    fn successors(&self, boundaries: &[Boundary], mut edge: impl FnMut(NodeId, &[BindingId])) {
        self.end
            .as_ref()
            .expect("complete state graph")
            .successors(boundaries, &mut edge);
        if let Some(escapes) = &self.escapes {
            for (_, escape) in &escapes.arms {
                edge(escape.to, &escape.bindings);
            }
        }
    }
}

struct Edge {
    to: NodeId,
    bindings: Vec<BindingId>,
}

#[derive(Clone, Copy)]
enum Dest {
    Finish,
    Return,
    Next(NodeId, Option<BindingId>),
}

#[derive(Clone, Copy)]
struct LoopTarget<'a> {
    label: Option<&'a syn::Label>,
    header: NodeId,
    exit: NodeId,
    result: Option<BindingId>,
    accepts_value: bool,
}

struct Graph<'a> {
    cx: &'a str,
    spawn: bool,
    bindings: Vec<Binding<'a>>,
    nodes: Vec<Node<'a>>,
    boundaries: Vec<Boundary>,
    loops: Vec<LoopTarget<'a>>,
}

impl<'a> Graph<'a> {
    fn liveness(&mut self) -> syn::Result<Vec<Live>> {
        let count = self.nodes.len();
        let mut predecessors = vec![Vec::new(); count];
        for (id, node) in self.nodes.iter().enumerate() {
            node.successors(&self.boundaries, |to, _| predecessors[to].push(id));
        }
        let mut live = vec![Live::new(); count];
        let mut queued = vec![true; count];
        let mut queue: VecDeque<_> = (0..count).rev().collect();
        while let Some(id) = queue.pop_front() {
            queued[id] = false;
            let node = &self.nodes[id];
            let mut incoming = node.uses.clone();
            node.successors(&self.boundaries, |to, kills| {
                for (&binding, &span) in &live[to] {
                    if !kills.contains(&binding) && !node.definitions.contains(&binding) {
                        incoming.entry(binding).or_insert(span);
                    }
                }
            });
            // The transfer is monotone; spans are diagnostic witnesses only.
            if incoming.len() > live[id].len() {
                live[id] = incoming;
                for &pred in &predecessors[id] {
                    if !std::mem::replace(&mut queued[pred], true) {
                        queue.push_back(pred);
                    }
                }
            }
        }
        for boundary in &self.boundaries {
            for (&id, &span) in &live[boundary.resume] {
                if Some(id) != boundary.output && matches!(self.bindings[id].kind, Kind::Outcome) {
                    return Err(Error::new(
                        span,
                        "GuestOutcome cannot cross another await; extract the owned JsValue first",
                    ));
                }
            }
        }
        let mut continuation = vec![false; count];
        let mut queue = Vec::new();
        for boundary in &self.boundaries {
            if !std::mem::replace(&mut continuation[boundary.resume], true) {
                queue.push(boundary.resume);
            }
        }
        while let Some(id) = queue.pop() {
            let node = &mut self.nodes[id];
            if let Some(error) = node.deferred.take() {
                return Err(error);
            }
            node.successors(&self.boundaries, |to, _| {
                if !std::mem::replace(&mut continuation[to], true) {
                    queue.push(to);
                }
            });
        }
        Ok(live)
    }

    fn node(&mut self) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node::default());
        id
    }

    fn bind(&mut self, env: &mut Env, pat: &PatIdent, kind: Kind<'a>) -> BindingId {
        let id = self.bindings.len();
        let ordinal = self
            .bindings
            .iter()
            .filter(|binding| binding.ordinal.is_some())
            .count() as u32;
        self.bindings.push(Binding {
            ident: pat.ident.clone(),
            mutable: pat.mutability.is_some(),
            kind,
            ordinal: Some(ordinal),
        });
        env.insert(name(&pat.ident), id);
        id
    }

    fn temporary(&mut self, span: Span, kind: Kind<'a>) -> BindingId {
        let id = self.bindings.len();
        self.bindings.push(Binding {
            ident: generated("value", id),
            mutable: false,
            kind,
            ordinal: None,
        });
        self.bindings[id]
            .ident
            .set_span(span.resolved_at(Span::mixed_site()));
        id
    }

    fn record(&mut self, node: NodeId, env: &Env, scan: Scan<'_>) -> syn::Result<()> {
        let mut scan = scan;
        scan.finish()?;
        let node = &mut self.nodes[node];
        for (name, span) in scan.free {
            if let Some(&id) = env.get(&name)
                && !node.definitions.contains(&id)
            {
                node.uses.entry(id).or_insert(span);
            }
        }
        if let Some(error) = scan.deferred {
            if let Some(previous) = &mut node.deferred {
                previous.combine(error);
            } else {
                node.deferred = Some(error);
            }
        }
        Ok(())
    }

    fn scan_expr(&mut self, node: NodeId, env: &Env, expr: &Expr) -> syn::Result<()> {
        let mut scan = Scan::new(self.cx);
        scan.visit_expr(expr);
        self.record(node, env, scan)
    }

    fn assign(&mut self, node: NodeId, id: BindingId, expr: TokenStream) {
        let ident = &self.bindings[id].ident;
        self.nodes[node].code.extend(quote!(let #ident = #expr;));
        self.nodes[node].definitions.push(id);
    }

    fn deliver(&mut self, node: NodeId, dest: Dest, expr: TokenStream) {
        self.nodes[node].end = Some(match dest {
            Dest::Finish | Dest::Return => End::Finish(quote!(return #expr;)),
            Dest::Next(to, Some(id)) => {
                self.assign(node, id, expr);
                End::Next(to)
            }
            Dest::Next(to, None) => {
                self.nodes[node].code.extend(quote!({ #expr };));
                End::Next(to)
            }
        });
    }

    fn await_at(
        &mut self,
        node: NodeId,
        env: &Env,
        mut effect: Effect<'a>,
        checked: bool,
        output: Option<BindingId>,
    ) -> syn::Result<NodeId> {
        let kind = match &effect {
            Effect::Call(args) => {
                for arg in args.iter() {
                    self.scan_expr(node, env, arg.as_ref())?;
                }
                "Call"
            }
            Effect::Gc => "Gc",
            Effect::External(op) => {
                if !self.spawn {
                    return Err(Error::new_spanned(
                        op,
                        "external await is only supported inside the tail `cx.spawn(async move { ... })` block",
                    ));
                }
                self.scan_expr(node, env, op.as_ref())?;
                "External"
            }
        };
        let mut rewrite = JumpRewrite::new(&self.loops);
        let args: &mut [Fragment<'_>] = match &mut effect {
            Effect::Call(args) => args.as_mut(),
            Effect::External(op) => std::slice::from_mut(op),
            Effect::Gc => &mut [],
        };
        for arg in args {
            if let Fragment::Source(expr) = *arg {
                *arg = rewrite.selector(expr);
            }
        }
        self.nodes[node].escapes = rewrite.finish()?;
        let stage = self.boundaries.len();
        let resume = self.node();
        self.boundaries.push(Boundary {
            resume,
            output,
            checked,
            kind,
        });
        self.nodes[node].end = Some(End::Await(stage, effect));
        Ok(resume)
    }

    fn loop_expr(
        &mut self,
        label: Option<&'a syn::Label>,
        condition: Option<&'a Expr>,
        body: &'a Block,
        env: Env,
        node: NodeId,
        dest: Dest,
    ) -> syn::Result<()> {
        let header = self.node();
        let exit = self.node();
        let result = match dest {
            Dest::Next(_, None) => None,
            _ => Some(self.temporary(body.span(), Kind::Value)),
        };
        self.nodes[node].end = Some(End::Next(header));
        self.loops.push(LoopTarget {
            label,
            header,
            exit,
            result,
            accepts_value: condition.is_none(),
        });
        let mut body_env = env;
        let body_node = if let Some(condition) = condition {
            let mut rewrite = JumpRewrite::new(&self.loops);
            let lowered_condition = rewrite.selector(condition);
            self.nodes[header].escapes = rewrite.finish()?;
            let mut scan = Scan::new(self.cx);
            let mut pats = Vec::new();
            scan.scope(|scan| scan.condition_bindings(condition, &mut pats));
            self.record(header, &body_env, scan)?;
            let bindings = pats
                .into_iter()
                .map(|pat| self.bind(&mut body_env, pat, Kind::Value))
                .collect();
            let yes = self.node();
            let no = self.node();
            self.exit_unit(no, exit, result);
            self.nodes[header].end = Some(End::If(
                lowered_condition,
                Edge { to: yes, bindings },
                Edge {
                    to: no,
                    bindings: Vec::new(),
                },
            ));
            yes
        } else {
            header
        };
        self.sequence(&body.stmts, body_env, body_node, Dest::Next(header, None))?;
        self.loops.pop();
        if let Some(id) = result {
            self.nodes[exit].uses.insert(id, body.span());
            if let Dest::Next(_, Some(out)) = dest {
                self.bindings[out].kind = self.bindings[id].kind;
            }
            let ident = &self.bindings[id].ident;
            self.deliver(exit, dest, quote!(#ident));
        } else {
            let Dest::Next(to, None) = dest else {
                unreachable!("discarded loop destination")
            };
            self.nodes[exit].end = Some(End::Next(to));
        }
        Ok(())
    }

    fn splits(&self, expr: &Expr) -> bool {
        contains_await(expr) || self.has_jump(expr)
    }

    fn has_jump(&self, expr: &Expr) -> bool {
        JumpFinder::find(expr)
    }

    fn check_selector(&self, expr: &Expr) -> syn::Result<()> {
        if self.has_jump(expr) {
            return Err(Error::new_spanned(
                expr,
                "move the loop jump out of the selector",
            ));
        }
        Ok(())
    }

    fn exit_unit(&mut self, node: NodeId, exit: NodeId, result: Option<BindingId>) {
        if result.is_some() {
            self.deliver(node, Dest::Next(exit, result), quote!(()));
        } else {
            self.nodes[node].end = Some(End::Next(exit));
        }
    }

    fn sync_jump(&mut self, expr: &Expr, env: &Env, node: NodeId, dest: Dest) -> syn::Result<()> {
        use syn::fold::Fold;

        self.scan_expr(node, env, expr)?;
        let mut rewrite = JumpRewrite::new(&self.loops);
        let rewritten = rewrite.fold_expr(expr.clone());
        self.nodes[node].escapes = rewrite.finish()?;
        self.nodes[node].end = Some(match dest {
            Dest::Next(to, result) => End::Eval(
                Box::new(rewritten),
                Edge {
                    to,
                    bindings: result.into_iter().collect(),
                },
            ),
            _ => End::Finish(quote!(return #rewritten;)),
        });
        Ok(())
    }

    fn sequence(
        &mut self,
        stmts: &'a [Stmt],
        mut env: Env,
        mut node: NodeId,
        dest: Dest,
    ) -> syn::Result<()> {
        let inherited = matches!(dest, Dest::Next(_, _)).then(|| env.clone());
        for (i, stmt) in stmts.iter().enumerate() {
            match stmt {
                Stmt::Local(local) => {
                    if let Some(init) = &local.init {
                        if let Some((_, diverge)) = &init.diverge {
                            self.check_selector(diverge)?;
                        }
                        if let Some((effect, checked)) = effect(&init.expr, self.cx)? {
                            let output = plain_ident(&local.pat).ok_or_else(|| {
                                Error::new_spanned(
                                    &local.pat,
                                    "await must bind a plain identifier (optionally `mut`)",
                                )
                            })?;
                            reject_cx(output, self.cx)?;
                            if init.diverge.is_some() || !local.attrs.is_empty() {
                                return Err(Error::new_spanned(
                                    local,
                                    "await bindings cannot have attributes or a let-else clause",
                                ));
                            }
                            let before = env.clone();
                            let id = self.bind(&mut env, output, outcome_kind(checked));
                            node = self.await_at(node, &before, effect, checked, Some(id))?;
                            continue;
                        }
                        if (structural(&init.expr) && contains_await(&init.expr))
                            || self.has_jump(&init.expr)
                        {
                            if init.diverge.is_some() || !local.attrs.is_empty() {
                                return Err(Error::new_spanned(
                                    local,
                                    "a split binding cannot have attributes or a let-else clause",
                                ));
                            }
                            let join = self.node();
                            let id = self.temporary(init.expr.span(), Kind::Value);
                            let mut scan = Scan::new(self.cx);
                            let pats = scan.pattern(&local.pat);
                            self.record(join, &env, scan)?;
                            let mut after = env.clone();
                            let definitions: Vec<_> = pats
                                .into_iter()
                                .map(|pat| self.bind(&mut after, pat, Kind::Value))
                                .collect();
                            self.expr(&init.expr, env, node, Dest::Next(join, Some(id)))?;
                            node = join;
                            env = after;
                            self.nodes[node].uses.insert(id, init.expr.span());
                            let ident = &self.bindings[id].ident;
                            let pat = &local.pat;
                            self.nodes[node].code.extend(quote!(let #pat = #ident;));
                            let kind = self.bindings[id].kind;
                            for binding in definitions {
                                self.bindings[binding].kind = kind;
                                self.nodes[node].definitions.push(binding);
                            }
                            continue;
                        }
                    }
                    let mut scan = Scan::new(self.cx);
                    let pats = scan.pattern(&local.pat);
                    // A nested structural scope must leave its inherited
                    // bindings intact for the enclosing join. Ordinary
                    // same-scope shadows stay in the original Rust fragment.
                    let shadows = pats.iter().any(|pat| {
                        let key = name(&pat.ident);
                        inherited
                            .as_ref()
                            .and_then(|outer| outer.get(&key))
                            .is_some_and(|id| env.get(&key) == Some(id))
                    });
                    if shadows {
                        let next = self.node();
                        self.nodes[node].end = Some(End::Next(next));
                        node = next;
                    }
                    scan.local_initializer(local);
                    self.record(node, &env, scan)?;
                    let mut renames = Renames::new();
                    for pat in pats {
                        let key = name(&pat.ident);
                        let was_visible = env.contains_key(&key);
                        let id = self.bind(&mut env, pat, Kind::Value);
                        self.nodes[node].definitions.push(id);
                        if shadows && local.init.is_some() && was_visible {
                            self.nodes[node].shadow.push(id);
                            renames.insert(key, generated("pattern", id));
                        }
                    }
                    if renames.is_empty() {
                        self.nodes[node].code.extend(quote!(#local));
                    } else {
                        let mut local = local.clone();
                        local.pat = Rename::default().pattern(local.pat, &renames);
                        self.nodes[node].code.extend(quote!(#local));
                        let next = self.node();
                        self.nodes[node].end = Some(End::Next(next));
                        node = next;
                    }
                }
                Stmt::Expr(expr, semi) if semi.is_none() && i + 1 == stmts.len() => {
                    return self.expr(expr, env, node, dest);
                }
                Stmt::Expr(expr, _) if matches!(ungroup(expr), Expr::Return(_)) => {
                    if self.splits(expr) {
                        return self.expr(expr, env, node, Dest::Return);
                    }
                    self.scan_expr(node, &env, expr)?;
                    self.nodes[node].end = Some(End::Finish(quote!(#expr;)));
                    return Ok(());
                }
                Stmt::Expr(expr, _) if self.splits(expr) => {
                    let next = self.node();
                    self.expr(expr, env.clone(), node, Dest::Next(next, None))?;
                    node = next;
                }
                _ => {
                    let mut scan = Scan::new(self.cx);
                    scan.visit_stmt(stmt);
                    self.record(node, &env, scan)?;
                    self.nodes[node].code.extend(quote!(#stmt));
                }
            }
        }
        self.deliver(node, dest, quote!({}));
        Ok(())
    }

    fn expr(&mut self, expr: &'a Expr, env: Env, node: NodeId, dest: Dest) -> syn::Result<()> {
        let suspends = contains_await(expr);
        let jumps = self.has_jump(expr);
        if !suspends && !jumps {
            self.scan_expr(node, &env, expr)?;
            let returns = match ungroup(expr) {
                Expr::Return(_) => true,
                Expr::Block(block) => matches!(block.block.stmts.last(),
                    Some(Stmt::Expr(tail, _)) if matches!(ungroup(tail), Expr::Return(_))),
                _ => false,
            };
            if returns {
                self.nodes[node].end = Some(End::Finish(quote!(#expr;)));
            } else {
                if let Dest::Next(_, Some(out)) = dest
                    && let Some(source) =
                        value_path(ungroup(expr)).and_then(|value| env.get(&name(value)))
                    && matches!(self.bindings[*source].kind, Kind::Outcome | Kind::JsValue)
                {
                    self.bindings[out].kind = self.bindings[*source].kind;
                }
                self.deliver(node, dest, quote!(#expr));
            }
            return Ok(());
        }
        if let Some((effect, checked)) = effect(expr, self.cx)? {
            if let Dest::Next(_, Some(out)) = dest {
                self.bindings[out].kind = outcome_kind(checked);
            }
            let discard = matches!(dest, Dest::Next(_, None));
            let value = (!discard).then(|| self.temporary(expr.span(), outcome_kind(checked)));
            let resume = self.await_at(node, &env, effect, checked, value)?;
            if let Some(id) = value {
                self.nodes[resume].uses.insert(id, expr.span());
                let ident = &self.bindings[id].ident;
                self.deliver(resume, dest, quote!(#ident));
            } else {
                let Dest::Next(to, None) = dest else {
                    unreachable!("discarded await destination")
                };
                self.nodes[resume].end = Some(End::Next(to));
            }
            return Ok(());
        }
        match ungroup(expr) {
            Expr::Loop(expr) if expr.attrs.is_empty() => {
                self.loop_expr(expr.label.as_ref(), None, &expr.body, env, node, dest)
            }
            Expr::While(expr) => {
                if contains_await(&expr.cond) {
                    return Err(Error::new_spanned(
                        &expr.cond,
                        "a while condition cannot suspend",
                    ));
                }
                if !expr.attrs.is_empty() {
                    return Err(Error::new_spanned(
                        expr,
                        "a split while cannot have attributes",
                    ));
                }
                self.loop_expr(
                    expr.label.as_ref(),
                    Some(&expr.cond),
                    &expr.body,
                    env,
                    node,
                    dest,
                )
            }
            Expr::ForLoop(expr) if suspends => {
                Err(Error::new_spanned(expr, "a for loop cannot suspend"))
            }
            Expr::Break(jump) if jump.attrs.is_empty() && jumps => {
                let target = resolve_loop(&self.loops, jump.label.as_ref(), jump.span())?;
                let dest = Dest::Next(target.exit, target.result);
                if let Some(value) = &jump.expr {
                    if !target.accepts_value {
                        return Err(Error::new_spanned(jump, "only loop accepts a break value"));
                    }
                    if suspends {
                        return self.scan_expr(node, &env, expr);
                    }
                    self.expr(value, env, node, dest)
                } else {
                    self.exit_unit(node, target.exit, target.result);
                    Ok(())
                }
            }
            Expr::Continue(jump) if jump.attrs.is_empty() && jumps => {
                let target = resolve_loop(&self.loops, jump.label.as_ref(), jump.span())?;
                self.nodes[node].end = Some(End::Next(target.header));
                Ok(())
            }
            Expr::Block(block) if block.label.is_none() && block.attrs.is_empty() => {
                self.sequence(&block.block.stmts, env, node, dest)
            }
            Expr::Return(ret) if ret.attrs.is_empty() => self.expr(
                ret.expr.as_deref().expect("split return has an operand"),
                env,
                node,
                Dest::Return,
            ),
            Expr::If(branch) if branch.attrs.is_empty() => {
                let mut rewrite = JumpRewrite::new(&self.loops);
                let condition = rewrite.selector(&branch.cond);
                self.nodes[node].escapes = rewrite.finish()?;
                let mut scan = Scan::new(self.cx);
                let mut pats = Vec::new();
                scan.scope(|scan| scan.condition_bindings(&branch.cond, &mut pats));
                self.record(node, &env, scan)?;
                let mut then_env = env.clone();
                let bindings = pats
                    .into_iter()
                    .map(|p| self.bind(&mut then_env, p, Kind::Value))
                    .collect();
                let then_node = self.node();
                self.sequence(&branch.then_branch.stmts, then_env, then_node, dest)?;
                let else_node = self.node();
                if let Some((_, expr)) = &branch.else_branch {
                    self.expr(expr, env, else_node, dest)?;
                } else {
                    self.deliver(else_node, dest, quote!({}));
                }
                self.nodes[node].end = Some(End::If(
                    condition,
                    Edge {
                        to: then_node,
                        bindings,
                    },
                    Edge {
                        to: else_node,
                        bindings: Vec::new(),
                    },
                ));
                Ok(())
            }
            Expr::Match(branch) if branch.attrs.is_empty() => {
                let mut rewrite = JumpRewrite::new(&self.loops);
                let selector = rewrite.selector(&branch.expr);
                let guards: Vec<_> = branch
                    .arms
                    .iter()
                    .map(|arm| arm.guard.as_ref().map(|(_, guard)| rewrite.selector(guard)))
                    .collect();
                self.nodes[node].escapes = rewrite.finish()?;
                self.scan_expr(node, &env, &branch.expr)?;
                let mut arms = Vec::new();
                for (arm, guard) in branch.arms.iter().zip(guards) {
                    let mut scan = Scan::new(self.cx);
                    let mut pats = Vec::new();
                    scan.scope(|scan| {
                        pats.extend(scan.bind_patterns(&arm.pat));
                        if let Some((_, guard)) = &arm.guard {
                            scan.condition_bindings(guard, &mut pats);
                        }
                    });
                    self.record(node, &env, scan)?;
                    let mut arm_env = env.clone();
                    let bindings = pats
                        .into_iter()
                        .map(|p| self.bind(&mut arm_env, p, Kind::Value))
                        .collect();
                    let to = self.node();
                    self.expr(&arm.body, arm_env, to, dest)?;
                    arms.push((arm, guard, Edge { to, bindings }));
                }
                self.nodes[node].end = Some(End::Match(selector, arms));
                Ok(())
            }
            _ if jumps && !suspends => self.sync_jump(expr, &env, node, dest),
            _ => self.scan_expr(node, &env, expr),
        }
    }
}

fn resolve_loop<'a>(
    loops: &[LoopTarget<'a>],
    label: Option<&syn::Lifetime>,
    span: Span,
) -> syn::Result<LoopTarget<'a>> {
    loops
        .iter()
        .rev()
        .copied()
        .find(|target| {
            label.is_none_or(|label| {
                target
                    .label
                    .is_some_and(|target| target.name.ident == label.ident)
            })
        })
        .ok_or_else(|| Error::new(span, "jump has no matching loop target"))
}

fn dispatch_label() -> syn::Lifetime {
    syn::Lifetime::new("'__yield_dispatch", Span::mixed_site())
}

struct Escapes {
    label: syn::Lifetime,
    ty: Ident,
    arms: Vec<(Ident, Edge)>,
}

impl Escapes {
    fn render(&self, bindings: &[Binding<'_>]) -> (TokenStream, Vec<(TokenStream, &Edge)>) {
        let ty = &self.ty;
        let mut generics = Vec::new();
        let mut variants = Vec::new();
        let mut arms = Vec::new();
        for (variant, edge) in &self.arms {
            if let Some(&id) = edge.bindings.first() {
                let generic = Ident::new(&format!("T{}", generics.len()), Span::mixed_site());
                let ident = &bindings[id].ident;
                variants.push(quote!(#variant(#generic)));
                arms.push((quote!(#ty::#variant(#ident)), edge));
                generics.push(generic);
            } else {
                variants.push(quote!(#variant));
                arms.push((quote!(#ty::#variant), edge));
            }
        }
        let params = (!generics.is_empty()).then(|| quote!(<#(#generics),*>));
        (quote!(enum #ty #params { #(#variants),* }), arms)
    }
}

#[derive(Default)]
struct JumpFinder<'a> {
    native: Vec<(Option<&'a syn::Lifetime>, bool)>,
    found: bool,
}

fn native_target<'a>(
    mut targets: impl Iterator<Item = (Option<&'a syn::Lifetime>, bool)>,
    label: Option<&syn::Lifetime>,
) -> bool {
    targets.any(|(name, is_loop)| {
        label.map_or(is_loop, |label| {
            name.is_some_and(|name| label.ident == name.ident)
        })
    })
}

impl<'a> JumpFinder<'a> {
    fn find(expr: &'a Expr) -> bool {
        let mut finder = Self::default();
        finder.visit_expr(expr);
        finder.found
    }
}

impl<'a> Visit<'a> for JumpFinder<'a> {
    fn visit_item(&mut self, _: &'a syn::Item) {}

    fn visit_expr(&mut self, expr: &'a Expr) {
        if self.found {
            return;
        }
        let (label, is_loop, condition, block) = match expr {
            Expr::Closure(_) | Expr::Async(_) | Expr::Const(_) => return,
            Expr::Break(jump) => {
                self.found = !native_target(self.native.iter().copied(), jump.label.as_ref());
                if let Some(value) = &jump.expr {
                    self.visit_expr(value);
                }
                return;
            }
            Expr::Continue(jump) => {
                self.found = !native_target(self.native.iter().copied(), jump.label.as_ref());
                return;
            }
            Expr::Loop(expr) => (&expr.label, true, None, &expr.body),
            Expr::While(expr) => (&expr.label, true, Some(expr.cond.as_ref()), &expr.body),
            Expr::ForLoop(expr) => {
                self.visit_expr(&expr.expr);
                (&expr.label, true, None, &expr.body)
            }
            Expr::Block(expr) => (&expr.label, false, None, &expr.block),
            _ => return visit::visit_expr(self, expr),
        };
        self.native
            .push((label.as_ref().map(|label| &label.name), is_loop));
        if let Some(condition) = condition {
            self.visit_expr(condition);
        }
        self.visit_block(block);
        self.native.pop();
    }
}

// A synchronous Rust fragment (notably a for loop) retains its own targets.
// Jumps to graph targets leave through a hygienic block with a typed tag.
struct JumpRewrite<'g, 'a> {
    loops: &'g [LoopTarget<'a>],
    native: Vec<(Option<syn::Lifetime>, bool)>,
    label: syn::Lifetime,
    ty: Ident,
    arms: Vec<(Ident, Edge)>,
    error: Option<Error>,
}

impl<'g, 'a> JumpRewrite<'g, 'a> {
    fn new(loops: &'g [LoopTarget<'a>]) -> Self {
        Self {
            loops,
            native: Vec::new(),
            label: syn::Lifetime::new("'__yield_escape", Span::mixed_site()),
            ty: Ident::new("__YieldEscape", Span::mixed_site()),
            arms: Vec::new(),
            error: None,
        }
    }

    fn selector<'e>(&mut self, expr: &'e Expr) -> Fragment<'e> {
        if JumpFinder::find(expr) {
            Fragment::Rewritten(Box::new(syn::fold::Fold::fold_expr(self, expr.clone())))
        } else {
            Fragment::Source(expr)
        }
    }

    fn finish(self) -> syn::Result<Option<Escapes>> {
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok((!self.arms.is_empty()).then_some(Escapes {
            label: self.label,
            ty: self.ty,
            arms: self.arms,
        }))
    }

    fn scope<T>(
        &mut self,
        label: Option<&syn::Label>,
        is_loop: bool,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.native
            .push((label.map(|label| label.name.clone()), is_loop));
        let result = f(self);
        self.native.pop();
        result
    }

    fn escape(
        &mut self,
        expr: &Expr,
        label: Option<&syn::Lifetime>,
        value: Option<&Expr>,
        is_break: bool,
    ) -> Option<Expr> {
        if native_target(
            self.native
                .iter()
                .map(|(name, is_loop)| (name.as_ref(), *is_loop)),
            label,
        ) {
            return None;
        }
        let target = match resolve_loop(self.loops, label, expr.span()) {
            Ok(target) => target,
            Err(error) => {
                self.error.get_or_insert(error);
                return None;
            }
        };
        if value.is_some() && !target.accepts_value {
            self.error
                .get_or_insert_with(|| Error::new_spanned(expr, "only loop accepts a break value"));
            return None;
        }
        let (to, result) = if is_break {
            (target.exit, target.result)
        } else {
            (target.header, None)
        };
        let variant = Ident::new(&format!("Jump{}", self.arms.len()), Span::mixed_site());
        self.arms.push((
            variant.clone(),
            Edge {
                to,
                bindings: result.into_iter().collect(),
            },
        ));
        let ty = &self.ty;
        let escape = &self.label;
        Some(match (result, value) {
            (Some(_), Some(value)) => syn::parse_quote!(break #escape #ty::#variant(#value)),
            (Some(_), None) => syn::parse_quote!(break #escape #ty::#variant(())),
            (None, Some(value)) => {
                syn::parse_quote!({ let _ = #value; break #escape #ty::#variant })
            }
            (None, None) => syn::parse_quote!(break #escape #ty::#variant),
        })
    }
}

impl syn::fold::Fold for JumpRewrite<'_, '_> {
    fn fold_expr(&mut self, expr: Expr) -> Expr {
        let expr = syn::fold::fold_expr(self, expr);
        match &expr {
            Expr::Break(jump) => self
                .escape(&expr, jump.label.as_ref(), jump.expr.as_deref(), true)
                .unwrap_or(expr),
            Expr::Continue(jump) => self
                .escape(&expr, jump.label.as_ref(), None, false)
                .unwrap_or(expr),
            _ => expr,
        }
    }

    fn fold_item(&mut self, item: syn::Item) -> syn::Item {
        item
    }
    fn fold_expr_closure(&mut self, expr: syn::ExprClosure) -> syn::ExprClosure {
        expr
    }
    fn fold_expr_async(&mut self, expr: syn::ExprAsync) -> syn::ExprAsync {
        expr
    }
    fn fold_expr_const(&mut self, expr: syn::ExprConst) -> syn::ExprConst {
        expr
    }

    fn fold_expr_loop(&mut self, mut expr: syn::ExprLoop) -> syn::ExprLoop {
        expr.body = self.scope(expr.label.as_ref(), true, |this| this.fold_block(expr.body));
        expr
    }

    fn fold_expr_while(&mut self, mut expr: syn::ExprWhile) -> syn::ExprWhile {
        (expr.cond, expr.body) = self.scope(expr.label.as_ref(), true, |this| {
            (
                Box::new(this.fold_expr(*expr.cond)),
                this.fold_block(expr.body),
            )
        });
        expr
    }

    fn fold_expr_for_loop(&mut self, mut expr: syn::ExprForLoop) -> syn::ExprForLoop {
        *expr.expr = self.fold_expr(*expr.expr);
        expr.body = self.scope(expr.label.as_ref(), true, |this| this.fold_block(expr.body));
        expr
    }

    fn fold_expr_block(&mut self, mut expr: syn::ExprBlock) -> syn::ExprBlock {
        expr.block = self.scope(expr.label.as_ref(), false, |this| {
            this.fold_block(expr.block)
        });
        expr
    }
}

fn outcome_kind(checked: bool) -> Kind<'static> {
    if checked {
        Kind::JsValue
    } else {
        Kind::Outcome
    }
}

fn generated(role: &str, id: usize) -> Ident {
    Ident::new(&format!("__yield_{role}_{id}"), Span::mixed_site())
}

fn structural(expr: &Expr) -> bool {
    matches!(
        ungroup(expr),
        Expr::Block(_)
            | Expr::If(_)
            | Expr::Match(_)
            | Expr::Loop(_)
            | Expr::While(_)
            | Expr::ForLoop(_)
    )
}

fn find_await(walk: impl FnOnce(&mut AwaitFinder)) -> bool {
    let mut find = AwaitFinder(false);
    walk(&mut find);
    find.0
}

struct AwaitFinder(bool);

impl<'ast> Visit<'ast> for AwaitFinder {
    fn visit_item(&mut self, _: &'ast syn::Item) {}

    fn visit_expr_await(&mut self, _: &'ast syn::ExprAwait) {
        self.0 = true;
    }
}

fn contains_await(expr: &Expr) -> bool {
    find_await(|find| find.visit_expr(expr))
}

fn stmt_contains_await(stmt: &Stmt) -> bool {
    find_await(|find| find.visit_stmt(stmt))
}

fn effect<'a>(expr: &'a Expr, cx: &str) -> syn::Result<Option<(Effect<'a>, bool)>> {
    let (expr, checked) = match ungroup(expr) {
        Expr::Try(expr) if expr.attrs.is_empty() => (ungroup(&expr.expr), true),
        expr => (expr, false),
    };
    let Expr::Await(awaited) = expr else {
        return Ok(None);
    };
    let Expr::MethodCall(call) = ungroup(&awaited.base) else {
        return Err(Error::new_spanned(
            awaited,
            "expected `.await` on the CallCx parameter's `.call`, `.gc`, or `.effect` method",
        ));
    };
    if !matches!(call.method.to_string().as_str(), "call" | "gc" | "effect")
        || !is_cx(ungroup(&call.receiver), cx)
        || call.turbofish.is_some()
    {
        return Err(Error::new_spanned(
            awaited,
            "expected `.await` on the CallCx parameter's `.call`, `.gc`, or `.effect` method",
        ));
    }
    let effect = if call.method == "gc" {
        if !call.args.is_empty() {
            return Err(Error::new_spanned(call, "cx.gc requires no arguments"));
        }
        Effect::Gc
    } else if call.method == "effect" {
        let mut args = call.args.iter();
        let (Some(op), None) = (args.next(), args.next()) else {
            return Err(Error::new_spanned(
                call,
                "`cx.effect` requires exactly one operation argument",
            ));
        };
        Effect::External(Fragment::Source(op))
    } else {
        let mut args = call.args.iter();
        let (Some(callee), Some(receiver), Some(argv), None) =
            (args.next(), args.next(), args.next(), args.next())
        else {
            return Err(Error::new_spanned(
                call,
                "`cx.call` requires exactly three arguments: callee, receiver, and args",
            ));
        };
        Effect::Call([callee, receiver, argv].map(Fragment::Source))
    };
    if !awaited.attrs.is_empty() || !call.attrs.is_empty() {
        return Err(Error::new_spanned(
            awaited,
            "await effects cannot have attributes",
        ));
    }
    Ok(Some((effect, checked)))
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

// Resolve free local names in an otherwise opaque Rust fragment.
struct Scan<'a> {
    cx: &'a str,
    deferred: Option<Error>,
    error: Option<Error>,
    scopes: Vec<HashSet<String>>,
    free: HashMap<String, Span>,
}

impl<'a> Scan<'a> {
    fn new(cx: &'a str) -> Self {
        Self {
            cx,
            deferred: None,
            error: None,
            scopes: Vec::new(),
            free: HashMap::new(),
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
        self.visit_pat(pat);
        pattern_bindings(pat)
    }

    fn bind_patterns<'ast>(&mut self, pat: &'ast Pat) -> Vec<&'ast PatIdent> {
        let bindings = self.pattern(pat);
        let scope = self.scopes.last_mut().expect("nested lexical scope");
        scope.extend(bindings.iter().map(|pat| name(&pat.ident)));
        bindings
    }

    fn bind(&mut self, pat: &Pat) {
        self.bind_patterns(pat);
    }

    fn condition(&mut self, expr: &Expr) {
        self.condition_bindings(expr, &mut Vec::new());
    }

    fn condition_bindings<'ast>(&mut self, expr: &'ast Expr, patterns: &mut Vec<&'ast PatIdent>) {
        match ungroup(expr) {
            Expr::Let(expr) => {
                self.visit_expr(&expr.expr);
                patterns.extend(self.bind_patterns(&expr.pat));
            }
            Expr::Binary(expr) if matches!(expr.op, syn::BinOp::And(_)) => {
                self.condition_bindings(&expr.left, patterns);
                self.condition_bindings(&expr.right, patterns);
            }
            expr => self.visit_expr(expr),
        }
    }
}

impl<'ast> Visit<'ast> for Scan<'_> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let Some(ident) = value_path(expr) {
            let name = name(ident);
            if name != self.cx && !self.scopes.iter().rev().any(|scope| scope.contains(&name)) {
                self.free.entry(name).or_insert_with(|| ident.span());
            }
        }
        visit::visit_expr(self, expr);
    }

    fn visit_expr_await(&mut self, expr: &'ast syn::ExprAwait) {
        self.fail(Error::new(expr.span(), "await is only supported on CallCx effects in a binding, statement, or structural tail; conditions, guards, and nested expressions cannot suspend"));
    }

    fn visit_expr_async(&mut self, expr: &'ast syn::ExprAsync) {
        self.fail(Error::new(
            expr.span(),
            "nested async blocks are not supported in CPS functions",
        ));
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        use proc_macro2::TokenTree;
        fn inspect(scan: &mut Scan<'_>, tokens: TokenStream) {
            let mut dot = false;
            for token in tokens {
                match &token {
                    TokenTree::Group(group) => inspect(scan, group.stream()),
                    TokenTree::Ident(ident) => {
                        if dot && ident == "await" {
                            scan.fail(Error::new(
                                ident.span(),
                                "await inside a macro cannot be lowered",
                            ));
                        }
                        let name = name(ident);
                        if name != scan.cx
                            && !scan.scopes.iter().rev().any(|scope| scope.contains(&name))
                        {
                            scan.free.entry(name).or_insert_with(|| ident.span());
                        }
                    }
                    _ => {}
                }
                dot = matches!(&token, TokenTree::Punct(p) if p.as_char() == '.');
            }
        }
        self.deferred.get_or_insert_with(|| {
            Error::new_spanned(
                mac,
                "macro calls are not supported after await; move this call to a synchronous helper",
            )
        });
        inspect(self, mac.tokens.clone());
    }

    fn visit_pat_ident(&mut self, pat: &'ast PatIdent) {
        if let Err(error) = reject_cx(pat, self.cx) {
            self.fail(error);
        }
        visit::visit_pat_ident(self, pat);
    }

    fn visit_item(&mut self, _: &'ast syn::Item) {}

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
        if contains_await(&expr.cond) {
            self.fail(Error::new_spanned(
                &expr.cond,
                "a while condition cannot suspend",
            ));
            return;
        }
        self.scope(|scan| {
            scan.condition(&expr.cond);
            scan.visit_block(&expr.body);
        });
    }

    fn visit_expr_for_loop(&mut self, expr: &'ast syn::ExprForLoop) {
        if contains_await(&expr.expr) || expr.body.stmts.iter().any(stmt_contains_await) {
            self.fail(Error::new_spanned(expr, "a for loop cannot suspend"));
            return;
        }
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
        if (call.method == "args" || call.method == "arg")
            && is_cx(ungroup(&call.receiver), self.cx)
        {
            self.deferred.get_or_insert_with(|| {
                Error::new_spanned(
                    call,
                    "use named parameters instead of reading CallCx arguments after await",
                )
            });
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
        assert_eq!(body.matches("ctx . arg (0usize)").count(), 2);
        assert!(!body.contains("__yield_saved_1"));
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
        assert_eq!(body.matches("ctx . arg (0usize)").count(), 1);
    }

    #[test]
    fn rebinding_keeps_initializer_uses_live() {
        let body = expand(syn::parse_quote!({
            let first = ctx.call(callee(), receiver(), arguments()).await;
            let keep = update(keep, first);
            finish(ctx, keep)
        }))
        .unwrap();
        assert_eq!(body.matches("ctx . arg (0usize)").count(), 1);
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
        assert!(body.contains("cx . this :: < Self >"));
        assert!(!body.contains("__yield_saved"));
    }

    #[test]
    fn early_return_stops_liveness() {
        let body = expand(syn::parse_quote!({
            let first = ctx.gc().await;
            let second = ctx.gc().await;
            return finish(ctx, second);
            consume(first, keep);
        }))
        .unwrap();
        assert!(!body.contains("ctx . arg"));
    }

    #[test]
    fn rejects_outcomes_that_need_to_cross_another_boundary() {
        let error = expand(syn::parse_quote!({
            let first = ctx.call(callee(), receiver(), arguments()).await;
            let second = ctx.gc().await;
            finish(ctx, first, second)
        }))
        .unwrap_err();
        assert!(error.to_string().contains("GuestOutcome cannot cross"));
    }

    #[test]
    fn structural_arms_share_the_join_and_keep_distinct_slots() {
        let signature = syn::parse_quote!(fn callback(ctx: &mut CallCx<'_>, keep: JsValue, dead: JsValue) -> JsResult<JsValue>);
        let block = syn::parse_quote!({
            let selected = match choose() {
                Some(keep) if guard(&keep) => {
                    ctx.effect(op()).await?;
                    keep
                }
                Some(_) => return Ok(()),
                None => ctx.call(callee(), receiver(), arguments()).await?,
            };
            tail(&selected);
            ctx.gc().await?;
            finish(keep, selected)
        });
        let (body, descriptor) = lower_spawn(&signature, &[], &block, None).unwrap();
        syn::parse2::<Block>(body.clone()).unwrap();
        syn::parse2::<Expr>(descriptor.clone()).unwrap();
        let body = body.to_string();
        let descriptor = descriptor.to_string();
        assert_eq!(body.matches("tail (& selected)").count(), 1);
        assert_eq!(body.matches("__outcome").count(), 3);
        let external = descriptor.split("CpsBoundary :: External").nth(1).unwrap();
        assert!(
            external
                .split("receiver")
                .next()
                .unwrap()
                .contains("[0u32 , 3u32]"),
            "{external}"
        );
        assert!(!descriptor.contains("__yield_saved_1"));
    }

    #[test]
    fn rejects_await_in_conditions_guards_and_macro_tokens() {
        let cases: [Block; 3] = [
            syn::parse_quote!({ if ctx.gc().await? { done() } else { done() } }),
            syn::parse_quote!({
                match value() {
                    _ if ctx.gc().await? => done(),
                    _ => done(),
                }
            }),
            syn::parse_quote!({
                inspect!(ctx.gc().await);
                done()
            }),
        ];
        for block in cases {
            assert!(expand(block).unwrap_err().to_string().contains("await"));
        }
    }

    #[test]
    fn rejects_suspending_for_and_while_condition_at_the_source() {
        for (source, expected, column) in [
            (
                "{ for x in items() { ctx.gc().await; } done() }",
                "a for loop cannot suspend",
                2,
            ),
            (
                "{ while ctx.gc().await? {} done() }",
                "a while condition cannot suspend",
                8,
            ),
        ] {
            let error = expand(syn::parse_str(source).unwrap()).unwrap_err();
            assert_eq!(error.to_string(), expected);
            assert_eq!(error.span().start().column, column);
        }
    }

    #[test]
    fn rejects_invalid_loop_jumps_and_outcomes_carried_by_backedges() {
        for (block, expected) in [
            (
                syn::parse_quote!({
                    loop {
                        ctx.gc().await?;
                        continue 'missing;
                    }
                }),
                "no matching loop target",
            ),
            (
                syn::parse_quote!({
                    while ready() {
                        ctx.gc().await?;
                        break 1;
                    }
                    done()
                }),
                "only loop accepts a break value",
            ),
            (
                syn::parse_quote!({
                    let keep = ctx.gc().await;
                    loop {
                        consume(&keep);
                        ctx.gc().await?;
                    }
                }),
                "GuestOutcome cannot cross",
            ),
            (
                syn::parse_quote!({
                    loop {
                        ctx.arg(0)?;
                        ctx.gc().await?;
                    }
                }),
                "named parameters",
            ),
        ] {
            let error = expand(block).unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn sync_async_binding_rejects_external_effect() {
        let error = expand(syn::parse_quote!({
            ctx.effect(op()).await?;
            done()
        }))
        .unwrap_err();
        assert!(error.to_string().contains("tail `cx.spawn"));
    }

    #[test]
    fn rejects_invalid_suspensions_and_continuation_constructs() {
        let cases: [(Block, &str); 6] = [
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
                    let value = ctx.gc(unexpected).await;
                }),
                "cx.gc requires no arguments",
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
