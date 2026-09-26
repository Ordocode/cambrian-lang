// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Type-alias expansion for the Solidity backend.
//!
//! Solidity has no transparent type aliases, and the EVM type mapper erases
//! any name it does not recognise to `uint256`. Every declared type is
//! therefore rewritten to its alias-free form before EVM codegen. Entity-
//! and library-local aliases shadow program-scope ones inside their scope.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::ast::*;

type AliasMap = HashMap<String, Type>;

pub(crate) fn expand_type_aliases(program: &Program) -> Cow<'_, Program> {
    let has_aliases = !program.type_aliases.is_empty()
        || program.entities.iter().any(|e| !e.type_aliases.is_empty())
        || program.libraries.iter().any(|l| !l.type_aliases.is_empty());
    if !has_aliases {
        return Cow::Borrowed(program);
    }
    let mut p = program.clone();
    let prog_map = alias_map(&AliasMap::new(), &p.type_aliases);

    for f in &mut p.pure_fns {
        pure_fn(f, &prog_map);
    }
    for r in &mut p.records {
        record(r, &prog_map);
    }
    for e in &mut p.enums {
        enum_decl(e, &prog_map);
    }
    for ta in &mut p.type_aliases {
        ta.ty = expand(&ta.ty, &prog_map);
    }
    for ev in &mut p.events {
        event(ev, &prog_map);
    }
    for er in &mut p.errors {
        params(&mut er.params, &prog_map);
    }
    for ext in &mut p.extern_entities {
        for r in &mut ext.routes {
            params(&mut r.params, &prog_map);
            opt(&mut r.return_type, &prog_map);
        }
    }
    for lib in &mut p.libraries {
        let m = alias_map(&prog_map, &lib.type_aliases);
        for f in &mut lib.pure_fns {
            pure_fn(f, &m);
        }
        for c in &mut lib.constants {
            c.ty = expand(&c.ty, &m);
        }
        for ta in &mut lib.type_aliases {
            ta.ty = expand(&ta.ty, &m);
        }
    }
    for ud in &mut p.using_decls {
        ud.target_type = expand(&ud.target_type, &prog_map);
    }
    for ent in &mut p.entities {
        let m = alias_map(&prog_map, &ent.type_aliases);
        for r in &mut ent.records {
            record(r, &m);
        }
        for e in &mut ent.enums {
            enum_decl(e, &m);
        }
        for ta in &mut ent.type_aliases {
            ta.ty = expand(&ta.ty, &m);
        }
        for c in &mut ent.constants {
            c.ty = expand(&c.ty, &m);
        }
        for mac in &mut ent.macros {
            params(&mut mac.params, &m);
            mac.return_type = expand(&mac.return_type, &m);
        }
        for r in &mut ent.routes {
            params(&mut r.params, &m);
            opt(&mut r.return_type, &m);
        }
        for mem in &mut ent.members {
            mem.ty = expand(&mem.ty, &m);
        }
        for ev in &mut ent.events {
            event(ev, &m);
        }
        for er in &mut ent.errors {
            params(&mut er.params, &m);
        }
    }
    for t in &mut p.tests {
        steps(&mut t.body, &prog_map);
    }
    for f in &mut p.fuzz_tests {
        params(&mut f.params, &prog_map);
        steps(&mut f.body, &prog_map);
    }
    for pr in &mut p.properties {
        params(&mut pr.params, &prog_map);
        steps(&mut pr.body, &prog_map);
    }
    for inv in &mut p.invariants {
        for a in &mut inv.actions {
            params(&mut a.params, &prog_map);
            steps(&mut a.body, &prog_map);
        }
        for q in &mut inv.derived {
            params(&mut q.params, &prog_map);
            q.return_type = expand(&q.return_type, &prog_map);
            steps(&mut q.body, &prog_map);
        }
    }
    Cow::Owned(p)
}

fn alias_map(outer: &AliasMap, aliases: &[TypeAlias]) -> AliasMap {
    let mut m = outer.clone();
    for ta in aliases {
        m.insert(ta.name.clone(), ta.ty.clone());
    }
    m
}

/// V52 rejects cyclic aliases before codegen; the depth cap only guards
/// callers that skip validation.
fn expand(ty: &Type, m: &AliasMap) -> Type {
    expand_depth(ty, m, 0)
}

fn expand_depth(ty: &Type, m: &AliasMap, depth: usize) -> Type {
    if depth > 64 {
        return ty.clone();
    }
    match ty {
        Type::Simple(name) => match m.get(name) {
            Some(target) => expand_depth(target, m, depth + 1),
            None => ty.clone(),
        },
        Type::Generic(name, ps) => Type::Generic(
            name.clone(),
            ps.iter().map(|p| expand_depth(p, m, depth + 1)).collect(),
        ),
        Type::Tuple(es) => Type::Tuple(es.iter().map(|e| expand_depth(e, m, depth + 1)).collect()),
        Type::TypedAddress(_) => ty.clone(),
    }
}

fn opt(ty: &mut Option<Type>, m: &AliasMap) {
    if let Some(t) = ty {
        *t = expand(t, m);
    }
}

fn params(ps: &mut [Param], m: &AliasMap) {
    for p in ps {
        p.ty = expand(&p.ty, m);
    }
}

fn pure_fn(f: &mut PureFn, m: &AliasMap) {
    params(&mut f.params, m);
    f.return_type = expand(&f.return_type, m);
}

fn record(r: &mut Record, m: &AliasMap) {
    for f in &mut r.fields {
        f.ty = expand(&f.ty, m);
    }
}

fn enum_decl(e: &mut EnumDecl, m: &AliasMap) {
    for v in &mut e.variants {
        for t in &mut v.fields {
            *t = expand(t, m);
        }
    }
}

fn event(ev: &mut EventDecl, m: &AliasMap) {
    for p in &mut ev.params {
        p.ty = expand(&p.ty, m);
    }
}

fn steps(body: &mut [TestStep], m: &AliasMap) {
    for s in body {
        if let TestStep::Let { ty: Some(t), .. } = s {
            *t = expand(t, m);
        }
    }
}
