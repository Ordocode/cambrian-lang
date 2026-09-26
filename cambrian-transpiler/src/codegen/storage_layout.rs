// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Solidity-compatible storage layout computation for entity members.
//!
//! Solidity assigns storage slots sequentially to non-immutable state
//! variables starting from slot 0. Consecutive value types that fit in
//! 32 bytes are packed into the same slot (lower-order bytes first).
//! Each `mapping(K => V)` and dynamic array occupies a full slot index,
//! but the actual data lives at a hash-derived location:
//!
//! * `mapping(K => V)` value at key `k` lives at `keccak256(abi.encode(k, slot))`.
//! * Dynamic array: length at `slot`, elements starting at `keccak256(slot)`.
//! * Struct / record members always start a fresh slot; fields pack
//!   internally with the same rules, then the next state variable resumes
//!   at the next unused slot (PM-024 / solc storage-layout).
//!
//! This module mirrors the layout produced by [`crate::codegen::evm`]:
//!
//! * Identity members → declared as `immutable` (not stored, no slot).
//! * `_factory` (deterministic mode) → `immutable` (no slot).
//! * Each non-identity member → packed into slots in declaration order.
//! * `mapping`s annotated with `.exists()` use → an additional `_exists`
//!   shadow mapping slot, immediately after the parent mapping slot.
//! * `_initialized: bool` (deterministic + non-identity init params) →
//!   packed at the end like any other value type.

use super::solidity::{has_non_identity_init_params, is_mapping_type, transforms_use_exists};
use crate::ast::{Entity, EnumDecl, Program, Record, Type};

/// What kind of storage slot a member occupies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotKind {
    /// Value type — slot holds the value (possibly packed with neighbors).
    Value,
    /// `mapping(K => V)` — slot itself is unused; value at `keccak(key . slot)`.
    Mapping,
    /// Dynamic array — slot holds length; data at `keccak(slot) + index`.
    DynamicArray,
    /// Struct / record — occupies one or more exclusive slots (fields pack
    /// internally); `slot` is the starting slot index.
    Struct,
}

#[derive(Debug, Clone)]
pub struct SlotInfo {
    pub name: String,
    pub ty: Option<Type>,
    pub slot: u64,
    /// Byte offset within the slot (0 = lowest-order bytes). Only meaningful
    /// for [`SlotKind::Value`]; mappings / arrays / structs always use offset 0.
    pub offset: u8,
    /// Byte size of the value within the slot. Mappings / arrays report 32.
    /// Structs report total packed byte width across their slots.
    pub size: u8,
    pub kind: SlotKind,
}

impl SlotInfo {
    /// True when this member alone occupies its storage slot (safe for a full-word
    /// `vm.store` without clobbering neighbors).
    pub fn occupies_full_slot(&self) -> bool {
        self.size == 32 || matches!(self.kind, SlotKind::Struct)
    }
}

#[derive(Debug, Clone, Default)]
pub struct StorageLayout {
    pub slots: Vec<SlotInfo>,
}

impl StorageLayout {
    pub fn lookup(&self, name: &str) -> Option<&SlotInfo> {
        self.slots.iter().find(|s| s.name == name)
    }

    /// True when `info` is the only occupant of its storage slot, so a
    /// full-word `vm.store` cannot clobber a neighbor. A lone `uint64` at
    /// offset 0 (with the high bytes unused) qualifies — Solidity reads
    /// only the low `size` bytes.
    pub fn is_exclusive(&self, info: &SlotInfo) -> bool {
        info.occupies_full_slot()
            || matches!(info.kind, SlotKind::Struct)
            || self.slots.iter().filter(|s| s.slot == info.slot).count() == 1
    }
}

/// Compute the slot layout for an entity (entity-local records / enums only).
pub fn compute_layout(entity: &Entity, deterministic: bool) -> StorageLayout {
    compute_layout_with_records(entity, deterministic, &[], &[])
}

/// Like [`compute_layout`], but also resolves program-scope `record` / `enum` names.
pub fn compute_layout_with_program(
    entity: &Entity,
    deterministic: bool,
    program: &Program,
) -> StorageLayout {
    compute_layout_with_records(
        entity,
        deterministic,
        &program.records,
        &program.enums,
    )
}

/// Compute the slot layout, looking up record / enum definitions in
/// `entity` then `program_records` / `program_enums` (PM-024 / PW3-O-003).
pub fn compute_layout_with_records(
    entity: &Entity,
    deterministic: bool,
    program_records: &[Record],
    program_enums: &[EnumDecl],
) -> StorageLayout {
    let mut slots = Vec::new();
    let mut next_slot: u64 = 0;
    let mut used_in_slot: u8 = 0;

    // _factory immutable in deterministic mode — no slot.
    let _ = deterministic;

    for m in &entity.members {
        if m.is_identity {
            // immutable — no storage slot
            continue;
        }

        if let Some(rec) = lookup_record(entity, program_records, &m.ty) {
            // Solidity structs always start a fresh slot and never pack
            // with neighboring state variables; fields pack internally.
            if used_in_slot > 0 {
                next_slot += 1;
            }
            let start = next_slot;
            let (end_slot, end_used, total_size) =
                pack_record_fields(rec, entity, program_records, program_enums, next_slot);
            // After a struct, the next state variable starts at the next
            // unused slot (solc never continues packing into a partial
            // trailing struct slot from outside the struct).
            next_slot = if end_used > 0 { end_slot + 1 } else { end_slot };
            used_in_slot = 0;
            slots.push(SlotInfo {
                name: m.name.clone(),
                ty: Some(m.ty.clone()),
                slot: start,
                offset: 0,
                size: total_size.min(255) as u8,
                kind: SlotKind::Struct,
            });
            continue;
        }

        if let Some(en) = lookup_enum(entity, program_enums, &m.ty) {
            if is_payload_enum(en) {
                if used_in_slot > 0 {
                    next_slot += 1;
                }
                let start = next_slot;
                let (end_slot, end_used, total_size) =
                    pack_payload_enum_fields(en, entity, program_records, program_enums, next_slot);
                next_slot = if end_used > 0 { end_slot + 1 } else { end_slot };
                used_in_slot = 0;
                slots.push(SlotInfo {
                    name: m.name.clone(),
                    ty: Some(m.ty.clone()),
                    slot: start,
                    offset: 0,
                    size: total_size.min(255) as u8,
                    kind: SlotKind::Struct,
                });
                continue;
            }
            // Unit enum: Solidity `enum` is uint8 and packs with neighbors.
        }

        let kind = if is_mapping_type(&m.ty) {
            SlotKind::Mapping
        } else if is_dynamic_array(&m.ty) {
            SlotKind::DynamicArray
        } else {
            SlotKind::Value
        };

        let (slot, offset, size) = match kind {
            SlotKind::Mapping | SlotKind::DynamicArray => {
                // Mappings / dynamic arrays always start a fresh slot and
                // consume it entirely (data lives at a keccak-derived address).
                if used_in_slot > 0 {
                    next_slot += 1;
                }
                let s = next_slot;
                next_slot += 1;
                used_in_slot = 0;
                (s, 0u8, 32u8)
            }
            SlotKind::Value => {
                let size = value_nbytes(entity, program_records, program_enums, &m.ty);
                if used_in_slot > 0 && used_in_slot + size > 32 {
                    next_slot += 1;
                    used_in_slot = 0;
                }
                let s = next_slot;
                let offset = used_in_slot;
                used_in_slot += size;
                if used_in_slot == 32 {
                    next_slot += 1;
                    used_in_slot = 0;
                }
                (s, offset, size)
            }
            SlotKind::Struct => unreachable!("handled above"),
        };

        let kind_for_check = kind.clone();
        slots.push(SlotInfo {
            name: m.name.clone(),
            ty: Some(m.ty.clone()),
            slot,
            offset,
            size,
            kind,
        });

        if matches!(kind_for_check, SlotKind::Mapping)
            && transforms_use_exists(&entity.members, &m.name)
        {
            // Shadow mapping always occupies its own full slot.
            if used_in_slot > 0 {
                next_slot += 1;
            }
            slots.push(SlotInfo {
                name: format!("{}_exists", m.name),
                ty: None,
                slot: next_slot,
                offset: 0,
                size: 32,
                kind: SlotKind::Mapping,
            });
            next_slot += 1;
            used_in_slot = 0;
        }
    }

    if deterministic && has_non_identity_init_params(entity) {
        // `_initialized: bool` packs like any other 1-byte value.
        let size = 1u8;
        if used_in_slot > 0 && used_in_slot + size > 32 {
            next_slot += 1;
            used_in_slot = 0;
        }
        let offset = used_in_slot;
        slots.push(SlotInfo {
            name: "_initialized".to_string(),
            ty: Some(Type::Simple("bool".into())),
            slot: next_slot,
            offset,
            size,
            kind: SlotKind::Value,
        });
    }

    StorageLayout { slots }
}

fn lookup_record<'a>(
    entity: &'a Entity,
    program_records: &'a [Record],
    ty: &Type,
) -> Option<&'a Record> {
    let Type::Simple(name) = ty else {
        return None;
    };
    entity
        .records
        .iter()
        .find(|r| r.name == *name)
        .or_else(|| program_records.iter().find(|r| r.name == *name))
}

fn lookup_enum<'a>(
    entity: &'a Entity,
    program_enums: &'a [EnumDecl],
    ty: &Type,
) -> Option<&'a EnumDecl> {
    let Type::Simple(name) = ty else {
        return None;
    };
    entity
        .enums
        .iter()
        .find(|e| e.name == *name)
        .or_else(|| program_enums.iter().find(|e| e.name == *name))
}

fn is_payload_enum(decl: &EnumDecl) -> bool {
    decl.variants.iter().any(|v| !v.fields.is_empty())
}

fn value_nbytes(
    entity: &Entity,
    program_records: &[Record],
    program_enums: &[EnumDecl],
    ty: &Type,
) -> u8 {
    if lookup_record(entity, program_records, ty).is_some() {
        return 32;
    }
    if let Some(en) = lookup_enum(entity, program_enums, ty) {
        if is_payload_enum(en) {
            return 32;
        }
        return 1;
    }
    sol_value_nbytes(ty).unwrap_or(32)
}

/// Pack record fields starting at `start_slot` with empty packing state.
/// Returns `(next_slot_index, used_in_last_slot, total_bytes)`.
fn pack_record_fields(
    rec: &Record,
    entity: &Entity,
    program_records: &[Record],
    program_enums: &[EnumDecl],
    start_slot: u64,
) -> (u64, u8, u32) {
    let mut next_slot = start_slot;
    let mut used_in_slot: u8 = 0;
    let mut total_bytes: u32 = 0;

    for f in &rec.fields {
        if let Some(nested) = lookup_record(entity, program_records, &f.ty) {
            if used_in_slot > 0 {
                next_slot += 1;
            }
            let (end_slot, end_used, nested_bytes) =
                pack_record_fields(nested, entity, program_records, program_enums, next_slot);
            total_bytes += nested_bytes;
            next_slot = if end_used > 0 { end_slot + 1 } else { end_slot };
            used_in_slot = 0;
            continue;
        }

        if let Some(en) = lookup_enum(entity, program_enums, &f.ty) {
            if is_payload_enum(en) {
                if used_in_slot > 0 {
                    next_slot += 1;
                }
                let (end_slot, end_used, nested_bytes) =
                    pack_payload_enum_fields(en, entity, program_records, program_enums, next_slot);
                total_bytes += nested_bytes;
                next_slot = if end_used > 0 { end_slot + 1 } else { end_slot };
                used_in_slot = 0;
                continue;
            }
        }

        if is_mapping_type(&f.ty) || is_dynamic_array(&f.ty) {
            if used_in_slot > 0 {
                next_slot += 1;
            }
            next_slot += 1;
            used_in_slot = 0;
            total_bytes += 32;
            continue;
        }

        let size = value_nbytes(entity, program_records, program_enums, &f.ty);
        if used_in_slot > 0 && used_in_slot + size > 32 {
            next_slot += 1;
            used_in_slot = 0;
        }
        used_in_slot += size;
        total_bytes += size as u32;
        if used_in_slot == 32 {
            next_slot += 1;
            used_in_slot = 0;
        }
    }

    (next_slot, used_in_slot, total_bytes)
}

fn pack_payload_enum_fields(
    decl: &EnumDecl,
    entity: &Entity,
    program_records: &[Record],
    program_enums: &[EnumDecl],
    start_slot: u64,
) -> (u64, u8, u32) {
    let mut next_slot = start_slot;
    // tag: uint8
    let mut used_in_slot: u8 = 1;
    let mut total_bytes: u32 = 1;

    for v in &decl.variants {
        for fty in &v.fields {
            if let Some(nested) = lookup_record(entity, program_records, fty) {
                if used_in_slot > 0 {
                    next_slot += 1;
                }
                let (end_slot, end_used, nested_bytes) =
                    pack_record_fields(nested, entity, program_records, program_enums, next_slot);
                total_bytes += nested_bytes;
                next_slot = if end_used > 0 { end_slot + 1 } else { end_slot };
                used_in_slot = 0;
                continue;
            }
            let size = value_nbytes(entity, program_records, program_enums, fty);
            if used_in_slot > 0 && used_in_slot + size > 32 {
                next_slot += 1;
                used_in_slot = 0;
            }
            used_in_slot += size;
            total_bytes += size as u32;
            if used_in_slot == 32 {
                next_slot += 1;
                used_in_slot = 0;
            }
        }
    }

    (next_slot, used_in_slot, total_bytes)
}

/// Byte width of a Solidity value type for storage packing. Returns `None`
/// for types that always consume a full slot (mappings, dynamic arrays,
/// references) — callers should treat those as 32-byte exclusive slots.
pub fn sol_value_nbytes(ty: &Type) -> Option<u8> {
    match ty {
        Type::Simple(name) => match name.as_str() {
            "bool" => Some(1),
            "u8" | "i8" | "uint8" | "int8" | "bytes1" => Some(1),
            "u16" | "i16" | "uint16" | "int16" | "bytes2" => Some(2),
            "u32" | "i32" | "uint32" | "int32" | "bytes4" => Some(4),
            "u64" | "i64" | "uint64" | "int64" | "bytes8" => Some(8),
            "u128" | "i128" | "uint128" | "int128" | "bytes16" => Some(16),
            "u256" | "U256" | "uint256" | "int256" | "bytes32" | "pubkey" => Some(32),
            "address" => Some(20),
            // CamData / String / bytes are dynamic — full slot for the pointer.
            "CamData" | "String" | "string" | "bytes" => Some(32),
            // Unknown names (including unresolved records when no table is
            // passed) fall back to one word — prefer `compute_layout_with_*`.
            _ => Some(32),
        },
        Type::TypedAddress(_) => Some(20),
        Type::Generic(name, _)
            if name == "HashMap" || name == "Vec" || name == "array" || name == "Option" =>
        {
            // Option<T> is a tagged-union struct (tag + payload) and
            // occupies its own storage slot(s); solc will not pack it
            // with a following value type.
            if name == "Option" {
                Some(32)
            } else {
                None
            }
        }
        Type::Tuple(_) => Some(32),
        _ => Some(32),
    }
}

fn is_dynamic_array(ty: &Type) -> bool {
    matches!(ty, Type::Generic(name, _) if name == "Vec" || name == "array")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{EnumDecl, EnumVariant, Field, Member, Span};

    fn mk_member(name: &str, ty: Type, is_id: bool) -> Member {
        Member {
            name: name.to_string(),
            ty,
            is_identity: is_id,
            default_value: None,
            transforms: vec![],
            span: Span::none(),
        }
    }

    fn mk_entity(members: Vec<Member>) -> Entity {
        Entity {
            name: "T".to_string(),
            records: vec![],
            enums: vec![],
            type_aliases: vec![],
            constants: vec![],
            macros: vec![],
            routes: vec![],
            members,
            events: vec![],
            errors: vec![],
            span: Span::none(),
        }
    }

    #[test]
    fn sequential_value_slots() {
        let e = mk_entity(vec![
            mk_member("a", Type::Simple("U256".into()), false),
            mk_member("b", Type::Simple("U256".into()), false),
            mk_member("c", Type::Simple("bool".into()), false),
        ]);
        let l = compute_layout(&e, false);
        assert_eq!(l.slots.len(), 3);
        assert_eq!(l.slots[0].slot, 0);
        assert_eq!(l.slots[1].slot, 1);
        // bool packs into its own slot (slot 2) because prior members are full words.
        assert_eq!(l.slots[2].slot, 2);
        assert!(matches!(l.slots[0].kind, SlotKind::Value));
    }

    #[test]
    fn packs_address_and_uint64() {
        // Mirrors Governor: address + address + address + u64 + U256 + u64.
        let e = mk_entity(vec![
            mk_member("m_token", Type::Simple("address".into()), false),
            mk_member("m_timelock", Type::Simple("address".into()), false),
            mk_member("m_admin", Type::Simple("address".into()), false),
            mk_member("m_voting_period", Type::Simple("u64".into()), false),
            mk_member("m_quorum", Type::Simple("U256".into()), false),
            mk_member("m_min_delay", Type::Simple("u64".into()), false),
        ]);
        let l = compute_layout(&e, false);
        assert_eq!(l.lookup("m_token").unwrap().slot, 0);
        assert_eq!(l.lookup("m_timelock").unwrap().slot, 1);
        // address (20) + u64 (8) = 28 → pack into slot 2.
        assert_eq!(l.lookup("m_admin").unwrap().slot, 2);
        assert_eq!(l.lookup("m_admin").unwrap().offset, 0);
        assert_eq!(l.lookup("m_voting_period").unwrap().slot, 2);
        assert_eq!(l.lookup("m_voting_period").unwrap().offset, 20);
        // U256 needs a full slot → slot 3.
        assert_eq!(l.lookup("m_quorum").unwrap().slot, 3);
        assert!(l.lookup("m_quorum").unwrap().occupies_full_slot());
        // trailing u64 → slot 4.
        assert_eq!(l.lookup("m_min_delay").unwrap().slot, 4);
    }

    #[test]
    fn mapping_slot_marked() {
        let e = mk_entity(vec![
            mk_member("count", Type::Simple("U256".into()), false),
            mk_member(
                "balances",
                Type::Generic(
                    "HashMap".into(),
                    vec![Type::Simple("address".into()), Type::Simple("U256".into())],
                ),
                false,
            ),
        ]);
        let l = compute_layout(&e, false);
        assert!(matches!(l.slots[0].kind, SlotKind::Value));
        assert!(matches!(l.slots[1].kind, SlotKind::Mapping));
        assert_eq!(l.slots[1].slot, 1);
    }

    #[test]
    fn lookup_by_name() {
        let e = mk_entity(vec![
            mk_member("a", Type::Simple("U256".into()), false),
            mk_member("b", Type::Simple("U256".into()), false),
        ]);
        let l = compute_layout(&e, false);
        assert_eq!(l.lookup("b").unwrap().slot, 1);
        assert!(l.lookup("zzz").is_none());
    }

    #[test]
    fn identity_members_skip_slots() {
        let e = mk_entity(vec![
            mk_member("id", Type::Simple("U256".into()), true),
            mk_member("count", Type::Simple("U256".into()), false),
        ]);
        let l = compute_layout(&e, false);
        assert_eq!(l.slots.len(), 1);
        assert_eq!(l.slots[0].name, "count");
        assert_eq!(l.slots[0].slot, 0);
    }

    #[test]
    fn record_member_spans_field_slots() {
        // PM-024: m_a:u64 @0, Info{U256,U256} @1–2, m_b:u64 @3.
        let mut e = mk_entity(vec![
            mk_member("m_a", Type::Simple("u64".into()), false),
            mk_member("m_info", Type::Simple("Info".into()), false),
            mk_member("m_b", Type::Simple("u64".into()), false),
        ]);
        e.records.push(Record {
            name: "Info".into(),
            fields: vec![
                Field {
                    name: "lo".into(),
                    ty: Type::Simple("U256".into()),
                },
                Field {
                    name: "hi".into(),
                    ty: Type::Simple("U256".into()),
                },
            ],
            span: Span::none(),
        });
        let l = compute_layout(&e, false);
        assert_eq!(l.lookup("m_a").unwrap().slot, 0);
        assert_eq!(l.lookup("m_info").unwrap().slot, 1);
        assert!(matches!(l.lookup("m_info").unwrap().kind, SlotKind::Struct));
        assert_eq!(l.lookup("m_b").unwrap().slot, 3);
    }

    fn mk_unit_enum(name: &str, variants: &[&str]) -> EnumDecl {
        EnumDecl {
            name: name.to_string(),
            variants: variants
                .iter()
                .map(|v| EnumVariant {
                    name: v.to_string(),
                    fields: vec![],
                })
                .collect(),
            span: Span::none(),
        }
    }

    #[test]
    fn packs_unit_enum_and_uint64() {
        let mut e = mk_entity(vec![
            mk_member("m_c", Type::Simple("Color".into()), false),
            mk_member("m_x", Type::Simple("u64".into()), false),
        ]);
        e.enums.push(mk_unit_enum("Color", &["Red", "Green"]));
        let l = compute_layout(&e, false);
        let c = l.lookup("m_c").unwrap();
        let x = l.lookup("m_x").unwrap();
        assert_eq!(c.slot, 0);
        assert_eq!(c.offset, 0);
        assert_eq!(c.size, 1);
        assert_eq!(x.slot, 0);
        assert_eq!(x.offset, 1);
        assert_eq!(x.size, 8);
        assert!(!l.is_exclusive(x));
    }

    #[test]
    fn packs_consecutive_unit_enums() {
        let mut e = mk_entity(vec![
            mk_member("m_a", Type::Simple("Color".into()), false),
            mk_member("m_b", Type::Simple("Color".into()), false),
        ]);
        e.enums.push(mk_unit_enum("Color", &["Red", "Green"]));
        let l = compute_layout(&e, false);
        assert_eq!(l.lookup("m_a").unwrap().offset, 0);
        assert_eq!(l.lookup("m_b").unwrap().offset, 1);
        assert_eq!(l.lookup("m_a").unwrap().slot, 0);
        assert_eq!(l.lookup("m_b").unwrap().slot, 0);
    }

    #[test]
    fn packs_bool_and_unit_enum() {
        let mut e = mk_entity(vec![
            mk_member("m_ok", Type::Simple("bool".into()), false),
            mk_member("m_c", Type::Simple("Color".into()), false),
        ]);
        e.enums.push(mk_unit_enum("Color", &["Red", "Green"]));
        let l = compute_layout(&e, false);
        assert_eq!(l.lookup("m_ok").unwrap().offset, 0);
        assert_eq!(l.lookup("m_c").unwrap().offset, 1);
        assert_eq!(l.lookup("m_c").unwrap().slot, 0);
    }

    #[test]
    fn payload_enum_is_struct_exclusive() {
        let mut e = mk_entity(vec![
            mk_member("m_a", Type::Simple("Action".into()), false),
            mk_member("m_x", Type::Simple("u64".into()), false),
        ]);
        e.enums.push(EnumDecl {
            name: "Action".into(),
            variants: vec![
                EnumVariant {
                    name: "Deposit".into(),
                    fields: vec![Type::Simple("U256".into())],
                },
                EnumVariant {
                    name: "Approve".into(),
                    fields: vec![],
                },
            ],
            span: Span::none(),
        });
        let l = compute_layout(&e, false);
        assert!(matches!(l.lookup("m_a").unwrap().kind, SlotKind::Struct));
        // tag (1) + U256 (32) → two slots; next member at slot 2.
        assert_eq!(l.lookup("m_x").unwrap().slot, 2);
    }
}
