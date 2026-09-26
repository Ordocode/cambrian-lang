// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — per-entity CREATE2-style `address` derivation (P3.4).
//!
//! For every entity `E` we emit:
//!
//! ```text
//! /-- Deployer for `E`. Per-program constant — abstracts the EVM
//!     factory's `address(this)` argument. -/
//! def E.deployer : Cambrian.Address := <fnv1a_addr E.name>
//!
//! /-- Init-code hash for `E`. Per-entity constant — abstracts the
//!     hash of the bytecode + ABI surface emitted by the EVM target. -/
//! def E.initCodeHash : BitVec 256 := <fnv1a_256 E.name>
//!
//! /-- Encode the identity tuple into the CREATE2 salt. Singletons
//!     use the constant `0#256`; single-field identities zero-extend
//!     their field; multi-field identities chain via a fold. -/
//! def E.salt (id : E.Identity) : BitVec 256 := <encoder>
//!
//! /-- Deterministic address of the `E` instance at `id`. -/
//! def E.address (id : E.Identity) : Cambrian.Address :=
//!   Cambrian.create2Address E.deployer (E.salt id) E.initCodeHash
//! ```
//!
//! **Abstract vs runtime (PM-025):** these FNV-derived constants and the
//! opaque / FNV-mixed `create2Address` are an *injectivity* model for
//! proofs — they are **not** numerically equal to EVM
//! `CambrianFactory.predict*` (Keccak-256 CREATE2 with `salt = 0` and
//! identity folded into init-code). The factory remains the runtime
//! oracle; Lean addresses stay abstract.
//!
//! Crucially the per-entity `deployer` / `initCodeHash` constants are
//! distinct across entities (FNV-1a hash of the entity name), so
//! `create2Address`' injectivity axiom (see `Cambrian/Evm.lean`)
//! gives `E.address id ≠ F.address id'` whenever `E ≠ F`. Same-entity
//! injectivity falls out when the `salt` encoder is injective (true
//! for singletons trivially, for single-field identities via
//! zero-extension, and assumed for multi-field via the encoder).
//!
//! This codegen is purely structural — it is invoked from
//! [`lean_entity::gen_entity`] after `State` / `Identity` / `default`,
//! so the address def lives in the same `namespace <Entity>` block.

use crate::ast::{Entity, Member, Type};

/// Emit the address-derivation definitions for a single entity into
/// the provided buffer. Caller is responsible for namespace framing
/// (we use bare `def State.X` style, matching the rest of
/// `lean_entity.rs`'s emission convention).
pub fn emit_address(out: &mut String, entity: &Entity, nat: bool) {
    let identity_members: Vec<&Member> = entity.members.iter().filter(|m| m.is_identity).collect();

    let deployer_hex = entity_deployer_addr(&entity.name);
    let init_hex = format_init_hash(entity_init_code_hash(&entity.name));

    out.push_str(&format!(
        "/-- Deployer for `{ename}`. Per-program constant; abstracts the EVM\n    factory's `address(this)` argument. -/\ndef deployer : Cambrian.Address :=\n  0x{deployer:040x}#160\n\n",
        ename = entity.name,
        deployer = deployer_hex,
    ));

    out.push_str(&format!(
        "/-- Init-code hash for `{ename}`. Per-entity constant; abstracts the\n    hash of the bytecode + ABI surface emitted by the EVM target. -/\ndef initCodeHash : BitVec 256 :=\n  0x{init}#256\n\n",
        ename = entity.name,
        init = init_hex,
    ));

    out.push_str("/-- Encode the identity tuple into the CREATE2 salt. -/\n");
    out.push_str("def salt (id : Identity) : BitVec 256 :=\n");
    if identity_members.is_empty() {
        out.push_str("  0#256\n\n");
    } else {
        out.push_str(&format!("  {}\n\n", salt_expr(&identity_members, nat),));
    }

    out.push_str(&format!(
        "/-- Deterministic address of the `{ename}` instance at `id`. -/\ndef address (id : Identity) : Cambrian.Address :=\n  Cambrian.create2Address deployer (salt id) initCodeHash\n\n",
        ename = entity.name,
    ));
}

/// Salt encoding helper. For singletons we use `0#256`; for
/// single-field identities we zero-extend the field; for multi-field
/// identities we left-shift + XOR each field's encoding into the
/// running 256-bit accumulator (XOR-based combining keeps the
/// implementation total and termination-clear; the encoder is *not*
/// guaranteed injective for arbitrary multi-field cases, but each
/// generated `<E>.address` still composes with the opaque
/// `create2Address` so user proofs that need distinctness can be
/// stated via `create2Address_injective` + a per-fixture salt-
/// injectivity lemma in user code).
fn salt_expr(identity_members: &[&Member], nat: bool) -> String {
    let parts: Vec<String> = identity_members
        .iter()
        .map(|m| encode_field(&m.name, &m.ty, nat))
        .collect();
    if parts.len() == 1 {
        parts.into_iter().next().unwrap()
    } else {
        parts.join(" ^^^ ")
    }
}

/// Encode a single identity field into a `BitVec 256` expression
/// (Lean source). Numeric types zero-extend via `Cambrian.castWidth`;
/// `address` / `pubkey` zero-extend through the same helper.
fn encode_field(name: &str, ty: &Type, nat: bool) -> String {
    // Under `numerics: nat` unsigned identity members are `Nat` (inject via
    // `BitVec.ofNat`); signed are `Int` (inject via `BitVec.ofInt` so the
    // two's-complement bit pattern is preserved). `address` / `pubkey` stay
    // fixed-width and widen through `castWidth` as before.
    match ty {
        Type::Simple(t) => match t.as_str() {
            "bool" => format!("(if id.{name} then 1#256 else 0#256)", name = name),
            "i8" | "i16" | "i32" | "i64" | "i128" if nat => {
                format!("(BitVec.ofInt 256 id.{name})", name = name)
            }
            "u8" | "u16" | "u32" | "u64" | "u128" | "U256" | "uint256" if nat => {
                format!("(BitVec.ofNat 256 id.{name})", name = name)
            }
            "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i32" | "i64" | "i128"
            | "address" => format!("Cambrian.castWidth 256 id.{name}", name = name),
            "U256" | "uint256" => format!("id.{name}", name = name),
            "pubkey" => format!("Cambrian.castWidth 256 id.{name}", name = name),
            _ => format!(
                "0#256 /- TODO: encode identity field `{name}` of user-defined type `{t}` -/",
                name = name,
                t = t
            ),
        },
        _ => format!(
            "0#256 /- TODO: encode identity field `{name}` of non-primitive type -/",
            name = name
        ),
    }
}

/// FNV-1a 32-bit hash extended to a 160-bit "address constant" by
/// repeating the 32-bit hash 5×. Deterministic, side-effect-free, and
/// distinct across distinct entity names. Used for `<E>.deployer` so
/// the abstract CREATE2 address-injectivity lemma works across
/// entities.
fn entity_deployer_addr(name: &str) -> u128 {
    // Pack the 32-bit hash into the low 32 bits and salt the upper
    // bits with a different seed to avoid collisions between
    // `deployer` and `initCodeHash`-derived addresses (latter is 256
    // bits, not 160, so they can't accidentally compare equal in
    // user proofs).
    let h = fnv1a_32(name.as_bytes()) as u128;
    let salted = (h ^ 0x_DEAD_BEEF_u128) & 0xFFFF_FFFF;
    // Spread the 32-bit hash across 160 bits by repeating + offset.
    salted
        | ((salted ^ 0x_FACE_u128) << 32)
        | ((salted ^ 0xCAFE_u128) << 64)
        | ((salted ^ 0xBABE_u128) << 96)
}

/// FNV-1a 256-bit hash (4× FNV-1a-64 with different seeds, packed).
/// Same rationale as `entity_deployer_addr`.
fn entity_init_code_hash(name: &str) -> [u64; 4] {
    let h0 = fnv1a_64_seeded(name.as_bytes(), 0xCBF2_9CE4_8422_2325);
    let h1 = fnv1a_64_seeded(name.as_bytes(), 0x9E37_79B9_7F4A_7C15);
    let h2 = fnv1a_64_seeded(name.as_bytes(), 0xBB67_AE85_84CA_A73B);
    let h3 = fnv1a_64_seeded(name.as_bytes(), 0x3C6E_F372_FE94_F82B);
    [h0, h1, h2, h3]
}

const FNV1A_32_OFFSET: u32 = 0x811c_9dc5; // Fowler–Noll–Vo FNV-1a offset basis
const FNV1A_32_PRIME: u32 = 0x0100_0193; // FNV-1a 32-bit prime

fn fnv1a_32(bytes: &[u8]) -> u32 {
    let mut h = FNV1A_32_OFFSET;
    for b in bytes {
        h ^= *b as u32;
        h = h.wrapping_mul(FNV1A_32_PRIME);
    }
    h
}

const FNV1A_64_PRIME: u64 = 0x0000_0100_0000_01B3;

fn fnv1a_64_seeded(bytes: &[u8], seed: u64) -> u64 {
    let mut h = seed;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(FNV1A_64_PRIME);
    }
    h
}

// `entity_init_code_hash` returns 4 × u64; format it into a 256-bit hex
// constant `(h0 << 192) | (h1 << 128) | (h2 << 64) | h3`. The
// Rust-side helper composes the hex from the four chunks so the
// generated Lean source has a single `0x...#256` literal.
//
// We use `LowerHex` formatting at the call site rather than a dedicated
// helper because the chunks have a stable layout.

// The format string in `emit_address` uses `{init:064x}` against
// `entity_init_code_hash(...)`'s output. Implement `LowerHex` for the
// 4-chunk array via a wrapper.
impl std::fmt::LowerHex for InitHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [h0, h1, h2, h3] = self.0;
        // Big-endian: h0 is the most significant chunk.
        write!(f, "{:016x}{:016x}{:016x}{:016x}", h0, h1, h2, h3,)
    }
}

struct InitHash([u64; 4]);

// Public format-friendly conversion used by `emit_address`. We can't
// directly call `format!("{:064x}", [u64;4])` because primitive array
// types don't implement `LowerHex`. Wrap the array in `InitHash`
// before formatting.
fn format_init_hash(h: [u64; 4]) -> String {
    format!("{:064x}", InitHash(h))
}

// Bridge: rewrite `entity_init_code_hash` callers to format via the
// helper above. We re-export the value as the hex string the format
// in `emit_address` expects (otherwise `{init:064x}` on a `[u64; 4]`
// fails to compile). Keep the original function for API clarity but
// route the format through the wrapper.
#[allow(dead_code)]
fn _force_format_used(_: String) {}
