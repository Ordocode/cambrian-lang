// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use std::cell::{Cell, RefCell};

use cambrian_core::U256;

/// Source position span (byte offsets into original .cam source).
/// `file_id` indexes the parse-time file table (`file_source`); 0 is unknown.
/// PartialEq ignores positions so AST comparisons in tests work unchanged.
#[derive(Debug, Clone, Copy)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub file_id: u32,
}

thread_local! {
    static CURRENT_FILE_ID: Cell<u32> = const { Cell::new(0) };
    static FILE_SOURCES: RefCell<Vec<(String, String)>> =
        RefCell::new(vec![(String::new(), String::new())]);
}

/// RAII guard: while it lives, `Span::new` stamps `CURRENT_FILE_ID`.
pub struct ParseFileGuard {
    _priv: (),
}

impl Drop for ParseFileGuard {
    fn drop(&mut self) {
        CURRENT_FILE_ID.with(|c| c.set(0));
    }
}

/// Clear the parse-time file table (id 0 remains the unknown slot).
pub fn reset_file_table() {
    FILE_SOURCES.with(|t| {
        let mut t = t.borrow_mut();
        t.clear();
        t.push((String::new(), String::new()));
    });
    CURRENT_FILE_ID.with(|c| c.set(0));
}

/// Register `path`/`source` and stamp subsequent `Span::new` calls with the
/// assigned file id until the guard is dropped.
pub fn begin_parse_file(path: &str, source: &str) -> ParseFileGuard {
    let id = FILE_SOURCES.with(|t| {
        let mut t = t.borrow_mut();
        let id = t.len() as u32;
        t.push((path.to_string(), source.to_string()));
        id
    });
    CURRENT_FILE_ID.with(|c| c.set(id));
    ParseFileGuard { _priv: () }
}

pub fn current_file_id() -> u32 {
    CURRENT_FILE_ID.with(|c| c.get())
}

/// Path and source text for a stamped `Span::file_id`. `None` for id 0.
pub fn file_source(id: u32) -> Option<(String, String)> {
    if id == 0 {
        return None;
    }
    FILE_SOURCES
        .with(|t| t.borrow().get(id as usize).cloned())
        .filter(|(p, _)| !p.is_empty())
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Span {
            start,
            end,
            file_id: current_file_id(),
        }
    }
    pub fn none() -> Self {
        Span {
            start: 0,
            end: 0,
            file_id: 0,
        }
    }
}

impl PartialEq for Span {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// Internal helper for top-level items before sorting
#[derive(Debug, Clone, PartialEq)]
pub enum TopLevel {
    Import(Import),
    /// Phase Library-1: cross-file `.cam` import — `import "path/file.cam"`.
    /// Resolved relative to the importing file. Pulls declarations
    /// (records, enums, pure_fns, type_aliases, events, errors, extern
    /// entities, file_imports, library decls, using decls) from the target
    /// file into the current program. `entity` / `test` / `fuzz` /
    /// `invariant` declarations from imported files are rejected (F4)
    /// because they would silently deploy/duplicate.
    ImportFile(ImportFile),
    PureFn(PureFn),
    TypeAlias(TypeAlias),
    Record(Record),
    Enum(EnumDecl),
    Entity(Entity),
    ExternEntity(ExternEntity),
    Test(TestDecl),
    Property(PropertyDecl),
    Invariant(InvariantDecl),
    /// Phase EVM-P0-C: program-scope `event Foo(...);` declaration.
    /// Visible to every entity in the program. Codegen emits the
    /// declaration at file scope (above the contract bodies).
    Event(EventDecl),
    /// Phase EVM-P0-D: program-scope `error Foo(args);` declaration.
    /// Lowers to a Solidity custom-error declaration on EVM and to a
    /// deterministic numeric code on Acki Nacki.
    Error(ErrorDecl),
    /// Phase Library-3: `library Name { pure fn ... const ... type ... }`
    /// declaration. Lowers to a Solidity `library` block on EVM and to a
    /// Rust `mod` on Acki Nacki.
    Library(LibraryDecl),
    /// Phase Library-2: `using { fn1, fn2 } for T;` or `using LibName for T;`
    /// — method-call sugar. Pure AST rewrite; no runtime effect.
    Using(UsingDecl),
}

/// Internal helper for parsing ident-starting expressions
#[derive(Debug, Clone, PartialEq)]
pub enum IdentTailKind {
    Nothing,
    Call(Vec<Expr>),
    Record(Vec<(String, Expr)>),
}

/// Internal helper for parsing library items before sorting into categories
#[derive(Debug, Clone, PartialEq)]
pub enum LibraryBodyItem {
    PureFn(PureFn),
    Const(Const),
    TypeAlias(TypeAlias),
}

/// Internal helper for parsing entity items before sorting into categories
#[derive(Debug, Clone, PartialEq)]
pub enum EntityItem {
    Record(Record),
    Enum(EnumDecl),
    TypeAlias(TypeAlias),
    Const(Const),
    Macro(Macro),
    Routes(Vec<Route>),
    Member(Member),
    /// Phase EVM-P0-C: `event Transfer(indexed from: address, ...);`
    /// declared at entity scope. Lowers to a Solidity `event`
    /// declaration inside the contract on the EVM target. On
    /// Acki Nacki the declaration is dropped with warning A01
    /// (no log opcode equivalent for arbitrary events).
    Event(EventDecl),
    /// Phase EVM-P0-D: `error InsufficientBalance(have: U256, need: U256);`
    /// declared at entity scope. Lowers to a Solidity `error`
    /// declaration inside the contract on EVM. On Acki Nacki it
    /// reduces to a deterministic numeric code.
    Error(ErrorDecl),
}

/// Top-level program: imports + pure functions + entity declarations
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub imports: Vec<Import>,
    /// Phase Library-1: cross-file `import "path"` directives encountered
    /// in the source. The project loader uses these to resolve transitive
    /// `.cam` dependencies before validation; after merging, all
    /// directives appear here (deduplicated by canonical path).
    pub file_imports: Vec<ImportFile>,
    pub pure_fns: Vec<PureFn>,
    pub type_aliases: Vec<TypeAlias>,
    pub records: Vec<Record>,
    pub enums: Vec<EnumDecl>,
    pub entities: Vec<Entity>,
    pub extern_entities: Vec<ExternEntity>,
    pub tests: Vec<TestDecl>,
    /// Abstract `property` declarations (the canonical, parameterised
    /// statement). The desugaring pass lowers each into the
    /// `fuzz_tests` / `tests` collections for the Rust/EVM backends; the
    /// Lean backend reads these directly so sampling bounds never leak
    /// into the generated theorems.
    pub properties: Vec<PropertyDecl>,
    /// Derived (and, historically, source) fuzz/property test
    /// declarations. After the desugaring pass these are the lowered
    /// instances of `properties`. Consumed by the Rust/EVM/revm/cargo-fuzz
    /// backends.
    pub fuzz_tests: Vec<FuzzDecl>,
    pub invariants: Vec<InvariantDecl>,
    /// Phase EVM-P0-C: program-scope event declarations. Visible to
    /// every entity. Lowered to file-scope Solidity `event`
    /// declarations on the EVM target.
    pub events: Vec<EventDecl>,
    /// Phase EVM-P0-D: program-scope custom error declarations.
    /// Lowered to file-scope Solidity `error` declarations on EVM.
    pub errors: Vec<ErrorDecl>,
    /// Phase Library-3: `library` declarations. Lower to Solidity
    /// `library` blocks on EVM and to Rust `mod` blocks on Acki Nacki.
    pub libraries: Vec<LibraryDecl>,
    /// Phase Library-2: `using ... for T;` directives — pure method-call
    /// sugar that rewrites `recv.method(args)` to `method(recv, args)`
    /// at codegen time.
    pub using_decls: Vec<UsingDecl>,
}

/// Foreign-contract interface declaration: `extern entity Name { route foo(...) -> T; }`
///
/// Lets the EVM target emit a fully populated `interface IName { ... }` for
/// contracts that aren't part of the current program (e.g. compiled
/// separately or third-party). The Acki Nacki target ignores extern entity
/// declarations.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternEntity {
    pub name: String,
    pub routes: Vec<ExternRoute>,
    /// Phase Library-4: `@solidity_import("@openzeppelin/contracts/...")`
    /// annotation. When set, the EVM emitter (a) suppresses its
    /// synthetic `interface IName { ... }` block and (b) prepends
    /// `import "<path>";` to the generated `.sol`. The author keeps
    /// interface drift in sync manually. No effect on Acki Nacki.
    pub solidity_import: Option<String>,
    pub span: Span,
}

/// Route signature inside `extern entity { ... }`. Body-less; only the
/// signature is declared.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternRoute {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
    /// `view route foo(...)` — read-only, lowers to Solidity `view`.
    pub is_view: bool,
    /// `accept route foo(...)` — accepts ETH, lowers to Solidity `payable`.
    pub is_payable: bool,
    pub span: Span,
}

/// SDK namespace import: `use gosh`
#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    pub namespace: String,
    pub span: Span,
}

/// Phase Library-1: cross-file `.cam` import — `import "path/file.cam"`.
///
/// Path is resolved relative to the importing file. Bare (non-`./`/`../`)
/// specs also search `project.yaml` `library_paths` after a file-relative
/// miss (F5 if no root hits). Transitive imports are discovered by the
/// project loader (BFS, cycle-detected on the canonicalised filesystem
/// path). Library files (those containing only declarations, no `entity`
/// / `test` / `fuzz` / `invariant`) become shareable across projects
/// without enumeration in `project.yaml` (or via yaml `imports:`).
#[derive(Debug, Clone, PartialEq)]
pub struct ImportFile {
    pub path: String,
    pub span: Span,
}

/// Phase Library-3: `library Name { pure fn ... const ... type ... }`
///
/// Lowers to a Solidity `library` block on EVM (called via JUMP, not
/// inlined like a bare `pure fn`). On Acki Nacki, the library becomes a
/// Rust `mod <library_snake>` block in the per-entity wasm crate.
///
/// Body items are restricted (V47) to `pure fn`, `const`, and `type`
/// declarations. State / member / temporal references inside any `pure
/// fn` body are rejected by the existing V4 walker (extended as V48).
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryDecl {
    pub name: String,
    pub pure_fns: Vec<PureFn>,
    pub constants: Vec<Const>,
    pub type_aliases: Vec<TypeAlias>,
    pub span: Span,
}

/// Phase Library-2: `using` directive — method-call sugar.
///
/// Two forms:
///   - `using { fn1, fn2 } for T;` — attaches a free list of `pure fn`
///     names to receiver type `T`. `x.fn1(y)` rewrites to `fn1(x, y)`.
///   - `using LibName for T;` — attaches every `pure fn` declared inside
///     `library LibName` to receiver type `T`. Same rewrite rule applies,
///     but the call lowers as `LibName.fn1(x, y)` on EVM (library JUMP).
#[derive(Debug, Clone, PartialEq)]
pub struct UsingDecl {
    pub items: UsingItems,
    pub target_type: Type,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UsingItems {
    /// `using { fn1, fn2 } for T;` — explicit free-function list.
    Functions(Vec<String>),
    /// `using LibName for T;` — every `pure fn` from `library LibName`.
    Library(String),
}

/// Pure function declared outside entities
#[derive(Debug, Clone, PartialEq)]
pub struct PureFn {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Type,
    pub body: Expr,
    pub span: Span,
}

/// Named parameter: `name: Type`
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

/// Entity declaration with all inner items
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub name: String,
    pub records: Vec<Record>,
    pub enums: Vec<EnumDecl>,
    pub type_aliases: Vec<TypeAlias>,
    pub constants: Vec<Const>,
    pub macros: Vec<Macro>,
    pub routes: Vec<Route>,
    pub members: Vec<Member>,
    /// Phase EVM-P0-C: entity-scope event declarations. Lowered to
    /// `event Foo(...);` declarations inside the Solidity contract.
    pub events: Vec<EventDecl>,
    /// Phase EVM-P0-D: entity-scope custom error declarations.
    /// Lowered to `error Foo(...);` declarations inside the Solidity
    /// contract on EVM.
    pub errors: Vec<ErrorDecl>,
    pub span: Span,
}

/// Record type declaration: `record Name { fields }`
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub name: String,
    pub fields: Vec<Field>,
    pub span: Span,
}

/// Record field: `name: Type`
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
}

/// Enum declaration: `enum State { Created, Funded, Released }`
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    pub name: String,
    pub variants: Vec<EnumVariant>,
    pub span: Span,
}

/// Enum variant — unit or with associated data fields
#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    pub fields: Vec<Type>,
}

/// Phase EVM-P0-C: event declaration —
/// `event Transfer(indexed from: address, indexed to: address, value: U256);`
///
/// At most three params may be marked `indexed` (Solidity's
/// non-anonymous limit). Validator V36 enforces this.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDecl {
    pub name: String,
    pub params: Vec<EventParam>,
    pub span: Span,
}

/// Phase EVM-P0-C: event parameter — `[indexed] name: Type`.
#[derive(Debug, Clone, PartialEq)]
pub struct EventParam {
    pub indexed: bool,
    pub name: String,
    pub ty: Type,
}

/// Phase EVM-P0-D: custom error declaration —
/// `error InsufficientBalance(have: U256, need: U256);`
///
/// Lowers to Solidity's `error` declaration on EVM and to a
/// deterministic numeric code on Acki Nacki.
#[derive(Debug, Clone, PartialEq)]
pub struct ErrorDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub span: Span,
}

/// Type alias: `type TokenId = u64`
#[derive(Debug, Clone, PartialEq)]
pub struct TypeAlias {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

/// Constant: `const NAME: Type = value`
#[derive(Debug, Clone, PartialEq)]
pub struct Const {
    pub name: String,
    pub ty: Type,
    pub value: Expr,
    pub span: Span,
}

/// Macro: `macro name(params) -> Type = { body }`
#[derive(Debug, Clone, PartialEq)]
pub struct Macro {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Type,
    pub body: Expr,
    pub span: Span,
}

/// Route declaration inside `routes { ... }`
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub name: String,
    pub params: Vec<Param>,
    pub from_clauses: Vec<FromClause>,
    pub where_clauses: Vec<WhereClause>,
    pub return_type: Option<Type>,
    pub body: RouteBody,
    pub is_view: bool,
    pub is_pure: bool,
    pub is_init: bool,
    pub is_accept: bool,
    pub is_private: bool,
    /// If set, this route is a recover handler for bounced messages tagged with `rescue <tag>`.
    /// Syntax: `recover <tag>() => [...]`
    /// No parameters — TVM bounce body is too small for reliable typed arg parsing.
    pub recover_tag: Option<String>,
    /// `#[factory_only]` — marks the init/constructor route as factory-guarded
    /// (required under `deterministic_addresses`; BUG-U4 / owner spec #5).
    pub factory_only: bool,
    pub span: Span,
}

/// Init route: `init` keyword or route named `constructor`.
pub fn is_init_route(route: &Route) -> bool {
    route.is_init || route.name == "constructor"
}

/// Apply route-level attributes (`#[factory_only]`, …).
pub fn apply_route_attrs(route: &mut Route, attrs: Vec<TestAttr>) {
    for a in &attrs {
        if let TestAttr::Flag(name) = a {
            if name == "factory_only" {
                route.factory_only = true;
            }
        }
    }
}

pub fn finish_route(route: Route, attrs: Vec<TestAttr>) -> Route {
    let mut route = route;
    apply_route_attrs(&mut route, attrs);
    route
}

/// Route body: either a flat list of actions (unphased) or named phases.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteBody {
    /// Current syntax: `=> [ action1, action2, ... ]`
    Unphased(Vec<RouteAction>),
    /// Phased syntax: `=> [ tag1: [ actions ] tag2: [ actions ] ... ]`
    Phased(Vec<PhaseBlock>),
    /// Invalid: mix of phased and unphased items (caught by validation)
    Mixed(Vec<PhaseBlock>, Vec<RouteAction>),
}

impl RouteBody {
    pub fn is_empty(&self) -> bool {
        match self {
            RouteBody::Unphased(actions) => actions.is_empty(),
            RouteBody::Phased(phases) => phases.is_empty(),
            RouteBody::Mixed(phases, actions) => phases.is_empty() && actions.is_empty(),
        }
    }

    pub fn all_actions(&self) -> Vec<&RouteAction> {
        match self {
            RouteBody::Unphased(actions) => actions.iter().collect(),
            RouteBody::Phased(phases) => phases.iter().flat_map(|p| p.actions.iter()).collect(),
            RouteBody::Mixed(phases, actions) => {
                let mut result: Vec<&RouteAction> =
                    phases.iter().flat_map(|p| p.actions.iter()).collect();
                result.extend(actions.iter());
                result
            }
        }
    }

    pub fn is_phased(&self) -> bool {
        matches!(self, RouteBody::Phased(_))
    }

    pub fn is_mixed(&self) -> bool {
        matches!(self, RouteBody::Mixed(_, _))
    }

    pub fn phases(&self) -> Option<&[PhaseBlock]> {
        match self {
            RouteBody::Phased(phases) => Some(phases),
            RouteBody::Unphased(_) | RouteBody::Mixed(_, _) => None,
        }
    }

    /// For unphased routes: returns the action slice (panics if phased).
    pub fn actions(&self) -> &[RouteAction] {
        match self {
            RouteBody::Unphased(actions) => actions,
            RouteBody::Phased(_) => panic!("called actions() on a phased route body"),
            RouteBody::Mixed(_, actions) => actions,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            RouteBody::Unphased(actions) => actions.len(),
            RouteBody::Phased(phases) => phases.len(),
            RouteBody::Mixed(phases, actions) => phases.len() + actions.len(),
        }
    }
}

/// A named phase block within a phased route body.
///
/// `where_clauses` are per-phase guards evaluated AFTER the previous phase's
/// actions have run (so they may reference vars defined in earlier phases)
/// but BEFORE this phase's transforms apply or actions run. Lowered to
/// `require(cond, "where[<phase>]")` on EVM. See LANGUAGE.md::Phased Routes.
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseBlock {
    pub name: String,
    pub where_clauses: Vec<WhereClause>,
    pub actions: Vec<RouteAction>,
    pub span: Span,
}

/// Whether a `from` clause references an entity (with constructor-style
/// args, used to compute the sender's address) or an address-typed member
/// of the current entity (matched directly against `msg.sender`).
///
/// `from m_proposer` ⇒ `Member` — lowers to `require(msg.sender == m_proposer, ...)`
/// `from MyEntity(arg1, arg2)` ⇒ `Entity` — computes the entity's address from
/// the identity args and checks `msg.sender == <addr>`.
#[derive(Debug, Clone, PartialEq)]
pub enum FromClauseKind {
    Entity,
    Member,
}

/// `from Entity(args...) with {sdk_params}` — sender verification clause.
/// `args` are the identity (static) fields of the entity used for address computation.
/// `with_params` is an optional record expression providing SDK-specific parameters
/// (e.g. `{pubkey: sender_pubkey}`) needed for address computation on the target platform.
///
/// When `kind == Member`, `entity_name` actually carries the **member name**
/// (an `address`-typed member of the current entity) and `args` is empty;
/// `with_params` is `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct FromClause {
    pub entity_name: String,
    pub args: Vec<Expr>,
    pub with_params: Option<Expr>,
    pub kind: FromClauseKind,
    /// Optional `: throw N` annotation. When present, the failed-sender
    /// `require(...)` reverts with the canonical `"throw(N)"` message so
    /// `expect throw N` test assertions can target this clause specifically.
    /// When absent, the codegen falls back to the generic "from clause
    /// failed" string.
    pub error_code: Option<u32>,
    /// Phase EVM-P0-D: optional `: throw CustomErr(args)` annotation.
    /// When `error_name` is `Some`, codegen emits a `revert
    /// CustomErr(args);` instead of the numeric `throw(N)` shape.
    pub error_name: Option<String>,
    pub error_args: Vec<Expr>,
}

/// `where condition : throw error_code`
#[derive(Debug, Clone, PartialEq)]
pub struct WhereClause {
    pub condition: Expr,
    pub error_code: u32,
    /// Phase EVM-P0-D: optional `: throw CustomErr(args)` annotation.
    /// When `error_name` is `Some`, codegen emits a `revert
    /// CustomErr(args);` instead of the numeric `throw(N)` shape.
    pub error_name: Option<String>,
    pub error_args: Vec<Expr>,
}

/// Actions inside route body `[...]`
#[derive(Debug, Clone, PartialEq)]
pub enum RouteAction {
    /// `message(args) ~> destination` or `~> destination` (plain value transfer)
    /// Optional `with <options_expr>` for platform-specific send parameters.
    Send {
        message: Option<String>,
        args: Vec<Expr>,
        dest: Expr,
        send_options: Option<Expr>,
    },
    /// `if cond => [...] else [...]`
    Conditional {
        condition: Expr,
        then_actions: Vec<RouteAction>,
        else_actions: Vec<RouteAction>,
    },
    /// `return(exprs)`
    Return { values: Vec<Expr> },
    /// `let name = expr;` inside route body
    Let { pattern: Pattern, value: Expr },
    /// `ns::name(args)` — platform effect (e.g. `gosh::rawReserve(value, flags)`)
    Effect {
        namespace: String,
        name: String,
        args: Vec<Expr>,
    },
    /// `deploy Entity with { value: ..., stateInit: si } (args)` — deploy a contract
    Deploy {
        entity: String,
        send_options: Option<Expr>,
        constructor_args: Vec<Expr>,
    },
    /// `rescue <tag>: <action>` — marks a send/deploy action as recoverable.
    /// On bounce, the corresponding `recover <tag>` handler is invoked.
    Rescue {
        tag: String,
        action: Box<RouteAction>,
    },
    /// `throw N` — revert transaction with error code N
    Throw { error_code: u32 },
    /// Phase EVM-P0-D: `throw CustomErr(args);` — revert with a
    /// declared custom error type. `args` are positional and validated
    /// against the matching `ErrorDecl::params`.
    ThrowCustom { name: String, args: Vec<Expr> },
    /// Phase EVM-P0-C: `emit EventName(args);` — fire a logged event.
    /// Lowers to Solidity's `emit` statement on EVM, dropped to a
    /// no-op on Acki Nacki (warning A01).
    Emit { event_name: String, args: Vec<Expr> },
    /// `call routeName(args)` — invoke a private route directly (deprecated, use UpdateCode for upgrades)
    CallRoute { name: String, args: Vec<Expr> },
    /// `var name = msg(args) ~> dest [with opts];` — cross-contract call with return value.
    /// EVM-only: generates a synchronous interface call and binds the result.
    /// The bound variable is visible in subsequent actions and member transforms.
    VarCall {
        name: String,
        message: String,
        args: Vec<Expr>,
        dest: Expr,
        send_options: Option<Expr>,
    },
    /// `gosh::updateCode(code, wasmHash) with callbackRoute(args...)`
    /// Atomic code upgrade: setcode + setCurrentCode + setWasmHash + resetStorage + invoke callback in NEW code.
    UpdateCode {
        update_args: Vec<Expr>,
        callback_route: String,
        callback_args: Vec<Expr>,
    },
    /// Action-level `for <pat> in <iter> => [ <body> ]`. The body is
    /// itself a list of route actions, so loops can drive sends,
    /// state writes, deploys, etc. The validator restricts the body
    /// to be either purely state-writing or purely effectful (sends /
    /// deploys) to preserve the SSTORE-before-CALL invariant across
    /// iterations.
    For {
        pattern: Pattern,
        iter: Expr,
        body: Vec<RouteAction>,
    },
}

/// Parser-internal: an item inside `[ ... ]` that is either a bare action or a named phase block.
/// Used only during parsing; the final AST uses [`RouteBody`].
#[derive(Debug, Clone)]
pub enum RawRouteBodyItem {
    Action(RouteAction),
    Phase(PhaseBlock),
}

/// State member with transformations.
/// If `is_identity` is true, this member defines the contract's deploy-time address
/// (becomes a Solidity `static` variable, set via `genaddr --data`).
/// Identity members must have no transforms and no default value.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub name: String,
    pub ty: Type,
    pub is_identity: bool,
    pub default_value: Option<Expr>,
    pub transforms: Vec<MemberTransform>,
    pub span: Span,
}

/// `in route_name(patterns) => body` or `in route_name(patterns) => phase: body`
#[derive(Debug, Clone, PartialEq)]
pub struct MemberTransform {
    pub route_name: String,
    pub params: Vec<Pattern>,
    pub body: Expr,
    /// Phase tag for phased routes (None = unphased)
    pub phase: Option<String>,
    pub span: Span,
}

// ---------------------------------------------------------------------------
// Type system
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// `bool`, `uint8`, `uint256`, `string`, `bytes`, `address`, `usize`
    Simple(String),
    /// `array<uint256>`, `mapping<uint64, Transaction>`
    Generic(String, Vec<Type>),
    /// `(uint8, uint8, uint64)`
    Tuple(Vec<Type>),
    /// `address(Entity)` — typed address
    TypedAddress(String),
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Decimal (`42`), hex (`0xFF`), or binary (`0b1010`) — canonical `U256` value.
    IntLiteral(U256),
    /// String literal: `"hello"`
    StringLiteral(String),
    /// Bytes literal: `b"raw bytes"`
    BytesLiteral(Vec<u8>),
    /// Boolean literal: `true`, `false`
    BoolLiteral(bool),
    /// Array literal: `[1, 2, 3]`
    ArrayLit(Vec<Expr>),
    /// Empty collection: `{}`
    EmptyCollection,
    /// Variable reference: `x`, `m_owner_key`
    Ident(String),
    /// Temporal reference: `^m_custodian_count`
    TemporalRef(String),
    /// Macro invocation: `@cleaned_transactions()`
    MacroRef(String, Vec<Expr>),
    /// Message context field: `msg::sender`, `msg::timestamp`, `msg::int`, `msg::ext`, `msg::body`
    MsgField(String),
    /// System context field: `sys::now`, `sys::address`, `sys::logicaltime`, `sys::rnd_seed`, `sys::pubkey`
    SysField(String),
    /// Trace-state field accessor (invariant-only): `trace::length`.
    TraceField(String),
    /// Trace-state call accessor (invariant-only): `trace::count(route)`,
    /// `trace::lastWas(route)`. `route` is the bare name of a declared action.
    TraceCall { name: String, route: String },
    /// Binary operation: `a + b`, `x && y`
    BinOp(Box<Expr>, BinOp, Box<Expr>),
    /// Unary operation: `!x`, `*ptr`
    UnaryOp(UnaryOp, Box<Expr>),
    /// Field access: `txn.confirmations_mask`
    FieldAccess(Box<Expr>, String),
    /// Index access: `owners[0]`, `m_custodians[key]`
    Index(Box<Expr>, Box<Expr>),
    /// Method call: `mask.exists(key)`, `owners.len()`
    MethodCall(Box<Expr>, String, Vec<Expr>),
    /// Function call: `check_bit(mask, index)`
    FnCall(String, Vec<Expr>),
    /// If expression: `if cond { then } else { else }`
    If(Box<Expr>, Box<Expr>, Option<Box<Expr>>),
    /// Let binding followed by body: `let x = val; body`
    Let(Pattern, Box<Expr>, Box<Expr>),
    /// Block: `{ stmt1; stmt2; result }`
    Block(Vec<Expr>),
    /// Record construction: `Transaction { id: val, ... }`
    RecordConstruct(String, Vec<(String, Expr)>),
    /// Record update: `txn { field: new_val, ... }`
    RecordUpdate(Box<Expr>, Vec<(String, Expr)>),
    /// Closure: `|params| body`
    Closure(Vec<Pattern>, Box<Expr>),
    /// Type cast: `expr as uint8`
    Cast(Box<Expr>, Type),
    /// Tuple expression: `(a, b)`
    Tuple(Vec<Expr>),
    /// Match expression: `match expr { Pattern => body, ... }`
    Match(Box<Expr>, Vec<MatchArm>),
    /// Enum variant reference: `State::Created` or `Msg::Transfer(100)`
    EnumVariant(String, String),
    /// Enum variant with data: `Msg::Transfer(amount)`
    EnumVariantWithData(String, String, Vec<Expr>),
    /// some(expr)
    Some(Box<Expr>),
    /// none
    None,
    /// Range expression: `start..end`
    Range(Box<Expr>, Box<Expr>),
    /// For expression: `for pat in iter { body }` — produces Vec of body results
    For(Pattern, Box<Expr>, Box<Expr>),
    /// SDK namespaced call: `gosh::decode(cell) with <String, address>`
    NamespacedCall {
        namespace: String,
        name: String,
        args: Vec<Expr>,
        type_params: Vec<Type>,
    },
    /// Test-only: `address_of Entity(args) with { pubkey: pk }`
    /// Computes a TVM address for an entity using registry and identity args.
    AddressOf {
        entity_name: String,
        args: Vec<Expr>,
        with_params: Vec<(String, Expr)>,
    },
    /// Test-only: `encode<Type>(value)` — serialize a value into CamData
    Encode { target_type: Type, value: Box<Expr> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    WrappingAdd,
    WrappingSub,
    WrappingMul,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOp {
    Not,
    Neg,
    Deref,
}

// ---------------------------------------------------------------------------
// Match arms
// ---------------------------------------------------------------------------

/// `Pattern => body` inside a match expression
#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: MatchPattern,
    pub body: Expr,
}

/// Patterns allowed in match arms
#[derive(Debug, Clone, PartialEq)]
pub enum MatchPattern {
    /// Enum variant (unit): `State::Created`
    EnumVariant(String, String),
    /// Enum variant with bindings: `Msg::Transfer(amount)`
    EnumVariantWithData(String, String, Vec<Pattern>),
    /// Wildcard: `_`
    Wildcard,
    /// Literal integer
    IntLiteral(U256),
    /// Literal bool
    BoolLiteral(bool),
    /// Binding: `x`
    Ident(String),
    /// `some(pattern)` in match arm
    Some(Pattern),
    /// `none` in match arm
    None,
}

// ---------------------------------------------------------------------------
// Type normalization: Generic("Address", [Simple(name)]) → TypedAddress(name)
// ---------------------------------------------------------------------------

pub fn normalize_type(ty: &Type) -> Type {
    match ty {
        Type::Generic(name, params) if name == "Address" && params.len() == 1 => {
            if let Type::Simple(entity_name) = &params[0] {
                Type::TypedAddress(entity_name.clone())
            } else {
                Type::Generic(name.clone(), params.iter().map(normalize_type).collect())
            }
        }
        Type::Generic(name, params) => {
            Type::Generic(name.clone(), params.iter().map(normalize_type).collect())
        }
        Type::Tuple(elems) => Type::Tuple(elems.iter().map(normalize_type).collect()),
        _ => ty.clone(),
    }
}

/// Rewrite `U256::MAX` / `uint256::ZERO` style enum paths to [`Expr::IntLiteral`].
fn desugar_primitive_type_literal(expr: &mut Expr) {
    if let Expr::EnumVariant(ty, name) = expr {
        let value = match (ty.as_str(), name.as_str()) {
            ("U256" | "uint256", "MAX") => Some(U256::MAX),
            ("U256" | "uint256", "ZERO") => Some(U256::ZERO),
            ("U256" | "uint256", "ONE") => Some(U256::ONE),
            _ => None,
        };
        if let Some(v) = value {
            *expr = Expr::IntLiteral(v);
        }
    }
}

/// Bottom-up rewrite of every expression subtree in `program`.
pub fn map_program_exprs(program: &mut Program, mut f: impl FnMut(&mut Expr)) {
    for fun in &mut program.pure_fns {
        map_expr(&mut fun.body, &mut f);
    }
    for entity in &mut program.entities {
        for member in &mut entity.members {
            if let Some(default) = &mut member.default_value {
                map_expr(default, &mut f);
            }
            for tr in &mut member.transforms {
                map_expr(&mut tr.body, &mut f);
            }
        }
        for route in &mut entity.routes {
            for fc in &mut route.from_clauses {
                for arg in &mut fc.args {
                    map_expr(arg, &mut f);
                }
                if let Some(opts) = &mut fc.with_params {
                    map_expr(opts, &mut f);
                }
            }
            for wc in &mut route.where_clauses {
                map_expr(&mut wc.condition, &mut f);
            }
            map_route_body(&mut route.body, &mut f);
        }
        for mac in &mut entity.macros {
            map_expr(&mut mac.body, &mut f);
        }
        for c in &mut entity.constants {
            map_expr(&mut c.value, &mut f);
        }
    }
    for lib in &mut program.libraries {
        for fun in &mut lib.pure_fns {
            map_expr(&mut fun.body, &mut f);
        }
        for c in &mut lib.constants {
            map_expr(&mut c.value, &mut f);
        }
    }
    for test in &mut program.tests {
        for (_, v) in &mut test.init_state {
            map_expr(v, &mut f);
        }
        for step in &mut test.body {
            map_test_step(step, &mut f);
        }
    }
    for fuzz in &mut program.fuzz_tests {
        for (_, v) in &mut fuzz.init_state {
            map_expr(v, &mut f);
        }
        for step in &mut fuzz.body {
            map_test_step(step, &mut f);
        }
    }
    for prop in &mut program.properties {
        for (_, v) in &mut prop.init_state {
            map_expr(v, &mut f);
        }
        for step in &mut prop.body {
            map_test_step(step, &mut f);
        }
        for inst in &mut prop.instances {
            for (_, arg) in &mut inst.bindings {
                match arg {
                    InstanceArg::Concrete(expr) => map_expr(expr, &mut f),
                    InstanceArg::Range { lo, hi, .. } => {
                        map_expr(lo, &mut f);
                        map_expr(hi, &mut f);
                    }
                }
            }
            for (_, v) in &mut inst.init_state {
                map_expr(v, &mut f);
            }
        }
    }
    for inv in &mut program.invariants {
        for addr in &mut inv.deploy {
            map_expr(addr, &mut f);
        }
        for sender in &mut inv.senders {
            map_expr(sender, &mut f);
        }
        for action in &mut inv.actions {
            for step in &mut action.body {
                map_test_step(step, &mut f);
            }
        }
        for check in &mut inv.checks {
            map_expr(check, &mut f);
        }
        for track in &mut inv.track {
            map_expr(&mut track.value, &mut f);
        }
        for derived in &mut inv.derived {
            for step in &mut derived.body {
                map_test_step(step, &mut f);
            }
            map_expr(&mut derived.return_value, &mut f);
        }
    }
}

fn map_route_body(body: &mut RouteBody, f: &mut impl FnMut(&mut Expr)) {
    match body {
        RouteBody::Unphased(actions) => {
            for a in actions {
                map_route_action(a, f);
            }
        }
        RouteBody::Phased(phases) => {
            for phase in phases {
                for wc in &mut phase.where_clauses {
                    map_expr(&mut wc.condition, f);
                }
                for a in &mut phase.actions {
                    map_route_action(a, f);
                }
            }
        }
        RouteBody::Mixed(phases, actions) => {
            for phase in phases {
                for wc in &mut phase.where_clauses {
                    map_expr(&mut wc.condition, f);
                }
                for a in &mut phase.actions {
                    map_route_action(a, f);
                }
            }
            for a in actions {
                map_route_action(a, f);
            }
        }
    }
}

fn map_route_action(action: &mut RouteAction, f: &mut impl FnMut(&mut Expr)) {
    match action {
        RouteAction::Send { args, dest, send_options, .. } => {
            for a in args {
                map_expr(a, f);
            }
            map_expr(dest, f);
            if let Some(opts) = send_options {
                map_expr(opts, f);
            }
        }
        RouteAction::Conditional { condition, then_actions, else_actions } => {
            map_expr(condition, f);
            for a in then_actions {
                map_route_action(a, f);
            }
            for a in else_actions {
                map_route_action(a, f);
            }
        }
        RouteAction::Return { values } => {
            for v in values {
                map_expr(v, f);
            }
        }
        RouteAction::Let { value, .. } => map_expr(value, f),
        RouteAction::Effect { args, .. } => {
            for a in args {
                map_expr(a, f);
            }
        }
        RouteAction::Deploy { send_options, constructor_args, .. } => {
            if let Some(opts) = send_options {
                map_expr(opts, f);
            }
            for a in constructor_args {
                map_expr(a, f);
            }
        }
        RouteAction::Rescue { action, .. } => map_route_action(action, f),
        RouteAction::Throw { .. } => {}
        RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                map_expr(a, f);
            }
        }
        RouteAction::Emit { args, .. } => {
            for a in args {
                map_expr(a, f);
            }
        }
        RouteAction::CallRoute { args, .. } => {
            for a in args {
                map_expr(a, f);
            }
        }
        RouteAction::VarCall { args, dest, send_options, .. } => {
            for a in args {
                map_expr(a, f);
            }
            map_expr(dest, f);
            if let Some(opts) = send_options {
                map_expr(opts, f);
            }
        }
        RouteAction::UpdateCode { update_args, callback_args, .. } => {
            for a in update_args {
                map_expr(a, f);
            }
            for a in callback_args {
                map_expr(a, f);
            }
        }
        RouteAction::For { iter, body, .. } => {
            map_expr(iter, f);
            for a in body {
                map_route_action(a, f);
            }
        }
    }
}

fn map_test_step(step: &mut TestStep, f: &mut impl FnMut(&mut Expr)) {
    match step {
        TestStep::SetContext { fields, .. } => {
            for (_, v) in fields {
                map_expr(v, f);
            }
        }
        TestStep::SetRegistry { code_hash, code_depth, wasm_hash, .. } => {
            map_expr(code_hash, f);
            map_expr(code_depth, f);
            map_expr(wasm_hash, f);
        }
        TestStep::Let { value, .. } => map_expr(value, f),
        TestStep::Call { args, .. } => {
            for a in args {
                map_expr(a, f);
            }
        }
        TestStep::ExpectState { fields, .. } => {
            for (_, v) in fields {
                map_expr(v, f);
            }
        }
        TestStep::ExpectThrow { .. } => {}
        TestStep::ExpectReturn { value, .. } => map_expr(value, f),
        TestStep::ExpectReturnTuple { values, .. } => {
            for v in values {
                map_expr(v, f);
            }
        }
        TestStep::ExpectReturnLens { value, .. } => map_expr(value, f),
        TestStep::ExpectPred { cond } => map_expr(cond, f),
        TestStep::ExpectEffects { elements, .. } => {
            for el in elements {
                if let TestEffectElement::Effect(eff) = el {
                    map_test_effect(eff, f);
                }
            }
        }
        TestStep::ExpectEmit { args, .. } => {
            for a in args {
                map_expr(a, f);
            }
        }
        TestStep::Assume { cond } => map_expr(cond, f),
        TestStep::Bound { lo, hi, .. } => {
            map_expr(lo, f);
            map_expr(hi, f);
        }
        TestStep::SkipIf { cond } => map_expr(cond, f),
        TestStep::AdvanceTime { secs } => map_expr(secs, f),
        TestStep::DeployPeer { args, init_state, .. } => {
            for a in args {
                map_expr(a, f);
            }
            for (_, v) in init_state {
                map_expr(v, f);
            }
        }
    }
}

fn map_test_effect(effect: &mut TestEffect, f: &mut impl FnMut(&mut Expr)) {
    match effect {
        TestEffect::Send { args, dest, send_options, .. } => {
            for a in args {
                map_expr(a, f);
            }
            map_expr(dest, f);
            if let Some(opts) = send_options {
                map_expr(opts, f);
            }
        }
        TestEffect::Deploy { send_options, .. } => {
            if let Some(opts) = send_options {
                map_expr(opts, f);
            }
        }
        TestEffect::PlatformEffect { args, .. } => {
            for a in args {
                map_expr(a, f);
            }
        }
    }
}

pub fn map_expr(expr: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    match expr {
        Expr::BinOp(l, _, r) => {
            map_expr(l, f);
            map_expr(r, f);
        }
        Expr::UnaryOp(_, inner) => map_expr(inner, f),
        Expr::FieldAccess(inner, _) => map_expr(inner, f),
        Expr::Index(inner, idx) => {
            map_expr(inner, f);
            map_expr(idx, f);
        }
        Expr::MethodCall(inner, _, args) => {
            map_expr(inner, f);
            for a in args {
                map_expr(a, f);
            }
        }
        Expr::FnCall(_, args) => {
            for a in args {
                map_expr(a, f);
            }
        }
        Expr::If(cond, then_e, else_e) => {
            map_expr(cond, f);
            map_expr(then_e, f);
            if let Some(e) = else_e {
                map_expr(e, f);
            }
        }
        Expr::Let(_, val, body) => {
            map_expr(val, f);
            map_expr(body, f);
        }
        Expr::Block(items) => {
            for e in items {
                map_expr(e, f);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                map_expr(v, f);
            }
        }
        Expr::RecordUpdate(inner, fields) => {
            map_expr(inner, f);
            for (_, v) in fields {
                map_expr(v, f);
            }
        }
        Expr::Closure(_, body) => map_expr(body, f),
        Expr::Cast(inner, _) => map_expr(inner, f),
        Expr::Tuple(items) => {
            for e in items {
                map_expr(e, f);
            }
        }
        Expr::Match(inner, arms) => {
            map_expr(inner, f);
            for arm in arms {
                map_expr(&mut arm.body, f);
            }
        }
        Expr::EnumVariantWithData(_, _, args) => {
            for a in args {
                map_expr(a, f);
            }
        }
        Expr::Some(inner) => map_expr(inner, f),
        Expr::Range(lo, hi) => {
            map_expr(lo, f);
            map_expr(hi, f);
        }
        Expr::For(_, iter, body) => {
            map_expr(iter, f);
            map_expr(body, f);
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                map_expr(a, f);
            }
        }
        Expr::AddressOf { args, with_params, .. } => {
            for a in args {
                map_expr(a, f);
            }
            for (_, v) in with_params {
                map_expr(v, f);
            }
        }
        Expr::Encode { value, .. } => map_expr(value, f),
        Expr::ArrayLit(items) => {
            for e in items {
                map_expr(e, f);
            }
        }
        Expr::MacroRef(_, args) => {
            for a in args {
                map_expr(a, f);
            }
        }
        _ => {}
    }
    f(expr);
}

pub fn desugar_primitive_type_literals(program: &mut Program) {
    map_program_exprs(program, desugar_primitive_type_literal);
}

pub fn normalize_program_types(program: &mut Program) {
    desugar_primitive_type_literals(program);
    for fun in &mut program.pure_fns {
        fun.return_type = normalize_type(&fun.return_type);
        for p in &mut fun.params {
            p.ty = normalize_type(&p.ty);
        }
    }
    for rec in &mut program.records {
        for f in &mut rec.fields {
            f.ty = normalize_type(&f.ty);
        }
    }
    for en in &mut program.enums {
        for variant in &mut en.variants {
            for ty in &mut variant.fields {
                *ty = normalize_type(ty);
            }
        }
    }
    for ta in &mut program.type_aliases {
        ta.ty = normalize_type(&ta.ty);
    }
    for entity in &mut program.entities {
        for member in &mut entity.members {
            member.ty = normalize_type(&member.ty);
        }
        for route in &mut entity.routes {
            for p in &mut route.params {
                p.ty = normalize_type(&p.ty);
            }
            if let Some(ref mut ret) = route.return_type {
                *ret = normalize_type(ret);
            }
        }
        for mac in &mut entity.macros {
            for p in &mut mac.params {
                p.ty = normalize_type(&p.ty);
            }
            mac.return_type = normalize_type(&mac.return_type);
        }
        for rec in &mut entity.records {
            for f in &mut rec.fields {
                f.ty = normalize_type(&f.ty);
            }
        }
        for en in &mut entity.enums {
            for variant in &mut en.variants {
                for ty in &mut variant.fields {
                    *ty = normalize_type(ty);
                }
            }
        }
        for c in &mut entity.constants {
            c.ty = normalize_type(&c.ty);
        }
        for ta in &mut entity.type_aliases {
            ta.ty = normalize_type(&ta.ty);
        }
    }
    for ext in &mut program.extern_entities {
        for route in &mut ext.routes {
            for p in &mut route.params {
                p.ty = normalize_type(&p.ty);
            }
            if let Some(ref mut ret) = route.return_type {
                *ret = normalize_type(ret);
            }
        }
    }
    for lib in &mut program.libraries {
        for fun in &mut lib.pure_fns {
            fun.return_type = normalize_type(&fun.return_type);
            for p in &mut fun.params {
                p.ty = normalize_type(&p.ty);
            }
        }
        for c in &mut lib.constants {
            c.ty = normalize_type(&c.ty);
        }
        for ta in &mut lib.type_aliases {
            ta.ty = normalize_type(&ta.ty);
        }
    }
    for ud in &mut program.using_decls {
        ud.target_type = normalize_type(&ud.target_type);
    }
}

// ---------------------------------------------------------------------------
// Patterns (for let bindings, closures, member transform params)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// Simple name binding: `x`
    Ident(String),
    /// Wildcard: `_`
    Wildcard,
    /// Tuple destructuring: `(a, b)`
    Tuple(Vec<Pattern>),
    /// Dereference in pattern: `*owner`
    Deref(Box<Pattern>),
    /// Option Some destructuring: `some(x)`
    Some(Box<Pattern>),
    /// Option None: `none`
    None,
}

// ---------------------------------------------------------------------------
// Test declarations
// ---------------------------------------------------------------------------

/// Segment of a field access path (lens).
#[derive(Debug, Clone, PartialEq)]
pub enum PathSegment {
    /// `.field_name` — record/struct field access
    Field(String),
    /// `[key_expr]` — HashMap key or Vec index
    Index(Expr),
    /// `.0`, `.1`, ... — positional tuple element access
    TupleIndex(usize),
}

/// A path for accessing nested data: `m_pools[0].reserve_a`, `return.0`, etc.
pub type FieldPath = Vec<PathSegment>;

/// Top-level test declaration: `test "name" [tag "..."] for Entity [skip from] with { ... } { steps }`
#[derive(Debug, Clone, PartialEq)]
pub struct TestDecl {
    pub name: String,
    pub entity_name: String,
    pub init_state: Vec<(String, Expr)>,
    pub skip_from: bool,
    pub body: Vec<TestStep>,
    /// Optional INV-XXX / REF-XXX audit-trail marker propagated to the
    /// generated function name and assertion messages.
    pub tag: Option<String>,
    /// Catalog `property` stem linked via `#[instantiates("…")]` (CAM-H-02).
    pub instantiates: Option<String>,
    pub span: Span,
}

/// Top-level fuzz/property test declaration:
/// `fuzz "name" [tag "..."] [runs N] for Entity(p1: T, p2: T) [skip from] [with { ... }] { steps }`
#[derive(Debug, Clone, PartialEq)]
pub struct FuzzDecl {
    pub name: String,
    pub entity_name: String,
    pub params: Vec<Param>,
    pub init_state: Vec<(String, Expr)>,
    pub skip_from: bool,
    pub body: Vec<TestStep>,
    /// Optional override for the per-test `fuzz.runs` knob.
    pub runs: Option<u32>,
    /// Optional INV-XXX / REF-XXX audit-trail marker.
    pub tag: Option<String>,
    /// Catalog `property` stem linked via `#[instantiates("…")]` (CAM-H-02).
    pub instantiates: Option<String>,
    pub span: Span,
}

/// Top-level abstract property declaration — the canonical, parameterised
/// statement that `test` / `fuzz` instances are derived from:
///
/// ```text
/// property "name" [#[tag("...")]] (p1: T1, p2: T2) for Entity [with { ... }] {
///     assume <cond>            // logical precondition (flows to Lean)
///     call route(p1)
///     expect state { ... }
///
///     test "case" { p1: v }                  // concrete instance
///     #[runs(N)] fuzz "case" { p1 in lo..hi } // sampling instance
/// }
/// ```
///
/// The property `body` holds only the *logical* content (context, `let`,
/// `assume`, `call`, `expect*`); sampling `bound`s live in the nested
/// `fuzz` instances and concrete values live in the nested `test`
/// instances. The Lean backend lowers `body` + `params` + `assume`s into a
/// theorem with no sampling artefacts.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyDecl {
    pub name: String,
    pub entity_name: String,
    pub params: Vec<Param>,
    /// Default initial state, shared by all instances. Each instance may
    /// override / extend it via its own `with { ... }`.
    pub init_state: Vec<(String, Expr)>,
    /// Forall-ized starting-state targets (state fields) declared with the
    /// `*` marker in the property-level `with { ... }`.
    pub forall_state: ForallSpec,
    /// Blockchain/context parameters declared in the property-level
    /// `ctx { ... }` block (concrete pins and `*` foralls).
    pub context: ContextSpec,
    /// Logical body: context / `let` / `assume` / `call` / `expect*`.
    pub body: Vec<TestStep>,
    /// Nested `test` / `fuzz` instantiations.
    pub instances: Vec<PropertyInstance>,
    /// Optional INV-XXX / REF-XXX audit-trail marker.
    pub tag: Option<String>,
    /// Catalog `property` / `invariant` stem via `#[instantiates("…")]` (CAM-H-02).
    pub instantiates: Option<String>,
    pub span: Span,
}

/// Whether a `PropertyInstance` is a concrete example (`test`) or a
/// randomised sampling run (`fuzz`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyInstanceKind {
    Test,
    Fuzz,
}

/// One binding of a property parameter inside a `test` / `fuzz` instance
/// block.
#[derive(Debug, Clone, PartialEq)]
pub enum InstanceArg {
    /// `p: value` — a concrete value (only valid in `test` instances).
    Concrete(Expr),
    /// `p in lo..hi` / `p in lo..=hi` — a sampling range (only valid in
    /// `fuzz` instances).
    Range { lo: Expr, hi: Expr, inclusive: bool },
}

/// A nested instantiation of a property:
/// `test "name" { p: v } [skip from] [with { ... }]` or
/// `[#[runs(N)]] fuzz "name" { p in lo..hi } [skip from] [with { ... }]`.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyInstance {
    pub kind: PropertyInstanceKind,
    /// Optional instance name; combined with the property name to form the
    /// generated test function name.
    pub name: Option<String>,
    pub bindings: Vec<(String, InstanceArg)>,
    /// Per-instance initial state, layered on top of the property default.
    pub init_state: Vec<(String, Expr)>,
    /// Per-instance forall-ized starting-state targets, layered on top of the
    /// property default `forall_state`.
    pub forall_state: ForallSpec,
    /// Per-instance `ctx { ... }` block, layered on top of the property
    /// default `context`.
    pub context: ContextSpec,
    pub skip_from: bool,
    /// Optional override for the per-test `fuzz.runs` knob (fuzz only).
    pub runs: Option<u32>,
    /// Optional INV-XXX / REF-XXX audit-trail marker (overrides the
    /// property tag on the derived decl when present).
    pub tag: Option<String>,
    /// Extra steps merged into the derived test/fuzz (CAM-H-02 catalog link).
    pub body_delta: Vec<TestStep>,
    /// When true, dynamic backends run `body_delta` instead of catalog `body`.
    pub replace_catalog_body: bool,
    pub span: Span,
}

/// Internal helper: one item inside a `property { ... }` body — either a
/// logical step or a nested `test` / `fuzz` instance.
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyBodyItem {
    Step(TestStep),
    Instance(PropertyInstance),
}

/// A starting-state target opted into universal quantification via the `*`
/// marker inside a `with { ... }` clause. On the Lean target each becomes a
/// `∀`-bound variable; on the dynamic (fuzz) targets it becomes a randomly
/// sampled input; concrete `test` instances ignore it (the field keeps its
/// default unless explicitly pinned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForallTarget {
    /// `m_count: *` — an entity state member.
    StateField(String),
    /// `msg::sender: *` / `sys::timestamp: *` — a blockchain/context
    /// parameter, identified by namespace (`msg` / `sys`) + field.
    Context { namespace: String, field: String },
}

/// The set of forall-ized targets declared in a `with { ... }` clause.
/// `all_state` is set by a bare `*` entry (`with { * }`) and means "quantify
/// over every state member".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ForallSpec {
    pub targets: Vec<ForallTarget>,
    pub all_state: bool,
}

impl ForallSpec {
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty() && !self.all_state
    }
}

/// One parsed entry inside a `with { ... }` clause, before it is folded into
/// concrete pins (`Vec<(String, Expr)>`) and a [`ForallSpec`].
#[derive(Debug, Clone, PartialEq)]
pub enum WithEntry {
    /// `field: value` — a concrete starting value.
    Pin(String, Expr),
    /// `field: *` — forall over a state member.
    ForallField(String),
    /// `msg::sender: *` / `sys::timestamp: *` — forall over a context param.
    ForallCtx(String, String),
    /// bare `*` — forall over all state members.
    ForallAll,
}

/// One entry inside a dedicated `ctx { ... }` block: a blockchain/context
/// parameter (`msg::sender`, `sys::now`, …) that is either pinned to a
/// concrete value or universally quantified (`*`). Context lives in its own
/// record — separate from the entity-state `with { ... }` clause — mirroring
/// the runtime split between entity state and the message/system context
/// (`CamData` in Rust, `MsgCtx`/`SysCtx` in Lean).
#[derive(Debug, Clone, PartialEq)]
pub struct ContextEntry {
    /// `"msg"` or `"sys"`.
    pub namespace: String,
    pub field: String,
    /// `Some(expr)` for a concrete pin; `None` for a forall (`*`).
    pub value: Option<Expr>,
}

impl ContextEntry {
    pub fn is_forall(&self) -> bool {
        self.value.is_none()
    }
}

/// The set of context parameters declared in a `ctx { ... }` block.
/// (No `Eq`: entries carry an `Expr`, which is not `Eq`.)
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContextSpec {
    pub entries: Vec<ContextEntry>,
}

impl ContextSpec {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Concrete (pinned) context entries.
    pub fn pins(&self) -> impl Iterator<Item = (&str, &str, &Expr)> {
        self.entries.iter().filter_map(|e| {
            e.value
                .as_ref()
                .map(|v| (e.namespace.as_str(), e.field.as_str(), v))
        })
    }

    /// Forall-ized (`*`) context entries.
    pub fn foralls(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .filter(|e| e.is_forall())
            .map(|e| (e.namespace.as_str(), e.field.as_str()))
    }
}

/// Fold parsed `with { ... }` entries into `(pins, forall_spec)`.
pub fn collect_with(entries: Vec<WithEntry>) -> (Vec<(String, Expr)>, ForallSpec) {
    let mut pins = Vec::new();
    let mut spec = ForallSpec::default();
    for e in entries {
        match e {
            WithEntry::Pin(name, value) => pins.push((name, value)),
            WithEntry::ForallField(name) => spec.targets.push(ForallTarget::StateField(name)),
            WithEntry::ForallCtx(namespace, field) => spec
                .targets
                .push(ForallTarget::Context { namespace, field }),
            WithEntry::ForallAll => spec.all_state = true,
        }
    }
    (pins, spec)
}

/// Fold parsed property-body items into the logical `body` steps and the
/// nested instance list, then build the `PropertyDecl`.
pub fn build_property(
    name: String,
    params: Vec<Param>,
    entity_name: String,
    init_state: Vec<(String, Expr)>,
    forall_state: ForallSpec,
    context: ContextSpec,
    items: Vec<PropertyBodyItem>,
    attrs: Vec<TestAttr>,
    span: Span,
) -> PropertyDecl {
    let mut body = Vec::new();
    let mut instances = Vec::new();
    for it in items {
        match it {
            PropertyBodyItem::Step(s) => body.push(s),
            PropertyBodyItem::Instance(i) => instances.push(i),
        }
    }
    let link = catalog_decl_attrs(&attrs);
    PropertyDecl {
        name,
        entity_name,
        params,
        init_state,
        forall_state,
        context,
        body,
        instances,
        tag: link.tag,
        instantiates: link.instantiates,
        span,
    }
}

/// Build a `PropertyInstance` from the parsed pieces, honouring the
/// `#[runs(N)]`, `#[tag("...")]` and `#[skip_from]` attributes. (`skip from`
/// is expressed as the flag attribute `#[skip_from]` on instances so the
/// trailing optional clause cannot collide with a following `skip if` step.)
pub fn build_property_instance(
    kind: PropertyInstanceKind,
    name: Option<String>,
    bindings: Vec<(String, InstanceArg)>,
    init_state: Vec<(String, Expr)>,
    forall_state: ForallSpec,
    context: ContextSpec,
    attrs: Vec<TestAttr>,
    span: Span,
) -> PropertyInstance {
    let mut runs = None;
    let mut tag = None;
    let mut skip_from = false;
    for a in attrs {
        match a {
            TestAttr::Int(n, v) if n == "runs" => runs = Some(v as u32),
            TestAttr::Str(n, v) if n == "tag" => tag = Some(v),
            TestAttr::Flag(n) if n == "skip_from" => skip_from = true,
            _ => {}
        }
    }
    PropertyInstance {
        kind,
        name,
        bindings,
        init_state,
        forall_state,
        context,
        skip_from,
        runs,
        tag,
        body_delta: vec![],
        replace_catalog_body: false,
        span,
    }
}

/// Top-level stateful invariant declaration. Two surface forms:
///
/// Single-entity (legacy):
///     `invariant "name" for Entity [#[fail_on_revert]] { init {...}? senders {...}? action* check+ }`
///
/// Multi-entity (system) form:
///     `invariant "name" for system [#[fail_on_revert]] { instances { v: Vault, t: Treasury }
///       init v { ... } init t { ... } senders {...}? action <inst>.<route>(...) {...}* check+ }`
///
/// Whether an invariant declaration is emitted to test backends (CAM-H-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InvariantEmitPolicy {
    #[default]
    Emit,
    /// Catalog root superseded by supplemental `#[instantiates]` links.
    Superseded,
}

/// Both forms parse into the same shape: at least one `InvariantInstance`. The
/// single-entity form desugars to a single instance named `_self`.
#[derive(Debug, Clone, PartialEq)]
pub struct InvariantDecl {
    pub name: String,
    pub instances: Vec<InvariantInstance>,
    pub skip_from: bool,
    pub senders: Vec<Expr>,
    /// `deploy { addr }` — setup-only deployer address(es); V1 allows one.
    pub deploy: Vec<Expr>,
    /// Blockchain/context parameters declared in the invariant's
    /// `ctx { ... }` block (concrete pins and `*` foralls). Applies to the
    /// whole trace (sender / time / etc.).
    pub context: ContextSpec,
    pub actions: Vec<InvariantAction>,
    pub checks: Vec<Expr>,
    pub fail_on_revert: bool,
    /// Optional override for the runner's `runs` knob (Foundry
    /// `invariant.runs`, proptest `cases`). When `None`, falls back to
    /// the project-level `invariant.runs` config.
    pub runs: Option<u32>,
    /// Optional override for the runner's `depth` knob (Foundry
    /// `invariant.depth`).
    pub depth: Option<u32>,
    /// Optional INV-XXX / REF-XXX marker — surfaced in generated
    /// function name suffixes and revert messages so audit-trail markers
    /// flow through to test reports.
    pub tag: Option<String>,
    /// Catalog `invariant` stem via `#[instantiates("…")]` (CAM-H-02).
    pub instantiates: Option<String>,
    /// Skip EVM / Lean emit when a supplemental link owns this catalog stem.
    pub emit_policy: InvariantEmitPolicy,
    /// `with time` — exposes a synthetic `advanceTime(uint256)` action
    /// in the handler so the runner can advance `block.timestamp`.
    pub with_time: bool,
    /// Track snapshot bindings: `track { let name = expr; ... }` —
    /// computed once in the handler constructor and stored as state.
    /// Available as bare identifiers in `check` / `derived` / `action`
    /// bodies.
    pub track: Vec<InvariantSetupBinding>,
    /// Derived view helpers on the handler: `derived name(p: T) -> T { ... }`.
    pub derived: Vec<InvariantQuery>,
    /// `exclude senders { 0x... }` — addresses passed to
    /// `excludeSender(...)` in the test contract's `setUp`.
    pub exclude_senders: Vec<Expr>,
    /// `exclude selectors { route1, route2 }` — handler selector names
    /// passed to `excludeSelector(...)` in the test contract's
    /// `setUp`. Stored as wrapper-function names (e.g.
    /// `<inst>_<route>`); the codegen resolves them to bytes4
    /// signatures.
    pub exclude_selectors: Vec<String>,
    pub span: Span,
}

/// One `let name = expr;` inside an invariant `setup { ... }` block.
/// Lowered to a state field on the handler contract whose value is
/// captured once in the constructor and read by `check` / `query` /
/// `action` bodies.
#[derive(Debug, Clone, PartialEq)]
pub struct InvariantSetupBinding {
    pub name: String,
    pub value: Expr,
    pub span: Span,
}

/// One `query name(params) -> T { steps }` inside an invariant body.
/// Lowered to a `view` function on the handler. The body is a
/// `TestStep` sequence; only `Let` and `Assume` are supported (the
/// validator enforces query purity).
#[derive(Debug, Clone, PartialEq)]
pub struct InvariantQuery {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Type,
    pub body: Vec<TestStep>,
    /// The expression returned from the query. Lowered to
    /// `return <expr>;` in the generated view function.
    pub return_value: Expr,
    pub span: Span,
}

impl InvariantDecl {
    /// True when this invariant addresses a single, implicit `_self` instance
    /// (i.e. it was parsed in the single-entity surface form).
    pub fn is_single_entity(&self) -> bool {
        self.instances.len() == 1 && self.instances[0].name == "_self"
    }

    /// Convenience accessor for the single-entity form: returns the entity
    /// name. Panics if called on a multi-instance invariant.
    pub fn entity_name(&self) -> &str {
        debug_assert!(
            self.is_single_entity(),
            "entity_name() called on multi-instance invariant"
        );
        &self.instances[0].entity
    }

    /// Convenience accessor for the single-entity form: returns the
    /// per-instance init for `_self`.
    pub fn init_state(&self) -> &[(String, Expr)] {
        debug_assert!(
            self.is_single_entity(),
            "init_state() called on multi-instance invariant"
        );
        &self.instances[0].init
    }
}

/// A named instance of an entity within an invariant's `instances { ... }`
/// block. The single-entity form synthesises one instance with `name = "_self"`.
#[derive(Debug, Clone, PartialEq)]
pub struct InvariantInstance {
    pub name: String,
    pub entity: String,
    pub init: Vec<(String, Expr)>,
    /// Forall-ized starting-state targets declared with the `*` marker in this
    /// instance's `init`/`with` clause.
    pub forall_state: ForallSpec,
    /// `true` when the source spelled `init { ... }` (including an empty block).
    pub init_specified: bool,
    pub span: Span,
}

/// One `action <inst>.<route>(<params>) { <bound|assume>* }` clause inside an
/// invariant. For the single-entity form, `instance` is `"_self"`.
#[derive(Debug, Clone, PartialEq)]
pub struct InvariantAction {
    pub instance: String,
    pub route: String,
    pub params: Vec<Param>,
    pub body: Vec<TestStep>,
    pub span: Span,
}

/// Internal helper: any single item inside an `invariant { ... }` body.
/// The grammar accepts these in arbitrary order; validation enforces
/// uniqueness of `init`/`senders` and the requirement of at least one
/// `action` and one `check`.
#[derive(Debug, Clone, PartialEq)]
pub enum InvariantBodyItem {
    /// Single-entity `init { field: value, ... }` block (pins + forall spec).
    Init(Vec<(String, Expr)>, ForallSpec),
    /// Per-instance `init <inst> { field: value, ... }` clause used in the
    /// system form (instance name, pins, forall spec).
    InitInstance(String, Vec<(String, Expr)>, ForallSpec),
    /// `instances { v: Vault, t: Treasury }` block; only legal in the system form.
    Instances(Vec<InvariantInstance>),
    /// `deploy { 0x… }` — setup deployer (constructor `msg.sender`).
    Deploy(Vec<Expr>),
    Senders(Vec<Expr>),
    /// `ctx { msg::sender: *, sys::now: 100 }` — blockchain/context params.
    Ctx(ContextSpec),
    Action(InvariantAction),
    Check(Expr),
    /// `track { let name = expr; ... }` — handler-side state snapshot
    /// bindings.
    Track(Vec<InvariantSetupBinding>),
    /// `derived name(p: T) -> T { ... return expr; }` — handler-side
    /// view helper.
    Derived(InvariantQuery),
    /// `exclude senders { 0x..., 0x... }` — addresses to exclude from
    /// invariant runner sender selection.
    ExcludeSenders(Vec<Expr>),
    /// `exclude selectors { name1, name2 }` — handler selector names
    /// to exclude.
    ExcludeSelectors(Vec<String>),
}

/// Holds the flat collections that make up an `InvariantDecl` body, decoupled
/// from the surface form (single vs system).
#[derive(Debug, Clone, Default)]
pub struct InvariantBody {
    pub init: Vec<(String, Expr)>,
    /// Single-entity `init { ... }` was present in the source (may be empty).
    pub init_specified: bool,
    /// Forall spec for the single-entity `init`/`with` block.
    pub init_forall: ForallSpec,
    /// Instance names that spelled `init <inst> { ... }` (may be empty).
    pub instance_init_specified: std::collections::HashSet<String>,
    pub instance_inits: Vec<(String, Vec<(String, Expr)>)>,
    /// Forall spec per instance name, parallel to `instance_inits`.
    pub instance_init_foralls: Vec<(String, ForallSpec)>,
    pub instances: Vec<InvariantInstance>,
    pub deploy: Vec<Expr>,
    pub senders: Vec<Expr>,
    pub context: ContextSpec,
    pub actions: Vec<InvariantAction>,
    pub checks: Vec<Expr>,
    pub track: Vec<InvariantSetupBinding>,
    pub derived: Vec<InvariantQuery>,
    pub exclude_senders: Vec<Expr>,
    pub exclude_selectors: Vec<String>,
}

/// Fold a parsed list of invariant body items into a structured `InvariantBody`.
/// Later occurrences of `init`/`senders`/`instances` overwrite earlier ones (the
/// validator may emit a diagnostic when this happens). Per-instance `init`
/// clauses accumulate.
pub fn collect_invariant_body(items: Vec<InvariantBodyItem>) -> InvariantBody {
    let mut body = InvariantBody::default();
    for it in items {
        match it {
            InvariantBodyItem::Init(i, forall) => {
                body.init = i;
                body.init_forall = forall;
                body.init_specified = true;
            }
            InvariantBodyItem::InitInstance(name, fields, forall) => {
                body.instance_init_specified.insert(name.clone());
                body.instance_inits.push((name.clone(), fields));
                body.instance_init_foralls.push((name, forall));
            }
            InvariantBodyItem::Instances(insts) => body.instances = insts,
            InvariantBodyItem::Deploy(d) => body.deploy = d,
            InvariantBodyItem::Senders(s) => body.senders = s,
            InvariantBodyItem::Ctx(c) => body.context = c,
            InvariantBodyItem::Action(a) => body.actions.push(a),
            InvariantBodyItem::Check(c) => body.checks.push(c),
            InvariantBodyItem::Track(s) => body.track.extend(s),
            InvariantBodyItem::Derived(q) => body.derived.push(q),
            InvariantBodyItem::ExcludeSenders(s) => body.exclude_senders.extend(s),
            InvariantBodyItem::ExcludeSelectors(s) => body.exclude_selectors.extend(s),
        }
    }
    body
}

/// Construct a single-entity `InvariantDecl` (desugared to one `_self`
/// instance) from the parsed body items.
/// One attribute attached to a `test` / `fuzz` / `invariant` declaration.
/// The attribute name is parsed as an arbitrary identifier; the
/// attribute applier (`apply_test_attrs` / `apply_fuzz_attrs` /
/// `apply_invariant_attrs`) decides which names are honoured for which
/// declaration kind. Unknown attribute names are silently ignored — the
/// validator may emit a warning when a future linter pass is added.
///
/// Surface forms:
///  - `#[name]` -> `Flag(name)`
///  - `#[name(arg)]` -> `Str(name, arg)` for string arguments,
///    `Int(name, arg)` for integer arguments.
#[derive(Debug, Clone, PartialEq)]
pub enum TestAttr {
    Flag(String),
    Str(String, String),
    Int(String, u128),
}

impl TestAttr {
    pub fn name(&self) -> &str {
        match self {
            TestAttr::Flag(n) | TestAttr::Str(n, _) | TestAttr::Int(n, _) => n,
        }
    }
}

/// Parsed `#[tag]` / `#[instantiates]` on a top-level test/property/invariant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogDeclAttrs {
    pub tag: Option<String>,
    pub instantiates: Option<String>,
}

/// Apply honoured string attributes on catalog-linked declarations.
pub fn catalog_decl_attrs(attrs: &[TestAttr]) -> CatalogDeclAttrs {
    let mut out = CatalogDeclAttrs::default();
    for a in attrs {
        if let TestAttr::Str(name, value) = a {
            match name.as_str() {
                "tag" => out.tag = Some(value.clone()),
                "instantiates" => out.instantiates = Some(value.clone()),
                _ => {}
            }
        }
    }
    out
}

/// Apply test/fuzz/invariant `#[tag("...")]` to an `Option<String>` slot.
/// Other attribute kinds are ignored on the test side (only `tag` is
/// honoured for `test` declarations).
pub fn apply_test_attrs(tag: &mut Option<String>, attrs: Vec<TestAttr>) {
    *tag = catalog_decl_attrs(&attrs).tag;
}

/// Apply attributes to a `FuzzDecl`. Honoured attributes:
///   * `#[tag("INV-XXX")]` -> `tag`
///   * `#[runs(N)]` -> `runs`
pub fn apply_fuzz_attrs(fuzz: &mut FuzzDecl, attrs: Vec<TestAttr>) {
    let link = catalog_decl_attrs(&attrs);
    fuzz.tag = link.tag;
    fuzz.instantiates = link.instantiates;
    for a in &attrs {
        if let TestAttr::Int(name, value) = a {
            if name == "runs" {
                fuzz.runs = Some(*value as u32);
            }
        }
    }
}

/// Apply attributes to an `InvariantDecl`. Honoured attributes:
///   * `#[tag("INV-XXX")]` -> `tag`
///   * `#[runs(N)]`, `#[depth(N)]` -> `runs` / `depth`
///   * `#[with_time]` -> `with_time = true`
///   * `#[fail_on_revert]` -> `fail_on_revert = true` (was previously
///     parsed inline; now goes through the same attribute pipeline).
pub fn apply_invariant_attrs(inv: &mut InvariantDecl, attrs: Vec<TestAttr>) {
    let link = catalog_decl_attrs(&attrs);
    inv.tag = link.tag;
    inv.instantiates = link.instantiates;
    for a in &attrs {
        match a {
            TestAttr::Int(name, value) if name == "runs" => {
                inv.runs = Some(*value as u32);
            }
            TestAttr::Int(name, value) if name == "depth" => {
                inv.depth = Some(*value as u32);
            }
            TestAttr::Flag(name) if name == "with_time" => inv.with_time = true,
            TestAttr::Flag(name) if name == "fail_on_revert" => {
                inv.fail_on_revert = true;
            }
            _ => {}
        }
    }
}

pub fn build_invariant_single(
    name: String,
    entity: String,
    fail_on_revert: bool,
    items: Vec<InvariantBodyItem>,
    span: Span,
) -> InvariantDecl {
    let body = collect_invariant_body(items);
    let self_inst = InvariantInstance {
        name: "_self".to_string(),
        entity,
        init: body.init,
        forall_state: body.init_forall,
        init_specified: body.init_specified,
        span,
    };
    InvariantDecl {
        name,
        instances: vec![self_inst],
        skip_from: false,
        senders: body.senders,
        deploy: body.deploy,
        context: body.context,
        actions: body.actions,
        checks: body.checks,
        fail_on_revert,
        runs: None,
        depth: None,
        tag: None,
        instantiates: None,
        emit_policy: InvariantEmitPolicy::Emit,
        with_time: false,
        track: body.track,
        derived: body.derived,
        exclude_senders: body.exclude_senders,
        exclude_selectors: body.exclude_selectors,
        span,
    }
}

/// Construct a multi-entity (`for { v: Vault, t: Treasury }`) `InvariantDecl`
/// from a header-supplied instance list and parsed body items, binding
/// per-instance `init <inst> {...}` clauses to declared instances. Validation
/// later catches dangling per-instance inits and other errors (rules I8-I11).
pub fn build_invariant_system_with_instances(
    name: String,
    fail_on_revert: bool,
    mut instances: Vec<InvariantInstance>,
    items: Vec<InvariantBodyItem>,
    span: Span,
) -> InvariantDecl {
    let body = collect_invariant_body(items);
    let forall_for = |inst_name: &str| -> ForallSpec {
        body.instance_init_foralls
            .iter()
            .find(|(n, _)| n == inst_name)
            .map(|(_, f)| f.clone())
            .unwrap_or_default()
    };
    for (inst_name, fields) in &body.instance_inits {
        if let Some(slot) = instances.iter_mut().find(|i| &i.name == inst_name) {
            slot.init = fields.clone();
            slot.forall_state = forall_for(inst_name);
            slot.init_specified = body.instance_init_specified.contains(inst_name);
        } else {
            instances.push(InvariantInstance {
                name: inst_name.clone(),
                entity: String::new(),
                init: fields.clone(),
                forall_state: forall_for(inst_name),
                init_specified: body.instance_init_specified.contains(inst_name),
                span,
            });
        }
    }
    InvariantDecl {
        name,
        instances,
        skip_from: false,
        senders: body.senders,
        deploy: body.deploy,
        context: body.context,
        actions: body.actions,
        checks: body.checks,
        fail_on_revert,
        runs: None,
        depth: None,
        tag: None,
        instantiates: None,
        emit_policy: InvariantEmitPolicy::Emit,
        with_time: false,
        track: body.track,
        derived: body.derived,
        exclude_senders: body.exclude_senders,
        exclude_selectors: body.exclude_selectors,
        span,
    }
}

/// Single step inside a test body.
#[derive(Debug, Clone, PartialEq)]
pub enum TestStep {
    /// `msg { sender: addr, value: 100 }` or `sys { now: 1000 }`
    SetContext {
        namespace: String,
        fields: Vec<(String, Expr)>,
    },

    /// `registry EntityName { code_hash: 0x..., code_depth: N, wasm_hash: 0x... }`
    SetRegistry {
        entity_name: String,
        code_hash: Expr,
        code_depth: Expr,
        wasm_hash: Expr,
    },

    /// `let name = expr` or `let name: Type = expr`
    Let {
        name: String,
        ty: Option<Type>,
        value: Expr,
    },

    /// `call routeName(args...)` — on the entity under test — or
    /// `call peer.routeName(args...)` — on a peer bound by `DeployPeer`.
    ///
    /// `target: None` is the single-contract form every suite in the corpus
    /// was written against, and it keeps meaning exactly what it did: the
    /// instance the harness deploys in `setUp()`.
    Call {
        target: Option<String>,
        route: String,
        args: Vec<Expr>,
    },

    /// `deploy <binding> = <Entity>(<ctor args>) [with { m_x: v }]`
    ///
    /// Deploys a second contract inside one test body and binds its address
    /// to `<binding>`. Without this a suite can only ever drive one instance,
    /// so every route that reaches another contract — the whole of
    /// `UniswapV2Pair.mint`, every ERC-4626 deposit — is unreachable from
    /// the generated harness and silently untested.
    ///
    /// Identity members go in the `with { ... }` clause and are forwarded as
    /// constructor arguments (they are CREATE2 salt ingredients, so they
    /// cannot be written after the fact); ordinary members in the same clause
    /// are seeded as storage after the deploy, exactly as a test-level
    /// `with { ... }` does.
    DeployPeer {
        binding: String,
        entity: String,
        args: Vec<Expr>,
        init_state: Vec<(String, Expr)>,
    },

    /// `expect state { m_field.sub[key]: value }` — with lens paths
    ExpectState { fields: Vec<(FieldPath, Expr)> },

    /// `expect throw 100`
    ExpectThrow { code: u32 },

    /// `expect return expr` — scalar return value
    ExpectReturn { value: Expr },

    /// `expect return (a, b, c)` — tuple return value
    ExpectReturnTuple { values: Vec<Expr> },

    /// `expect return.0 == expr` or `expect return[i].field == expr` — lens on return
    ExpectReturnLens { path: FieldPath, value: Expr },

    /// `expect <bool expr>` — postcondition that is not equality.
    ///
    /// `Ident("result")` is the preceding call's return value. Relational
    /// `expect return <op> …` lowers to this form with that identifier on
    /// the left. Equality `expect return E` / `expect return.field == E`
    /// stays on the equality variants.
    ExpectPred { cond: Expr },

    /// `expect effects [...]` with wildcard `..` in any position for pattern matching
    ExpectEffects { elements: Vec<TestEffectElement> },

    /// `expect emit EventName(args…)` — Foundry `vm.expectEmit` (EVM test observability).
    ExpectEmit {
        event_name: String,
        args: Vec<Expr>,
    },

    /// `assume <bool expr>` — discard fuzz input when condition is false.
    /// Allowed only inside `fuzz` blocks; validation enforces this.
    Assume { cond: Expr },

    /// `bound <var> in <lo>..<hi>` (or `..=`) — constrain a fuzz parameter range.
    /// Allowed inside `fuzz` blocks and inside `invariant` action bodies;
    /// validation enforces context.
    Bound {
        var: String,
        lo: Expr,
        hi: Expr,
        inclusive: bool,
    },

    /// `skip if <cond>;` — early-return-without-rejection inside an
    /// invariant action body. Lowers to `if (!cond) return;` on Foundry
    /// handlers; no-op on revm / Acki Nacki backends.
    SkipIf { cond: Expr },

    /// `advanceTime(<secs>);` — built-in action available inside
    /// invariant action bodies when the invariant is declared
    /// `with time`. Lowers to `vm.warp(block.timestamp + secs)` +
    /// `vm.roll(block.number + 1)` on Foundry; no-op on other backends.
    AdvanceTime { secs: Expr },
}

/// Name of the preceding call's return value inside `expect <bool>`.
pub const EXPECT_RESULT_NAME: &str = "result";

/// `Ident("result")` — the return value of the call an `expect` predicate follows.
pub fn expect_result_expr() -> Expr {
    Expr::Ident(EXPECT_RESULT_NAME.to_string())
}

/// Apply a return-lens path to `root` (`result.field`, `result[i]`, `result.0`).
pub fn expr_from_field_path(root: Expr, path: &[PathSegment]) -> Expr {
    let mut e = root;
    for seg in path {
        e = match seg {
            PathSegment::Field(name) => Expr::FieldAccess(Box::new(e), name.clone()),
            PathSegment::TupleIndex(i) => Expr::FieldAccess(Box::new(e), i.to_string()),
            PathSegment::Index(idx) => Expr::Index(Box::new(e), Box::new(idx.clone())),
        };
    }
    e
}

/// `lhs <op> rhs` followed by `&&` / `||` tails, as one `ExpectPred`.
///
/// `&&` is folded first so `return > 0 && a || b` is `(return > 0 && a) || b`.
/// Each `||` tail is itself an and-expression (`c && d`).
pub fn expect_pred_rel(
    lhs: Expr,
    op: BinOp,
    rhs: Expr,
    ands: Vec<Expr>,
    ors: Vec<Expr>,
) -> TestStep {
    let mut cond = Expr::BinOp(Box::new(lhs), op, Box::new(rhs));
    for rhs in ands {
        cond = Expr::BinOp(Box::new(cond), BinOp::And, Box::new(rhs));
    }
    for rhs in ors {
        cond = Expr::BinOp(Box::new(cond), BinOp::Or, Box::new(rhs));
    }
    TestStep::ExpectPred { cond }
}

/// `expect return == E` is the same equality step as `expect return E`.
pub fn expect_return_eq(value: Expr) -> TestStep {
    match value {
        Expr::Tuple(values) => TestStep::ExpectReturnTuple { values },
        other => TestStep::ExpectReturn { value: other },
    }
}

/// True when `expr` contains an identifier `name` (not a field name).
pub fn expr_mentions_ident(expr: &Expr, name: &str) -> bool {
    let mut found = false;
    let mut cloned = expr.clone();
    map_expr(&mut cloned, &mut |node| {
        if let Expr::Ident(n) = node {
            if n == name {
                found = true;
            }
        }
    });
    found
}

/// Rename identifier `from` to `to` everywhere in `expr`.
pub fn rename_ident(expr: &mut Expr, from: &str, to: &str) {
    map_expr(expr, &mut |node| {
        if let Expr::Ident(n) = node {
            if n == from {
                *n = to.to_string();
            }
        }
    });
}

/// Element in an effect-matching pattern: either a concrete effect or a wildcard (`..`).
#[derive(Debug, Clone, PartialEq)]
pub enum TestEffectElement {
    Effect(TestEffect),
    /// `..` — matches zero or more arbitrary effects
    Wildcard,
}

/// Effect description in test assertions.
#[derive(Debug, Clone, PartialEq)]
pub enum TestEffect {
    /// `Message(args) ~> dest` or `~> dest with { ... }`
    Send {
        message: Option<String>,
        args: Vec<Expr>,
        dest: Expr,
        send_options: Option<Expr>,
    },
    /// `deploy Entity with { ... }`
    Deploy {
        entity: String,
        send_options: Option<Expr>,
    },
    /// `rawReserve(value, flags)` — named platform effect
    PlatformEffect { name: String, args: Vec<Expr> },
}

/// Parse error when an integer literal exceeds `U256::MAX`.
pub const U256_LITERAL_OVERFLOW: &str = "integer literal exceeds U256::MAX";

/// `IntLitVal` helper — returns a structured parse error instead of panicking.
pub fn parse_int_literal_u256(lit: &str) -> Result<U256, &'static str> {
    U256::from_decimal_str(lit).map_err(|_| U256_LITERAL_OVERFLOW)
}

/// `HexLitVal` helper — 1–64 hex digits after `0x`.
pub fn parse_hex_literal_u256(lit: &str) -> Result<U256, &'static str> {
    U256::from_hex_digits(&lit[2..]).map_err(|_| U256_LITERAL_OVERFLOW)
}

/// `BinLitVal` helper — returns a structured parse error instead of panicking.
pub fn parse_bin_literal_u256(lit: &str) -> Result<U256, &'static str> {
    U256::from_binary_digits(lit).map_err(|_| U256_LITERAL_OVERFLOW)
}

/// Narrow a literal to `u128` when the AST slot still uses a primitive (throw codes, tuple indices).
pub fn int_literal_to_u128(v: U256) -> Result<u128, &'static str> {
    if v.fits_u128() {
        Ok(v.lo)
    } else {
        Err(U256_LITERAL_OVERFLOW)
    }
}

pub fn int_literal_to_u32(v: U256) -> Result<u32, &'static str> {
    let n = int_literal_to_u128(v)?;
    u32::try_from(n).map_err(|_| U256_LITERAL_OVERFLOW)
}

pub fn int_literal_to_usize(v: U256) -> Result<usize, &'static str> {
    let n = int_literal_to_u128(v)?;
    usize::try_from(n).map_err(|_| U256_LITERAL_OVERFLOW)
}

/// Canonical 64-nibble lowercase hex (no `0x` prefix).
pub fn u256_hex_digits(v: &U256) -> String {
    format!("{:032x}{:032x}", v.hi, v.lo)
}

/// Address-shaped hex literal: 40 nibbles or 64 with 48 leading zero nibbles.
/// Decimal `0` is never address-shaped (unlike ambiguous hex surface).
pub fn u256_is_address_shaped(v: &U256) -> bool {
    if v.is_zero() {
        return false;
    }
    let s = u256_hex_digits(v);
    s.len() == 64 && (s.len() == 40 || s.as_bytes()[..48].iter().all(|c| *c == b'0'))
}

pub fn int_literal_as_u256(expr: &Expr) -> Option<U256> {
    match expr {
        Expr::IntLiteral(v) => Some(*v),
        _ => None,
    }
}

/// Decode a string-literal source slice (without the quotes) with the same
/// escape table as [`parse_bytes_literal`]. A sequence that does not decode
/// to UTF-8 (e.g. `\xff`) keeps the source text unchanged.
pub fn parse_string_literal(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    String::from_utf8(parse_bytes_literal(s)).unwrap_or_else(|_| s.to_string())
}

/// Decode a bytes-literal source slice (without the surrounding `b"` and
/// `"`). Recognised escapes: `\xHH` (one byte), `\n`, `\r`, `\t`, `\0`,
/// `\\`, `\"`. Anything else is treated as a literal byte. Used by the
/// `BytesLitVal` rule so EIP-712-style two-byte prefixes (e.g.
/// `b"\x19\x01"`) round-trip to the right packed payload.
pub fn parse_bytes_literal(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'x' if i + 3 < bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 2..i + 4]).unwrap_or("00");
                    out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
                    i += 4;
                }
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                }
                b'r' => {
                    out.push(b'\r');
                    i += 2;
                }
                b't' => {
                    out.push(b'\t');
                    i += 2;
                }
                b'0' => {
                    out.push(0);
                    i += 2;
                }
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                }
                b'"' => {
                    out.push(b'"');
                    i += 2;
                }
                _ => {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}
